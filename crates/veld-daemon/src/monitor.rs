use crate::broadcaster::Broadcaster;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use veld_core::config::{self, LivenessProbe, VeldConfig};
use veld_core::logging::{self, LogWriter};
use veld_core::state::{GlobalRegistry, NodeStatus, ProjectState, RunStatus};

/// Interval between health-check scans (seconds).
const SCAN_INTERVAL_SECS: u64 = 5;

/// Tracks when each node's liveness probe was last executed.
/// Key: `"project_root:run_name:node:variant"`.
type LastCheckMap = HashMap<String, Instant>;

/// Resolve the user's full PATH by spawning an interactive login shell.
/// Falls back to the current process PATH if resolution fails.
fn resolve_user_path() -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    // Use -l -i -c to get a fully initialized interactive login shell.
    // This captures PATH after .zprofile/.bash_profile/brew shellenv etc.
    let output = std::process::Command::new(&shell)
        .arg("-l")
        .arg("-i")
        .arg("-c")
        .arg("echo $PATH")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if !path.is_empty() {
                info!(path = %path, "resolved user PATH from login shell");
                return path;
            }
        }
        Ok(o) => {
            debug!(
                exit_code = o.status.code(),
                "login shell PATH resolution exited non-zero, using fallback"
            );
        }
        Err(e) => {
            debug!(error = %e, "failed to resolve user PATH, using fallback");
        }
    }

    std::env::var("PATH").unwrap_or_default()
}

/// Periodically scan all runs from the global registry and check process health.
/// When a status change is detected, update the registry and broadcast the event.
pub async fn run_health_monitor(broadcaster: Broadcaster) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(SCAN_INTERVAL_SECS));
    let mut last_checks: LastCheckMap = HashMap::new();

    // Resolve the user's full PATH once at startup so probe commands can
    // find tools like pg_isready even when the daemon starts at boot.
    let mut user_path = resolve_user_path();
    let mut path_resolved_at = Instant::now();

    loop {
        interval.tick().await;
        debug!("running health-check scan");

        // Re-resolve PATH every 60s to pick up changes after user login.
        if path_resolved_at.elapsed() > Duration::from_secs(60) {
            user_path = resolve_user_path();
            path_resolved_at = Instant::now();
        }

        match scan_and_update(&broadcaster, &mut last_checks, &user_path).await {
            Ok(changes) => {
                if changes > 0 {
                    info!("health scan detected {changes} status change(s)");
                }
            }
            Err(e) => {
                warn!("health scan error: {e}");
            }
        }
    }
}

