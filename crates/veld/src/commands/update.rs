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
