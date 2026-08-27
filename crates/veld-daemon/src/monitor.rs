use crate::broadcaster::Broadcaster;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use veld_core::config::{self, LivenessProbe, VeldConfig};
use veld_core::db::{Db, LogStream};
use veld_core::logging::LogWriter;
use veld_core::state::{NodeStatus, RunStatus};
// Shared login-shell PATH helper — see `veld_core::user_path` for why (bare
// launchd/systemd service PATH) and the timeout/fallback semantics.
// Directory-scoped, not the process-wide `cached_user_path`: a probe command
// or a recovery restart runs a specific project's own tooling, and a
// directory-based version-manager hook (`.nvmrc` + `.zshrc`) only fires
// correctly when resolution happens in that project's own directory — see
// `cached_user_path_for`'s doc comment.
use veld_core::user_path::cached_user_path_for;

/// Interval between health-check scans (seconds).
const SCAN_INTERVAL_SECS: u64 = 5;

/// Tracks each node's liveness-probe bookkeeping.
/// Key: `"project_root:run_name:node:variant"`.
type LastCheckMap = HashMap<String, ProbeBookkeeping>;

/// How often a *passing* probe writes a line to the `internal` stream.
///
/// The prober used to write two rows per node per poll unconditionally — one
/// before the probe and one after a success. In the #332 incident that was
/// **77,539 rows, 22.7% of the run's logs and 3.66 MiB of text**, every row
/// carrying the same sentence, bought at a fifth of the database. It is a
/// successful-probe history and nothing reads it.
///
/// A transition is what carries information, and those lines already exist
/// separately (failed / cannot run / self-healed). This heartbeat exists only so
/// a long-healthy run does not look *silent* to somebody watching
/// `veld logs --source internal`: ~21 rows per node per day instead of ~11,200.
const PASS_HEARTBEAT: Duration = Duration::from_secs(3600);

/// What a *passing* liveness probe should write to the `internal` stream.
#[derive(Debug, PartialEq, Eq)]
enum PassLog {
    /// The probe had been failing and is not any more — the transition, which is
    /// the line that carries information.
    Recovered,
    /// Nothing changed; this is the periodic "still passing" line.
    Heartbeat,
    /// Write nothing. The overwhelmingly common case, and the point of #332's
    /// fix 6.
    Quiet,
}

/// Decide what a passing probe writes.
///
/// Split out from the probe loop and given `Duration` rather than `Instant` so
/// the policy is testable: the loop's own `Instant`s cannot be moved backwards
/// portably, and this decision has three inputs that interact.
fn pass_log_decision(
    consecutive_failures: u32,
    was_unhealthy: bool,
    since_last_pass_log: Option<Duration>,
) -> PassLog {
    if consecutive_failures > 0 {
        return PassLog::Recovered;
    }
    // A zero counter with an unhealthy status is the `Unrunnable` shape: the
    // probe could not run at all, so no attempt was ever counted. That is still
    // a transition worth a line, and it is why this is not just the heartbeat
    // check.
    if was_unhealthy {
        return PassLog::Heartbeat;
    }
    // `None` — nothing logged yet in this daemon's lifetime — logs immediately,
    // so a fresh daemon says something about every node rather than going quiet
    // for an hour.
    if since_last_pass_log.is_none_or(|d| d >= PASS_HEARTBEAT) {
        return PassLog::Heartbeat;
    }
    PassLog::Quiet
}

/// Per-node liveness bookkeeping that must survive between scans.
#[derive(Clone, Copy)]
struct ProbeBookkeeping {
    /// When the probe last ran — what `liveness.interval_ms` is measured against.
    last_probe: Instant,
    /// When a *passing* probe last wrote a line, `None` if none ever has.
    /// Drives [`PASS_HEARTBEAT`]; `None` makes the first pass after a daemon
    /// start always log, so a fresh daemon says something about every node
    /// rather than going quiet for an hour.
    last_pass_logged: Option<Instant>,
}

