use std::io::Write;

use veld_core::config;
use veld_core::orchestrator::Orchestrator;
use veld_core::state::RunStatus;

use crate::output;

/// `veld update` -- update Veld to the latest version.
pub async fn run() -> i32 {
    let current = env!("CARGO_PKG_VERSION");
    output::print_info(&format!("Current version: {current}"));
    output::print_info("Checking for updates...");

    match veld_core::setup::check_update().await {
        Ok(Some(new_version)) => {
            // Check for running environments and stop them before updating.
            let running = find_running_environments();
            if !running.is_empty() {
                println!();
                output::print_info(&format!(
                    "Found {} running environment(s) that must be stopped before updating:",
                    running.len()
                ));
                for (project, run_name) in &running {
                    println!(
                        "  {} {}",
                        output::cyan(run_name),
                        output::dim(&format!("({})", project.display()))
                    );
                }
                println!();
                print!(
                    "{}",
                    output::yellow("Stop all environments and proceed with update? [y/N] ")
                );
                let _ = std::io::stdout().flush();

                let mut answer = String::new();
                if std::io::stdin().read_line(&mut answer).is_err()
                    || !answer.trim().eq_ignore_ascii_case("y")
                {
                    output::print_info("Update cancelled.");
                    return 0;
                }

                // Stop all running environments.
                let stopped = stop_all_environments(&running).await;
                output::print_success(&format!("Stopped {stopped} environment(s)."));
                println!();
            }

            output::print_info(&format!("New version available: {current} → {new_version}"));

            // Privileged mode relies on the helper's own binary-change watcher
            // plus launchd/systemd (KeepAlive + WatchPaths) to bring up the new
            // version — restarting the root service from here would need sudo.
            // Both mechanisms require the service to still be REGISTERED. A
            // merely-unresponsive process behind an intact registration gets
            // relaunched onto the new binary, so only the job being gone means
            // the update truly can't self-apply. Check BEFORE installing so
            // that's reported as the pre-existing problem it is, instead of a
            // 45-second wait ending in a misleading "did not pick up the new
            // binary". In unprivileged mode the installer bootstraps the
            // LaunchAgent itself, so no pre-flight skip there.
            let helper_dead_privileged = super::read_setup_mode().as_deref() == Some("privileged")
                && !privileged_helper_serviceable().await;
            if helper_dead_privileged {
                output::print_error(
                    "The veld-helper service is not registered with the service manager. The \
                     update will install new binaries, but the helper cannot restart itself — \
                     run `veld setup privileged` afterwards.",
                    false,
                );
            }

            output::print_info("Installing update...");

            match veld_core::setup::perform_update(&new_version).await {
                Ok(()) => {
                    output::print_success(&format!("Updated to {new_version}."));
                    cleanup_stale_binaries();
                    output::print_info("Restarting services with new binaries...");
                    restart_services(&new_version, helper_dead_privileged).await;
                    refresh_hammerspoon().await;
                    0
                }
                Err(e) => {
                    output::print_error(&format!("Update failed: {e}"), false);
                    1
                }
            }
        }
        Ok(None) => {
            output::print_success(&format!("Already on the latest version ({current})."));
            0
        }
        Err(e) => {
            output::print_error(&format!("Update check failed: {e}"), false);
            1
        }
    }
}

/// Find all running environments across all projects.
/// Returns (project_root, run_name) pairs.
fn find_running_environments() -> Vec<(std::path::PathBuf, String)> {
    let registry = match veld_core::db::Db::open().and_then(|db| db.registry()) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut running = Vec::new();
    for entry in registry.projects.values() {
        for (run_name, run_info) in &entry.runs {
            if run_info.status == RunStatus::Running {
                running.push((entry.project_root.clone(), run_name.clone()));
            }
        }
    }
    running
}