/// Scan the global registry, check each running process, and return the number
/// of status changes applied.
async fn scan_and_update(
    broadcaster: &Broadcaster,
    last_checks: &mut LastCheckMap,
    user_path: &str,
) -> anyhow::Result<usize> {
    let registry = GlobalRegistry::load()?;

    let mut changes = 0;

    for reg_entry in registry.projects.values() {
        let project_root = &reg_entry.project_root;

        for (run_name, run_info) in &reg_entry.runs {
            if run_info.status != RunStatus::Running {
                continue;
            }

            // Load the actual project state to get full RunState with node PIDs.
            let project_state = match ProjectState::load(project_root) {
                Ok(ps) => ps,
                Err(e) => {
                    debug!(
                        "could not load project state for {}: {e}",
                        project_root.display()
                    );
                    continue;
                }
            };

            let run_state = match project_state.get_run(run_name) {
                Some(rs) => rs,
                None => continue,
            };

            // Check if any node with a PID has died.
            let mut any_dead = false;
            for node_state in run_state.nodes.values() {
                if let Some(pid) = node_state.pid {
                    if !is_process_alive(pid) {
                        any_dead = true;
                        info!(
                            "process {pid} (node {}:{}) is no longer alive",
                            node_state.node_name, node_state.variant
                        );
                    }
                }
            }

            if any_dead {
                // Update the project state: mark the run as stopped.
                let mut project_state = match ProjectState::load(project_root) {
                    Ok(ps) => ps,
                    Err(_) => continue,
                };

                if let Some(run) = project_state.get_run_mut(run_name) {
                    run.status = RunStatus::Stopped;
                    run.stopped_at = Some(chrono::Utc::now());

                    // Mark dead nodes as stopped.
                    for node in run.nodes.values_mut() {
                        if let Some(pid) = node.pid {
                            if !is_process_alive(pid) {
                                node.status = veld_core::state::NodeStatus::Stopped;
                            }
                        }
                    }
                }

                let _ = project_state.save(project_root);

                // Update the global registry.
                let mut registry = GlobalRegistry::load().unwrap_or_default();
                if let Some(entry) = registry
                    .projects
                    .get_mut(&project_root.to_string_lossy().into_owned())
                {
                    if let Some(info) = entry.runs.get_mut(run_name) {
                        info.status = RunStatus::Stopped;
                    }
                }
                let _ = registry.save();

                // Broadcast the change.
                let event = serde_json::json!({
                    "event": "status_change",
                    "run": run_name,
                    "project": project_root.to_string_lossy(),
                    "old_status": "running",
                    "new_status": "stopped",
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                broadcaster.broadcast(&event).await;

                changes += 1;
                continue; // Skip liveness checks for a run that just stopped.
            }

            // --- Liveness probe checks ---
            // Load the project config to access probe definitions.
            let config = match load_config_for_project(project_root) {
                Some(c) => c,
                None => continue,
            };

            // Create internal log writer for this run.
            let log_path = logging::internal_log_file(project_root, run_name);
            let internal_log = LogWriter::new(log_path).await.ok();

            changes += run_liveness_checks(
                project_root,
                run_name,
                &config,
                broadcaster,
                last_checks,
                internal_log.as_ref(),
                user_path,
            )
            .await;
        }
    }

    Ok(changes)
}

/// Run liveness probes for all healthy nodes in a run. Returns number of state changes.
async fn run_liveness_checks(
    project_root: &Path,
    run_name: &str,
    config: &VeldConfig,
    broadcaster: &Broadcaster,
    last_checks: &mut LastCheckMap,
    internal_log: Option<&LogWriter>,
    user_path: &str,
) -> usize {
    // Reload state fresh for liveness checks.
    let mut project_state = match ProjectState::load(project_root) {
        Ok(ps) => ps,
        Err(_) => return 0,
    };

    let run = match project_state.get_run_mut(run_name) {
        Some(r) => r,
        None => return 0,
    };

    let mut changes = 0;

    // Collect nodes to check — both Healthy and Unhealthy nodes get probed.
    // Unhealthy nodes can recover if probes start passing again.
    let nodes_to_check: Vec<(String, String, String)> = run
        .nodes
        .iter()
        .filter(|(_, ns)| ns.status == NodeStatus::Healthy || ns.status == NodeStatus::Unhealthy)
        .map(|(key, ns)| (key.clone(), ns.node_name.clone(), ns.variant.clone()))
        .collect();

    for (key, node_name, variant_name) in &nodes_to_check {
        let node_cfg = match config.nodes.get(node_name) {
            Some(c) => c,
            None => continue,
        };
        let variant_cfg = match node_cfg.variants.get(variant_name) {
            Some(c) => c,
            None => continue,
        };

        let liveness = match variant_cfg.liveness_probe() {
            Some(lp) => lp,
            None => continue,
        };

        // Respect per-probe interval_ms — skip if not enough time has elapsed.
        let check_key = format!("{}:{}:{}", project_root.to_string_lossy(), run_name, key);
        let probe_interval = Duration::from_millis(liveness.interval_ms);
        if let Some(last) = last_checks.get(&check_key) {
            if last.elapsed() < probe_interval {
                continue;
            }
        }
        last_checks.insert(check_key, Instant::now());

        // Run a single liveness check attempt.
        let working_dir = config::resolve_cwd(
            project_root,
            node_cfg.cwd.as_deref(),
            variant_cfg.cwd.as_deref(),
        );

        let node_label = format!("{node_name}:{variant_name}");

        if let Some(log) = internal_log {
            let _ = log
                .write_line(&format!(
                    "[liveness] {node_label} — running probe (type: {})",
                    liveness.check_type
                ))
                .await;
        }

        let check_result =
            run_single_liveness_check(liveness, &working_dir, run, key, user_path).await;

        let node_state = match run.nodes.get_mut(key) {
            Some(ns) => ns,
            None => continue,
        };

        match check_result {
            Ok(()) => {
                if let Some(log) = internal_log {
                    let _ = log
                        .write_line(&format!("[liveness] {node_label} — probe passed"))
                        .await;
                }
                // Reset failure counter on success.
                if node_state.consecutive_failures > 0 || node_state.status == NodeStatus::Unhealthy
                {
                    node_state.consecutive_failures = 0;
                    node_state.last_liveness_error = None;
                    // Transition Unhealthy -> Healthy (probe started passing again).
                    if node_state.status == NodeStatus::Unhealthy {
                        node_state.status = NodeStatus::Healthy;
                        info!(
                            node = node_name.as_str(),
                            variant = variant_name.as_str(),
                            "node self-healed — transitioning from unhealthy to healthy"
                        );
                        if let Some(log) = internal_log {
                            let _ = log
                                .write_line(&format!(
                                    "[liveness] {node_label} — self-healed, back to healthy"
                                ))
                                .await;
                        }
                    }
                    changes += 1;
                }
            }
            Err(error_detail) => {
                node_state.consecutive_failures += 1;
                node_state.last_liveness_error = Some(error_detail.clone());
                changes += 1;

                info!(
                    node = node_name.as_str(),
                    variant = variant_name.as_str(),
                    consecutive_failures = node_state.consecutive_failures,
                    threshold = liveness.failure_threshold,
                    "liveness probe failed"
                );

                if let Some(log) = internal_log {
                    let _ = log
                    .write_line(&format!(
                        "[liveness] {node_label} — probe failed ({}/{} consecutive): {error_detail}",
                        node_state.consecutive_failures, liveness.failure_threshold
                    ))
                    .await;
                }

                // Check if failure threshold is reached.
                if node_state.consecutive_failures >= liveness.failure_threshold {
                    if node_state.recovery_count >= liveness.max_recoveries {
                        // Exhausted — permanently fail.
                        node_state.status = NodeStatus::Failed;
                        warn!(
                            node = node_name.as_str(),
                            variant = variant_name.as_str(),
                            max_recoveries = liveness.max_recoveries,
                            "recovery exhausted — node permanently failed"
                        );

                        if let Some(log) = internal_log {
                            let _ = log
                                .write_line(&format!(
                                    "[recovery] {node_label} — permanently failed after {} recovery attempts",
                                    liveness.max_recoveries
                                ))
                                .await;
                        }

                        let event = serde_json::json!({
                            "event": "recovery_exhausted",
                            "run": run_name,
                            "project": project_root.to_string_lossy(),
                            "node": node_name,
                            "variant": variant_name,
                            "max_recoveries": liveness.max_recoveries,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        });
                        broadcaster.broadcast(&event).await;
                    } else {
                        // Trigger restart.
                        let new_recovery_count = node_state.recovery_count + 1;

                        info!(
                            node = node_name.as_str(),
                            variant = variant_name.as_str(),
                            attempt = new_recovery_count,
                            max = liveness.max_recoveries,
                            "triggering recovery restart"
                        );

                        if let Some(log) = internal_log {
                            let _ = log
                                .write_line(&format!(
                                    "[recovery] {node_label} — restarting environment (attempt {new_recovery_count}/{})",
                                    liveness.max_recoveries
                                ))
                                .await;
                        }

                        let event = serde_json::json!({
                            "event": "recovery_starting",
                            "run": run_name,
                            "project": project_root.to_string_lossy(),
                            "node": node_name,
                            "variant": variant_name,
                            "attempt": new_recovery_count,
                            "max_recoveries": liveness.max_recoveries,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        });
                        broadcaster.broadcast(&event).await;

                        // Save state BEFORE restart so recovery_count is persisted.
                        // Don't set status to Unhealthy — the restart will create
                        // fresh Healthy state. We only need recovery_count to survive.
                        node_state.recovery_count = new_recovery_count;
                        node_state.consecutive_failures = 0;
                        let _ = project_state.save(project_root);

                        // Run the restart. This stops+starts the entire environment,
                        // creating fresh node state with recovery_count: 0.
                        run_veld_restart(project_root, run_name, internal_log, user_path).await;

                        // Restore recovery_count on the fresh state so it accumulates
                        // across restarts and eventually hits max_recoveries.
                        if let Ok(mut fresh_state) = ProjectState::load(project_root) {
                            if let Some(fresh_run) = fresh_state.get_run_mut(run_name) {
                                if let Some(fresh_node) = fresh_run.nodes.get_mut(key) {
                                    fresh_node.recovery_count = new_recovery_count;
                                }
                            }
                            let _ = fresh_state.save(project_root);
                        }

                        // Return early — don't save stale in-memory state over
                        // the fresh state created by the restart.
                        return changes;
                    }
                }
            }
        }
    }

    // Persist any state changes (failure counts, etc.).
    if changes > 0 {
        let _ = project_state.save(project_root);
    }

    changes
}

/// Run a single liveness check for a node.
/// Returns `Ok(())` if healthy, `Err(reason)` with details if unhealthy.
async fn run_single_liveness_check(
    liveness: &LivenessProbe,
    working_dir: &Path,
    run: &veld_core::state::RunState,
    node_key: &str,
    user_path: &str,
) -> Result<(), String> {
    let node_state = match run.nodes.get(node_key) {
        Some(ns) => ns,
        None => return Ok(()),
    };

    match liveness.check_type.as_str() {
        "command" | "bash" => {
            if let Some(ref cmd) = liveness.command {
                // Timeout command checks to prevent hanging the monitor loop.
                // Inject the resolved user PATH so probes find tools like
                // pg_isready even when the daemon starts at boot.
                let result = tokio::time::timeout(Duration::from_secs(30), async {
                    let mut command = tokio::process::Command::new("sh");
                    command
                        .arg("-c")
                        .arg(cmd)
                        .current_dir(working_dir)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped())
                        .env("PATH", user_path);
                    // Inject node outputs as environment variables so probe
                    // commands can reference them (e.g., pg_isready -h $DB_HOST).
                    for (key, value) in &node_state.outputs {
                        command.env(key, value);
                    }
                    command.output().await
                })
                .await;

                match result {
                    Ok(Ok(output)) if output.status.success() => Ok(()),
                    Ok(Ok(output)) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stderr = stderr.trim();
                        let code = output.status.code().unwrap_or(-1);
                        if stderr.is_empty() {
                            Err(format!("exit code {code}"))
                        } else {
                            Err(format!("exit code {code}: {stderr}"))
                        }
                    }
                    Ok(Err(e)) => Err(format!("exec error: {e}")),
                    Err(_) => Err("command timed out (30s)".to_owned()),
                }
            } else {
                Ok(()) // No command configured, consider healthy.
            }
        }
        "port" => {
            if let Some(port) = node_state.port {
                let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
                match tokio::time::timeout(
                    Duration::from_secs(5),
                    tokio::net::TcpStream::connect(addr),
                )
                .await
                {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(e)) => Err(format!("port {port} connection failed: {e}")),
                    Err(_) => Err(format!("port {port} connection timed out")),
                }
            } else {
                Ok(()) // No port known, skip.
            }
        }
        "http" => {
            if let Some(port) = node_state.port {
                let path = liveness.path.as_deref().unwrap_or("/");
                let path = if path.starts_with('/') {
                    path.to_owned()
                } else {
                    format!("/{path}")
                };
                let url = format!("http://127.0.0.1:{port}{path}");
                let expected = liveness.expect_status.unwrap_or(200);

                let client = match reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => return Err(format!("http client error: {e}")),
                };

                match client.get(&url).send().await {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        if status == expected {
                            Ok(())
                        } else {
                            Err(format!("http status {status} (expected {expected})"))
                        }
                    }
                    Err(e) => Err(format!("http request failed: {e}")),
                }
            } else {
                Ok(()) // No port known, skip.
            }
        }
        other => {
            warn!(
                check_type = other,
                "unknown liveness probe type — treating as healthy"
            );
            Ok(())
        }
    }
}