/// Periodically scan all runs from the global registry and check process health.
/// When a status change is detected, update the registry and broadcast the event.
pub async fn run_health_monitor(broadcaster: Broadcaster) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(SCAN_INTERVAL_SECS));
    // After a macOS sleep the monotonic clock jumps; with the default `Burst`
    // behavior an overnight sleep queues thousands of missed 5s ticks that all
    // fire back-to-back on wake (CPU spike, PATH re-resolution storm). Skip the
    // backlog and resume the normal cadence instead.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_checks: LastCheckMap = HashMap::new();

    loop {
        interval.tick().await;
        debug!("running health-check scan");

        match scan_and_update(&broadcaster, &mut last_checks).await {
            Ok(changes) => {
                if changes > 0 {
                    info!("health scan detected {changes} status change(s)");
                }
            }
            Err(e) => {
                warn!("health scan error: {e}");
                crate::dbhealth::note_reported(&e);
            }
        }
    }
}

/// Scan the global registry, check each running process, and return the number
/// of status changes applied.
async fn scan_and_update(
    broadcaster: &Broadcaster,
    last_checks: &mut LastCheckMap,
) -> anyhow::Result<usize> {
    // Open per scan so the daemon self-heals across CLI upgrades that migrate
    // the schema (a long-lived handle would keep working, but a fresh open
    // also surfaces a NewerSchema error as a log line instead of a crash).
    let db = Db::open()?;
    let registry = db.registry()?;

    let mut changes = 0;

    for reg_entry in registry.projects.values() {
        let project_root = &reg_entry.project_root;

        for (run_name, run_info) in &reg_entry.runs {
            if run_info.status != RunStatus::Running {
                continue;
            }

            // Load the full RunState with node PIDs.
            let run_state = match db.get_run(project_root, run_name) {
                Ok(Some(rs)) => rs,
                Ok(None) => continue,
                Err(e) => {
                    debug!(
                        "could not load run state for {}: {e}",
                        project_root.display()
                    );
                    continue;
                }
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
                // Crash detection: one-step finalize, guarded on
                // starting/running only — a run that `begin_ending` already
                // moved to `stopping` (a deliberate stop mid-teardown) makes
                // this a no-op, so the stop can't be relabeled as a crash.
                //
                // Surviving sibling PIDs are killed here, not left for the
                // 600s GC straggler sweep: a half-dead run whose remaining
                // node still serves traffic while status says `crashed`
                // hands an agent contradictory signals for up to 10 minutes.
                let mut run = run_state;
                let mut dead_node: Option<String> = None;
                for (key, node) in run.nodes.iter_mut() {
                    if let Some(pid) = node.pid {
                        if is_process_alive(pid) {
                            // Escalating SIGTERM → SIGKILL — leak-freedom
                            // must not depend on the survivor honoring
                            // SIGTERM.
                            let _ = veld_core::process::kill_process(pid).await;
                        } else if dead_node.is_none() {
                            dead_node = Some(key.clone());
                        }
                    }
                }
                // Confirm pass; unconfirmed PIDs stay recorded so the GC
                // straggler sweep keeps covering them.
                for node in run.nodes.values_mut() {
                    if let Some(pid) = node.pid {
                        if !is_process_alive(pid) {
                            node.status = veld_core::state::NodeStatus::Stopped;
                            node.pid = None;
                        }
                    }
                }

                // Persist final node states while the run is still live (a
                // save against an already-finalized run is a whole-txn no-op),
                // then finalize as crashed.
                let _ = db.save_run(project_root, &reg_entry.project_name, &run);
                let detail = veld_core::state::EndDetail {
                    failed_node: dead_node,
                    ..Default::default()
                };
                let crashed = db
                    .finalize_crashed(&run.run_id, Some(&detail))
                    .unwrap_or(false);

                if crashed {
                    let event = serde_json::json!({
                        "event": "status_change",
                        "run": run_name,
                        "project": project_root.to_string_lossy(),
                        "old_status": "running",
                        "new_status": "crashed",
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    });
                    broadcaster.broadcast(&event).await;
                    changes += 1;
                }
                continue; // Skip liveness checks for a run that just ended.
            }

            // --- Liveness probe checks ---
            // Load the project config to access probe definitions.
            let config = match load_config_for_project(project_root) {
                Some(c) => c,
                None => continue,
            };

            // Create internal log writer for this run instance.
            let mut internal_log =
                LogWriter::for_run(db.clone(), project_root, run_name, LogStream::Internal);
            internal_log.set_run_id(run_info.run_id);

            changes += run_liveness_checks(
                &db,
                project_root,
                &reg_entry.project_name,
                run_name,
                &config,
                broadcaster,
                last_checks,
                Some(&internal_log),
            )
            .await;
        }
    }

    Ok(changes)
}