/// Stop all running environments. Returns number successfully stopped.
async fn stop_all_environments(envs: &[(std::path::PathBuf, String)]) -> usize {
    let mut stopped = 0;
    for (project_root, run_name) in envs {
        let config_path = project_root.join("veld.json");
        let cfg = match config::load_config(&config_path) {
            Ok(c) => c,
            Err(e) => {
                output::print_error(
                    &format!("Failed to load config for {}: {e}", project_root.display()),
                    false,
                );
                // Even if config can't load, try to clean up state.
                cleanup_state(project_root, run_name);
                continue;
            }
        };

        let mut orchestrator = match Orchestrator::new(config_path, cfg) {
            Ok(o) => o,
            Err(e) => {
                output::print_error(&format!("Failed to initialize: {e}"), false);
                cleanup_state(project_root, run_name);
                continue;
            }
        };
        match orchestrator.stop(run_name).await {
            Ok(_) => {
                output::print_info(&format!("  Stopped '{run_name}'"));
                stopped += 1;
            }
            Err(e) => {
                output::print_error(&format!("  Failed to stop '{run_name}': {e}"), false);
            }
        }
    }
    stopped
}

/// Best-effort cleanup of state for a run when config can't be loaded.
fn cleanup_state(project_root: &std::path::Path, run_name: &str) {
    if let Ok(db) = veld_core::db::Db::open() {
        let _ = db.remove_run(project_root, run_name);
    }
}

/// Re-install the Hammerspoon Spoon if it was previously set up.
/// The Spoon files are embedded in the binary, so they need to be re-extracted
/// after every CLI update to pick up any changes.
async fn refresh_hammerspoon() {
    let spoon_dir = match dirs::home_dir() {
        Some(h) => h.join(".hammerspoon/Spoons/Veld.spoon"),
        None => return,
    };
    if !spoon_dir.exists() {
        return;
    }

    output::print_info("Updating Hammerspoon Veld.spoon...");
    match veld_core::setup::install_hammerspoon().await {
        Ok(result) => {
            output::print_success(&result.message);
        }
        Err(e) => {
            output::print_error(
                &format!(
                    "Failed to update Hammerspoon Spoon: {e}. Run `veld setup hammerspoon` manually."
                ),
                false,
            );
        }
    }
}

/// Remove stale daemon/helper copies next to the CLI binary.
///
/// If a dev previously ran `just dev-install` or manually copied binaries into
/// `~/.local/bin/`, those copies persist after `veld update` and can shadow the
/// real binaries in `~/.local/lib/veld/`. This cleans them up.
fn cleanup_stale_binaries() {
    let cli_dir = match std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_owned()))
    {
        Some(d) => d,
        None => return,
    };
    let lib = veld_core::paths::lib_dir();
    for name in ["veld-daemon", "veld-helper"] {
        let stale = cli_dir.join(name);
        let canonical = lib.join(name);
        if stale.exists() && stale != canonical && std::fs::remove_file(&stale).is_ok() {
            output::print_info(&format!("Removed stale {}", stale.display()));
        }
    }
}

/// Whether the privileged helper can pick up a new binary by itself: either
/// its socket answers (live helper with a watcher), or the service manager
/// still has it registered (launchd KeepAlive/WatchPaths or systemd
/// Restart=always relaunch it onto the new binary even if the process is
/// transiently down).
async fn privileged_helper_serviceable() -> bool {
    let socket = veld_core::helper::system_socket_path();
    let client = veld_core::helper::HelperClient::new(&socket);
    if client.status().await.is_ok() {
        return true;
    }
    if cfg!(target_os = "macos") {
        // Only a definitive "no job" counts as unserviceable — a failed/timed
        // out query (None) must not scare the user into re-running setup.
        veld_core::setup::launchd_job_registered("system", veld_core::setup::HELPER_LABEL_MACOS)
            .await
            != Some(false)
    } else {
        veld_core::setup::systemd_pid_query(veld_core::setup::HELPER_SERVICE_LINUX, false).await
            != Some(None)
    }
}

