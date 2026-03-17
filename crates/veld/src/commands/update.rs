use crate::output;

/// `veld update` -- update Veld to the latest version.
pub async fn run() -> i32 {
    let current = env!("CARGO_PKG_VERSION");
    output::print_info(&format!("Current version: {current}"));
    output::print_info("Checking for updates...");

    match veld_core::setup::check_update().await {
        Ok(Some(new_version)) => {
            output::print_info(&format!("New version available: {current} → {new_version}"));
            output::print_info("Installing update...");

            match veld_core::setup::perform_update(&new_version).await {
                Ok(()) => {
                    output::print_success(&format!("Updated to {new_version}."));
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

/// Restart daemon and helper so they run the newly installed binaries.
/// Mode-aware: uses sudo only for privileged mode, runs without sudo otherwise.
async fn restart_services() {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            output::print_error(
                &format!("Cannot determine executable path: {e}. Run `veld setup` manually."),
                false,
            );
            return;
        }
    };

    let mode = super::read_setup_mode();

    match mode.as_deref() {
        Some("privileged") => {
            // Use sudo to restart system services.
            let status = std::process::Command::new("sudo")
                .arg(&exe)
                .arg("setup")
                .arg("privileged")
                .status();
            match status {
                Ok(s) if s.success() => {
                    output::print_success("Services restarted.");
                }
                Ok(s) => {
                    output::print_error(
                        &format!(
                            "Setup exited with code {}. Run `veld setup` manually.",
                            s.code().unwrap_or(-1)
                        ),
                        false,
                    );
                }
                Err(e) => {
                    output::print_error(
                        &format!("Failed to run setup: {e}. Run `veld setup` manually."),
                        false,
                    );
                }
            }
        }
        Some("unprivileged") => {
            // Re-run unprivileged setup to restart user-level services.
            eprintln!("Restarting user-level services...");
            let status = std::process::Command::new(&exe)
                .arg("setup")
                .arg("unprivileged")
                .status();
            match status {
                Ok(s) if s.success() => {
                    output::print_success("Services restarted.");
                }
                Ok(s) => {
                    output::print_error(
                        &format!(
                            "Setup exited with code {}. Run `veld setup` manually.",
                            s.code().unwrap_or(-1)
                        ),
                        false,
                    );
                }
                Err(e) => {
                    output::print_error(
                        &format!("Failed to run setup: {e}. Run `veld setup` manually."),
                        false,
                    );
                }
            }
        }
        _ => {
            // "auto" mode or no mode — just kill the user-level helper.
            // Next `veld start` will re-bootstrap with the new binary.
            eprintln!("Restarting auto-bootstrapped helper...");
            let user_socket = veld_core::helper::user_socket_path();
            let client = veld_core::helper::HelperClient::new(&user_socket);
            if client.shutdown().await.is_ok() {
                eprintln!("Helper stopped. It will restart on next `veld start`.");
            }
        }
    }
}