/// Load the VeldConfig for a project root, if a veld.json exists.
fn load_config_for_project(project_root: &Path) -> Option<VeldConfig> {
    let config_path = project_root.join("veld.json");
    if !config_path.exists() {
        return None;
    }
    config::load_config(&config_path).ok()
}

/// Find the veld CLI binary path.
/// Checks: next to daemon binary, `~/.local/bin/veld`, then falls back to PATH.
fn find_veld_binary() -> std::path::PathBuf {
    // 1. Same directory as daemon binary.
    if let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("veld")))
        .filter(|p| p.exists())
    {
        return sibling;
    }

    // 2. Standard user install location.
    if let Some(home) = dirs::home_dir() {
        let user_bin = home.join(".local/bin/veld");
        if user_bin.exists() {
            return user_bin;
        }
    }

    // 3. System paths.
    for path in ["/usr/local/bin/veld", "/usr/bin/veld"] {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return p;
        }
    }

    // 4. Fall back to PATH lookup.
    std::path::PathBuf::from("veld")
}

/// Run `veld restart --name <run>` and wait for completion.
/// Captures stdout/stderr and logs the result.
async fn run_veld_restart(
    project_root: &Path,
    run_name: &str,
    internal_log: Option<&LogWriter>,
    user_path: &str,
) {
    let veld_bin = find_veld_binary();

    info!(
        run = run_name,
        bin = %veld_bin.display(),
        "running veld restart"
    );

    if let Some(log) = internal_log {
        let _ = log
            .write_line(&format!(
                "[recovery] running: {} restart --name {}",
                veld_bin.display(),
                run_name
            ))
            .await;
    }

    let result = tokio::time::timeout(
        Duration::from_secs(300), // 5 min timeout for full restart
        tokio::process::Command::new(&veld_bin)
            .arg("restart")
            .arg("--name")
            .arg(run_name)
            .current_dir(project_root)
            .env("PATH", user_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                info!(run = run_name, "veld restart completed successfully");
                if let Some(log) = internal_log {
                    let _ = log
                        .write_line(&format!(
                            "[recovery] veld restart completed (exit code {code})"
                        ))
                        .await;
                    if !stdout.trim().is_empty() {
                        for line in stdout.trim().lines() {
                            let _ = log.write_line(&format!("[recovery]   {line}")).await;
                        }
                    }
                }
            } else {
                warn!(run = run_name, exit_code = code, "veld restart failed");
                if let Some(log) = internal_log {
                    let _ = log
                        .write_line(&format!(
                            "[recovery] veld restart FAILED (exit code {code})"
                        ))
                        .await;
                    if !stdout.trim().is_empty() {
                        for line in stdout.trim().lines() {
                            let _ = log
                                .write_line(&format!("[recovery]   stdout: {line}"))
                                .await;
                        }
                    }
                    if !stderr.trim().is_empty() {
                        for line in stderr.trim().lines() {
                            let _ = log
                                .write_line(&format!("[recovery]   stderr: {line}"))
                                .await;
                        }
                    }
                }
            }
        }
        Ok(Err(e)) => {
            warn!(
                run = run_name,
                bin = %veld_bin.display(),
                error = %e,
                "failed to execute veld restart"
            );
            if let Some(log) = internal_log {
                let _ = log
                    .write_line(&format!("[recovery] failed to execute veld restart: {e}"))
                    .await;
            }
        }
        Err(_) => {
            warn!(run = run_name, "veld restart timed out (300s)");
            if let Some(log) = internal_log {
                let _ = log
                    .write_line("[recovery] veld restart timed out (300s)")
                    .await;
            }
        }
    }
}

/// Check whether a given PID is alive by sending signal 0.
fn is_process_alive(pid: u32) -> bool {
    let Some(pid) = i32::try_from(pid).ok().filter(|&p| p > 0) else {
        return false;
    };
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
