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
                    output::print_info("Running setup to restart services...");
                    // Re-run setup to restart daemons with the new binaries.
                    let _ = commands_setup_run().await;
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

/// Run `veld setup` to restart helper/daemon with updated binaries.
async fn commands_setup_run() -> i32 {
    match veld_core::setup::require_setup().await {
        Ok(_) => 0,
        Err(_) => {
            // Setup not complete — tell user to run it manually.
            output::print_info("Run `veld setup` to complete configuration.");
            0
        }
    }
}
