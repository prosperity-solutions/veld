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

/// Resolve the run name to use. If `name` is given, use it directly. Otherwise
/// look at the project state: if exactly one active run exists, use that; if
/// zero or multiple, print an actionable error and return `None`.
///
/// For read-only commands (status, urls, logs), also considers stopped runs
/// so that users can inspect past runs without specifying `--name`.
pub fn resolve_run_name(
    name: Option<String>,
    project_state: &veld_core::state::ProjectState,
    include_stopped: bool,
    json: bool,
) -> Option<String> {
    if let Some(n) = name {
        return Some(n);
    }

    // Prefer active runs first.
    let active_runs: Vec<&String> = project_state
        .runs
        .iter()
        .filter(|(_, r)| r.status != veld_core::state::RunStatus::Stopped)
        .map(|(name, _)| name)
        .collect();

    match active_runs.len() {
        1 => return Some(active_runs[0].clone()),
        n if n > 1 => {
            let mut names: Vec<&str> = active_runs.iter().map(|s| s.as_str()).collect();
            names.sort();
            output::print_error(
                &format!(
                    "Multiple active runs found. Specify one with --name: {}",
                    names.join(", ")
                ),
                json,
            );
            return None;
        }
        _ => {}
    }

    // No active runs. For read-only commands, fall back to any run.
    if include_stopped && project_state.runs.len() == 1 {
        return Some(project_state.runs.keys().next().unwrap().clone());
    }

    if include_stopped && project_state.runs.len() > 1 {
        let mut names: Vec<&str> = project_state.runs.keys().map(|s| s.as_str()).collect();
        names.sort();
        output::print_error(
            &format!(
                "Multiple runs found. Specify one with --name: {}",
                names.join(", ")
            ),
            json,
        );
        return None;
    }

    output::print_error("No runs found. Start one with `veld start`.", json);
    None
}

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
