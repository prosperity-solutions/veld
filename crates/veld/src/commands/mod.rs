pub mod gc;
pub mod graph;
pub mod init;
pub mod list;
pub mod logs;
pub mod nodes;
pub mod presets;
pub mod restart;
pub mod runs;
pub mod setup;
pub mod start;
pub mod status;
pub mod stop;
pub mod uninstall;
pub mod update;
pub mod urls;
pub mod version;

use crate::output;

/// Check that the Veld setup has been completed. Returns `true` if ready,
/// `false` (and prints an error) if not.
///
/// Commands that operate on environments should call this before doing
/// anything else.
pub async fn require_setup(json: bool) -> bool {
    match veld_core::setup::require_setup().await {
        Ok(_status) => true,
        Err(e) => {
            if json {
                let veld_core::setup::SetupError::Incomplete { ref missing } = e;
                let payload = veld_core::setup::setup_required_json(missing);
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            } else {
                output::print_error("Veld is not set up yet. Run `veld setup` first.", false);
            }
            false
        }
    }
}

/// Load the project configuration from the current working directory.
/// On failure prints an error and returns `None`.
pub fn load_config(json: bool) -> Option<(std::path::PathBuf, veld_core::config::VeldConfig)> {
    match veld_core::config::load_config_from_cwd() {
        Ok(pair) => Some(pair),
        Err(e) => {
            output::print_error(&format!("Failed to load config: {e}"), json);
            None
        }
    }
}
