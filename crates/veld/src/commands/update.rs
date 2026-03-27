use std::io::Write;

use veld_core::config;
use veld_core::orchestrator::Orchestrator;
use veld_core::state::{GlobalRegistry, ProjectState, RunStatus};

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
            output::print_info("Installing update...");

            match veld_core::setup::perform_update(&new_version).await {
                Ok(()) => {
                    output::print_success(&format!("Updated to {new_version}."));
                    cleanup_stale_binaries();
                    output::print_info("Restarting services with new binaries...");
                    restart_services().await;
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
    let registry = match GlobalRegistry::load() {
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

        let mut orchestrator = Orchestrator::new(config_path, cfg);
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
    if let Ok(mut state) = ProjectState::load(project_root) {
        state.runs.remove(run_name);
        let _ = state.save(project_root);
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

/// Restart daemon and helper so they run the newly installed binaries.
///
/// The install script (run by `perform_update`) already restarts launchd /
/// systemd services for both privileged and unprivileged modes. This function
/// only needs to handle the "auto" case (no persistent service) and print
/// mode-specific guidance after the update.
async fn restart_services() {
    let mode = super::read_setup_mode();

    match mode.as_deref() {
        Some("privileged") => {
            // The install script already bounced the system LaunchDaemon /
            // systemd service with the new binaries — no second sudo needed.
            // The plist on disk still has the correct binary paths since this
            // is an in-place update (paths unchanged).
            output::print_success("Services restarted by the installer (privileged mode).");
        }
        Some("unprivileged") => {
            // Install script already restarted the user-level LaunchAgent /
            // systemd --user service.
            output::print_success("Services restarted by the installer.");
        }
        _ => {
            // "auto" mode or no mode — just kill the user-level helper.
            // Next `veld start` will re-bootstrap with the new binary.
            output::print_info("Restarting auto-bootstrapped helper...");
            let user_socket = veld_core::helper::user_socket_path();
            let client = veld_core::helper::HelperClient::new(&user_socket);
            if client.shutdown().await.is_ok() {
                output::print_info("Helper stopped. It will restart on next `veld start`.");
            }
        }
    }
}