/// Run liveness probes for all healthy nodes in a run. Returns number of state changes.
#[allow(clippy::too_many_arguments)]
async fn run_liveness_checks(
    db: &Db,
    project_root: &Path,
    project_name: &str,
    run_name: &str,
    config: &VeldConfig,
    broadcaster: &Broadcaster,
    last_checks: &mut LastCheckMap,
    internal_log: Option<&LogWriter>,
) -> usize {
    // Reload state fresh for liveness checks.
    let mut run_owned = match db.get_run(project_root, run_name) {
        Ok(Some(r)) => r,
        _ => return 0,
    };
    let run = &mut run_owned;

    // Drop bookkeeping left by *previous instances* of this run name.
    //
    // `last_checks` is created once for the daemon's lifetime and has never been
    // pruned, which was harmless while its key was
    // `project_root:run_name:node:variant` — bounded by the node count. The key
    // now carries the run id (so a fresh run re-arms its probe interval and its
    // heartbeat, see `check_key` below), and a run id is a new UUID per
    // `veld start` — so without this the map would grow by one entry per node
    // per restart, forever, on a daemon that survives `veld update`.
    let name_prefix = format!("{}:{}:", project_root.to_string_lossy(), run_name);
    let live_prefix = format!("{name_prefix}{}:", run.run_id);
    last_checks.retain(|k, _| !k.starts_with(&name_prefix) || k.starts_with(&live_prefix));

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

        // Resolved: `probes` is hoistable to node level (F3), so a raw read
        // would silently stop probing a node that declares its liveness probe
        // once for all variants.
        let Some(resolved) = config.resolved(node_name, variant_name) else {
            continue;
        };
        let liveness = match &resolved.liveness {
            Some(lp) => lp,
            None => continue,
        };

        // Respect per-probe interval_ms — skip if not enough time has elapsed.
        //
        // **Keyed on the run instance, not just the run name.** This map is
        // per-daemon and never pruned, so a name-only key carries a *previous*
        // run's `last_pass_logged` into a fresh `veld start` of the same name —
        // and the new run's `internal` stream then says nothing about that node
        // for up to `PASS_HEARTBEAT`, which is not what README and
        // `skills/veld/SKILL.md` promise a reader ("a quiet stream means
        // healthy"). `run.run_id` changes per instance, so a restart re-arms
        // both the probe interval and the heartbeat.
        let check_key = format!(
            "{}:{}:{}:{}",
            project_root.to_string_lossy(),
            run_name,
            run.run_id,
            key
        );
        let probe_interval = Duration::from_millis(liveness.interval_ms);
        let book = last_checks.get(&check_key).copied();
        if let Some(b) = book {
            if b.last_probe.elapsed() < probe_interval {
                continue;
            }
        }
        last_checks.insert(
            check_key.clone(),
            ProbeBookkeeping {
                last_probe: Instant::now(),
                last_pass_logged: book.and_then(|b| b.last_pass_logged),
            },
        );

        // Run a single liveness check attempt.
        let working_dir = config::resolve_cwd(
            project_root,
            node_cfg.cwd.as_deref(),
            variant_cfg.cwd.as_deref(),
        );

        let node_label = format!("{node_name}:{variant_name}");

        // No "running probe" line. It was written before *every* probe and said
        // nothing a reader could act on — the only fact in it that was not
        // already in the node's config is the check type, which the failure
        // lines below now carry instead. See `PASS_HEARTBEAT`.

        let check_result = run_single_liveness_check(
            liveness,
            &working_dir,
            run,
            key,
            project_root,
            project_name,
            run_name,
        )
        .await;

        let node_state = match run.nodes.get_mut(key) {
            Some(ns) => ns,
            None => continue,
        };

        match check_result {
            Ok(()) => {
                // A passing probe writes at most one line, and only when it
                // carries information: the recovery itself, or the periodic
                // heartbeat that keeps a long-healthy run from reading as
                // silent. Both reads happen *before* the counters below reset
                // them. See `PASS_HEARTBEAT` and `pass_log_decision`.
                let was_unhealthy = node_state.status == NodeStatus::Unhealthy;
                let recovered = node_state.consecutive_failures > 0 || was_unhealthy;
                let decision = pass_log_decision(
                    node_state.consecutive_failures,
                    was_unhealthy,
                    book.and_then(|b| b.last_pass_logged).map(|t| t.elapsed()),
                );
                if let Some(log) = internal_log {
                    let line = match decision {
                        PassLog::Recovered => Some(format!(
                            "[liveness] {node_label} — probe passed again after {} failed \
                             attempt(s)",
                            node_state.consecutive_failures
                        )),
                        PassLog::Heartbeat => Some(format!(
                            "[liveness] {node_label} — probe passing (type: {})",
                            liveness.check_type
                        )),
                        PassLog::Quiet => None,
                    };
                    if let Some(line) = line {
                        let _ = log.write_line(&line).await;
                        if let Some(b) = last_checks.get_mut(&check_key) {
                            b.last_pass_logged = Some(Instant::now());
                        }
                    }
                }
                // Reset failure counter on success.
                if recovered {
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
            // A probe that cannot run is reported and never counted. Marking
            // the node unhealthy is honest — the check is not answering — but
            // letting it reach `failure_threshold` would restart the whole
            // environment over a config shape a restart cannot change, and the
            // restart's `veld start` now refuses on the same shape, so the
            // environment would stay down until `max_recoveries` ran out.
            Err(LivenessFailure::Unrunnable(detail)) => {
                if node_state.status != NodeStatus::Unhealthy
                    || node_state.last_liveness_error.as_deref() != Some(detail.as_str())
                {
                    node_state.status = NodeStatus::Unhealthy;
                    node_state.last_liveness_error = Some(detail.clone());
                    changes += 1;
                    warn!(
                        node = node_name.as_str(),
                        variant = variant_name.as_str(),
                        detail = detail.as_str(),
                        "liveness probe cannot run — reporting unhealthy without recovery, \
                         since restarting cannot change a probe's shape"
                    );
                    if let Some(log) = internal_log {
                        let _ = log
                            .write_line(&format!(
                                "[liveness] {node_label} — probe cannot run ({detail}). Not \
                                 counted toward recovery; fix the probe and restart."
                            ))
                            .await;
                    }
                }
            }
            Err(failure) => {
                let error_detail = failure.detail().to_owned();
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
                    // The check type is named here rather than on a line before
                    // every probe — this is the one place a reader needs it.
                    let _ = log
                        .write_line(&format!(
                            "[liveness] {node_label} — probe failed ({}/{} consecutive, type: \
                             {}): {error_detail}",
                            node_state.consecutive_failures,
                            liveness.failure_threshold,
                            liveness.check_type
                        ))
                        .await;
                }

                // **Never spend a recovery attempt during an update.** Recovery
                // execs `veld restart`, which refuses with `EX_TEMPFAIL` while
                // `veld update` holds the lock — and the attempt is counted and
                // persisted *before* that call, with nothing rolling it back. An
                // update deliberately leaves environments running while it
                // restarts the daemon and helper underneath them, so probes
                // failing during that window are the expected case, not a sick
                // node: without this, a single update could burn a node's whole
                // budget and land it in `recovery_exhausted` having never
                // actually restarted anything. Skipping (rather than counting a
                // failed attempt) is the point — the next scan after the update
                // recovers normally if the node really is unhealthy.
                if veld_core::update_lock::current().is_some() {
                    continue;
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
                        let _ = db.save_run(project_root, project_name, run);

                        // Run the restart. This stops+starts the entire environment,
                        // creating fresh node state with recovery_count: 0.
                        run_veld_restart(project_root, run_name, internal_log).await;

                        // Restore recovery_count on the fresh state so it accumulates
                        // across restarts and eventually hits max_recoveries.
                        if let Ok(Some(mut fresh_run)) = db.get_run(project_root, run_name) {
                            if let Some(fresh_node) = fresh_run.nodes.get_mut(key) {
                                fresh_node.recovery_count = new_recovery_count;
                            }
                            let _ = db.save_run(project_root, project_name, &fresh_run);
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
        let _ = db.save_run(project_root, project_name, run);
    }

    changes
}

/// Why a liveness check did not pass.
///
/// The distinction is load-bearing, not cosmetic. A **failed** probe means the
/// thing it watches is sick, and restarting is the documented remedy. An
/// **unrunnable** probe means the probe itself cannot execute — a `port` check on
/// a node that has no port, a `type` veld does not implement — and no number of
/// restarts changes a config shape. Counting those toward `failure_threshold`
/// takes the whole environment down and keeps it down: the restart re-enters
/// `veld start`, which now refuses on the very lint error the probe shape
/// produces. `veld update` deliberately does not stop live environments, so this
/// is reachable on a config that was accepted when the run began.
enum LivenessFailure {
    /// The probe ran and did not pass. Counts toward recovery.
    Failed(String),
    /// The probe could not run at all. Reported, never counted.
    Unrunnable(String),
}

impl LivenessFailure {
    fn detail(&self) -> &str {
        match self {
            LivenessFailure::Failed(d) | LivenessFailure::Unrunnable(d) => d,
        }
    }
}

/// Run a single liveness check for a node.
/// Returns `Ok(())` if healthy, else why it did not pass — see [`LivenessFailure`].
#[allow(clippy::too_many_arguments)]
async fn run_single_liveness_check(
    liveness: &LivenessProbe,
    working_dir: &Path,
    run: &veld_core::state::RunState,
    node_key: &str,
    project_root: &Path,
    project_name: &str,
    run_name: &str,
) -> Result<(), LivenessFailure> {
    let node_state = match run.nodes.get(node_key) {
        Some(ns) => ns,
        None => return Ok(()),
    };

    match liveness.check_type.as_str() {
        "command" | "bash" => {
            if let Some(cmd) = liveness.cmd.spec() {
                // Timeout command checks to prevent hanging the monitor loop.
                // Inject the resolved user PATH so probes find tools like
                // pg_isready even when the daemon starts at boot.
                let Ok(mut command) = veld_core::process::tokio_command(&cmd) else {
                    return Err(LivenessFailure::Unrunnable(
                        "liveness probe declares an empty argv".to_owned(),
                    ));
                };
                let result = tokio::time::timeout(Duration::from_secs(30), async {
                    // Resolved in `working_dir`, not the process-wide cache:
                    // a probe like `pg_isready` from a project-local
                    // nvm/asdf shim needs the PATH a login shell would have
                    // sitting in this node's own directory, which is what
                    // `working_dir` (already `node`/`variant`-`cwd`-resolved
                    // above) represents. Inside this timeout, not before it:
                    // a cold resolution (first-ever check for this exact
                    // directory) then costs at most this probe's own 30s
                    // budget instead of stacking an unbounded PATH-resolution
                    // wait in front of it — a stale hit still returns
                    // instantly, so this only matters the first time.
                    let user_path = cached_user_path_for(working_dir).await;
                    command
                        .current_dir(working_dir)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped())
                        .env("PATH", &user_path)
                        // The veld-owned vars — VELD_RUN, VELD_ROOT, the
                        // port/url/host family — from the shared builder.
                        .envs(veld_core::orchestrator::veld_owned_env(
                            &veld_core::orchestrator::NodeEnvContext::from_node_state(
                                run_name,
                                Some(run.run_id.to_string()),
                                project_root,
                                project_name,
                                node_state,
                            ),
                        ));
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
                            Err(LivenessFailure::Failed(format!("exit code {code}")))
                        } else {
                            Err(LivenessFailure::Failed(format!(
                                "exit code {code}: {stderr}"
                            )))
                        }
                    }
                    Ok(Err(e)) => Err(LivenessFailure::Failed(format!("exec error: {e}"))),
                    Err(_) => Err(LivenessFailure::Failed(
                        "command timed out (30s)".to_owned(),
                    )),
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
                    Ok(Err(e)) => Err(LivenessFailure::Failed(format!(
                        "port {port} connection failed: {e}"
                    ))),
                    Err(_) => Err(LivenessFailure::Failed(format!(
                        "port {port} connection timed out"
                    ))),
                }
            } else {
                // Absent is never zero. Reporting healthy because there is
                // nothing to check is how a node with a port-shaped probe and
                // no port stayed "healthy" forever — including after it died.
                Err(LivenessFailure::Unrunnable(
                    "liveness probe is \"port\", but this node has no port — use \"command\""
                        .to_owned(),
                ))
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
                    Err(e) => {
                        return Err(LivenessFailure::Failed(format!("http client error: {e}")));
                    }
                };

                match client.get(&url).send().await {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        if status == expected {
                            Ok(())
                        } else {
                            Err(LivenessFailure::Failed(format!(
                                "http status {status} (expected {expected})"
                            )))
                        }
                    }
                    Err(e) => Err(LivenessFailure::Failed(format!("http request failed: {e}"))),
                }
            } else {
                Err(LivenessFailure::Unrunnable(
                    "liveness probe is \"http\", but this node has no port — use \"command\""
                        .to_owned(),
                ))
            }
        }
        other => {
            // A typo used to mean "always healthy", so `type: "htpp"` disabled
            // the probe silently. `unknown-probe-type` rejects it at validate
            // time; this is the path for a config that predates the rule.
            warn!(check_type = other, "unknown liveness probe type");
            Err(LivenessFailure::Unrunnable(format!(
                "unknown liveness probe type \"{other}\" — expected \"command\", \"http\" or \
                 \"port\""
            )))
        }
    }
}