/// Restart the helper/daemon so they run the newly installed binaries, then
/// verify the helper actually came back healthy.
///
/// A managed helper (privileged/unprivileged) restarts *itself* when its binary
/// changes on disk (an in-process watcher exits so launchd relaunches the new
/// version — no sudo), complemented by the plist's `WatchPaths`. Rather than
/// assume that worked (the old bug), we poll until the helper reports the new
/// version, and give actionable guidance if it doesn't.
///
/// `target_version` is the version we just updated TO (from `check_update`),
/// NOT `env!("CARGO_PKG_VERSION")` — this process is the *old* CLI, so its
/// compile-time version is the version we updated *from*. Comparing against
/// that would invert the check (fail on every successful update, pass on a
/// failed one).
async fn restart_services(target_version: &str, helper_dead_privileged: bool) {
    let mode = super::read_setup_mode();

    // Auto mode has no persistent service: stop the ephemeral helper so the
    // next `veld start` re-bootstraps it with the new binary.
    if !matches!(mode.as_deref(), Some("privileged") | Some("unprivileged")) {
        output::print_info("Restarting auto-bootstrapped helper...");
        let user_socket = veld_core::helper::user_socket_path();
        let client = veld_core::helper::HelperClient::new(&user_socket);
        if client.shutdown().await.is_ok() {
            output::print_info("Helper stopped. It will restart on next `veld start`.");
        }
        return;
    }

    if helper_dead_privileged {
        // Already reported before the install; a dead privileged helper has no
        // watcher and nothing here can restart it without sudo, so waiting 45s
        // for its version to flip would only produce a second, misleading error.
        output::print_error(
            "Skipping helper restart check — the helper service was not registered before the \
             update. Run `veld setup privileged` to start it on the new version.",
            false,
        );
    } else {
        // Verify against the specific socket for this mode — not `connect()` (which
        // falls through to the user socket and could latch onto a stale auto-helper
        // while the privileged one is mid-restart).
        let socket = if mode.as_deref() == Some("privileged") {
            veld_core::helper::system_socket_path()
        } else {
            veld_core::helper::user_socket_path()
        };
        output::print_info("Waiting for veld-helper to restart with the new binary...");
        if wait_for_helper_version(&socket, target_version, std::time::Duration::from_secs(45))
            .await
        {
            output::print_success("veld-helper restarted and healthy.");
        } else {
            output::print_error(
                "veld-helper did not pick up the new binary automatically. \
                 Run `veld doctor`; if it stays down, re-run `veld setup`.",
                false,
            );
        }
    }

    // The daemon is a user-level service (LaunchAgent / systemd --user) that the
    // installer restarts. Verify it came back on the new binary too — otherwise
    // `veld update` returns while the daemon is mid-restart, and an immediate
    // `veld doctor` shows "Daemon: not running / Feedback server not responding"
    // even though it self-heals moments later.
    output::print_info("Waiting for veld-daemon to restart with the new binary...");
    if wait_for_daemon_version(target_version, std::time::Duration::from_secs(45)).await {
        output::print_success("veld-daemon restarted and healthy.");
    } else {
        output::print_error(
            "veld-daemon did not pick up the new binary automatically. \
             Run `veld doctor`; if it stays down, re-run `veld setup`.",
            false,
        );
    }
}

/// Poll the daemon's `/api/health` until it reports `expected_version`, or the
/// timeout elapses.
///
/// The daemon is hard-restarted by the installer (bootout + bootstrap), so its
/// HTTP endpoint goes down and comes back; waiting for the version to match
/// confirms the NEW daemon is serving, not a lingering old instance or a
/// pre-change daemon that has no `version` field (which reports nothing and
/// correctly times out into the actionable error).
async fn wait_for_daemon_version(expected_version: &str, timeout: std::time::Duration) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = std::time::Instant::now();
    loop {
        if let Ok(resp) = client.get("http://127.0.0.1:19899/api/health").send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if body.get("version").and_then(|v| v.as_str()) == Some(expected_version) {
                    return true;
                }
            }
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Poll the helper on `socket` until it reports `expected_version`, or the
/// timeout elapses.
///
/// The managed helper keeps serving the OLD binary until its watcher fires
/// (~12s), so we wait for the version to actually flip rather than treating
/// "a helper is reachable" as success. A pre-change helper (no `version` field)
/// reports `None` and never matches, so this correctly times out into the
/// actionable error instead of falsely reporting success on the first update.
async fn wait_for_helper_version(
    socket: &std::path::Path,
    expected_version: &str,
    timeout: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    let client = veld_core::helper::HelperClient::new(socket);
    loop {
        if let Ok(Some(v)) = client.version().await {
            if v == expected_version {
                return true;
            }
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