/// Load the VeldConfig for a project root, if a root config exists.
fn load_config_for_project(project_root: &Path) -> Option<VeldConfig> {
    // `root_config_in` already proved the file exists.
    let config_path = veld_core::config::root_config_in(project_root)?;
    config::parse_config(&config_path).ok()
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
///
/// **Precondition, enforced only at the call site: no `veld update` may be
/// holding the update lock.** `veld restart` is not on the CLI's
/// `command_survives_an_update` allow-list, so during an update it exits 75
/// (`EX_TEMPFAIL`) without doing anything — while the caller has already
/// incremented and *persisted* `recovery_count`. A second call site that forgets
/// the check therefore burns a node's whole recovery budget over one update and
/// lands it in `recovery_exhausted` having never restarted anything. The check
/// lives at the caller because what it must do is **skip** the cycle rather than
/// record a failed attempt, which is a decision this function cannot make for it.
async fn run_veld_restart(project_root: &Path, run_name: &str, internal_log: Option<&LogWriter>) {
    let veld_bin = find_veld_binary();
    // Resolved in `project_root`: this execs `veld restart`, which itself
    // spawns the project's declared commands — same directory-scoped-PATH
    // need as `spawn_veld`.
    let user_path = cached_user_path_for(project_root).await;

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
            // Null rather than inherited, as in `spawn_veld_in`: a daemon
            // launched from a terminal would otherwise pass its tty to an
            // automatic recovery restart, which must never wait on a human.
            // (Its stderr is piped, so the prompt gate already refuses here —
            // this is the half that does not depend on that staying true.)
            .stdin(std::process::Stdio::null())
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

            // One transaction for the whole restart report, not one per line: a
            // `veld restart` can print hundreds of lines and the loops below
            // used to take the process-wide connection lock for each of them.
            if output.status.success() {
                info!(run = run_name, "veld restart completed successfully");
                if let Some(log) = internal_log {
                    let mut lines = vec![format!(
                        "[recovery] veld restart completed (exit code {code})"
                    )];
                    lines.extend(
                        stdout
                            .trim()
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|line| format!("[recovery]   {line}")),
                    );
                    let _ = log.write_lines(chrono::Utc::now(), &lines);
                }
            } else {
                warn!(run = run_name, exit_code = code, "veld restart failed");
                if let Some(log) = internal_log {
                    let mut lines =
                        vec![format!("[recovery] veld restart FAILED (exit code {code})")];
                    lines.extend(
                        stdout
                            .trim()
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|line| format!("[recovery]   stdout: {line}")),
                    );
                    lines.extend(
                        stderr
                            .trim()
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|line| format!("[recovery]   stderr: {line}")),
                    );
                    let _ = log.write_lines(chrono::Utc::now(), &lines);
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

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::state::{NodeState, RunState};

    fn run_with_node(port: Option<u16>) -> RunState {
        let mut run = RunState::new("dev", "proj");
        let mut ns = NodeState::new("web", "local");
        ns.status = NodeStatus::Healthy;
        ns.port = port;
        run.nodes.insert("web:local".into(), ns);
        run
    }

    /// The 22.7% of #332's database that this fix is about. A healthy node whose
    /// probe passed recently writes **nothing** — that is the whole point, and it
    /// is the assertion most likely to be quietly undone by a later change.
    #[test]
    fn a_healthy_node_that_keeps_passing_writes_nothing() {
        assert_eq!(
            pass_log_decision(0, false, Some(Duration::from_secs(1))),
            PassLog::Quiet
        );
        assert_eq!(
            pass_log_decision(0, false, Some(PASS_HEARTBEAT - Duration::from_secs(1))),
            PassLog::Quiet,
            "one second inside the window is still inside it"
        );
    }

    /// A run that has been healthy for hours must not read as *silent*, which is
    /// the one thing the old two-rows-per-poll behaviour did buy.
    #[test]
    fn a_long_healthy_node_still_says_so_once_an_interval() {
        assert_eq!(
            pass_log_decision(0, false, Some(PASS_HEARTBEAT)),
            PassLog::Heartbeat,
            "the boundary is inclusive — at exactly the interval the heartbeat is due"
        );
        assert_eq!(
            pass_log_decision(0, false, Some(PASS_HEARTBEAT * 3)),
            PassLog::Heartbeat
        );
        assert_eq!(
            pass_log_decision(0, false, None),
            PassLog::Heartbeat,
            "nothing logged yet in this daemon's lifetime must log immediately, or a \
             freshly started daemon looks dead for an hour"
        );
    }

    /// The transitions are what the `internal` stream is *for*, and they must
    /// survive the quieting. Note the second case: a node marked unhealthy
    /// because its probe could not run at all has a zero failure counter, so a
    /// gate written only against the counter would let its recovery pass silently.
    #[test]
    fn a_transition_back_to_passing_is_always_reported() {
        assert_eq!(
            pass_log_decision(3, false, Some(Duration::from_secs(1))),
            PassLog::Recovered,
            "a probe that had been failing must report recovering even mid-window"
        );
        assert_eq!(
            pass_log_decision(0, true, Some(Duration::from_secs(1))),
            PassLog::Heartbeat,
            "the `Unrunnable` shape: unhealthy with no counted attempt must still \
             report that the probe is answering again"
        );
        // And a failing counter outranks an unhealthy status, so the line names
        // the number of attempts rather than falling through to the heartbeat.
        assert_eq!(pass_log_decision(2, true, None), PassLog::Recovered);
    }

    /// The heartbeat only pays for itself if it is rare. At one hour a node
    /// costs ~24 rows a day where the old behaviour cost ~11,200 per probe
    /// stream; drop the interval into the probe's own cadence and the fix is
    /// undone without a single test failing.
    #[test]
    fn the_heartbeat_interval_stays_far_above_the_probe_cadence() {
        assert!(
            PASS_HEARTBEAT >= Duration::from_secs(900),
            "a heartbeat this frequent re-creates the row volume it was written to remove"
        );
    }

    fn probe(check_type: &str) -> LivenessProbe {
        serde_json::from_value(serde_json::json!({ "type": check_type }))
            .expect("a probe with only a type parses")
    }

    /// A probe that cannot run and a probe that ran and failed are different
    /// answers, and only one of them is worth restarting an environment over.
    ///
    /// Getting this wrong is not theoretical: `veld update` does not stop live
    /// environments, so a run whose config predates `probe-needs-port` keeps
    /// going with a probe that now reports failure — and the recovery it would
    /// trigger re-enters `veld start`, which refuses on that same config. The
    /// environment would stay down until `max_recoveries` ran out.
    #[tokio::test]
    async fn a_probe_that_cannot_run_is_not_a_probe_that_failed() {
        let dir = std::env::temp_dir();

        // No port to connect to: unrunnable, whatever the node is doing.
        for check_type in ["port", "http"] {
            let run = run_with_node(None);
            let err = run_single_liveness_check(
                &probe(check_type),
                &dir,
                &run,
                "web:local",
                &dir,
                "proj",
                "dev",
            )
            .await
            .expect_err("a port-shaped probe with no port cannot pass");
            assert!(
                matches!(err, LivenessFailure::Unrunnable(_)),
                "{check_type}: {}",
                err.detail()
            );
        }

        // A type veld does not implement: also unrunnable, not "unhealthy".
        let run = run_with_node(Some(3000));
        let err =
            run_single_liveness_check(&probe("htpp"), &dir, &run, "web:local", &dir, "proj", "dev")
                .await
                .expect_err("an unknown probe type cannot pass");
        assert!(
            matches!(err, LivenessFailure::Unrunnable(_)),
            "{}",
            err.detail()
        );

        // A real check against a port nothing is listening on DID run, and
        // failed — that one counts, because a restart can fix it.
        let run = run_with_node(Some(1));
        let err =
            run_single_liveness_check(&probe("port"), &dir, &run, "web:local", &dir, "proj", "dev")
                .await
                .expect_err("nothing is listening on port 1");
        assert!(
            matches!(err, LivenessFailure::Failed(_)),
            "{}",
            err.detail()
        );
    }
}
