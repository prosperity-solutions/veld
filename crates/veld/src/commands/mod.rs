pub mod config;
pub mod doctor;
pub mod feedback;
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
pub mod ui;
pub mod uninstall;
pub mod update;
pub mod urls;
pub mod version;

use crate::output;

/// Read the setup mode from `~/.veld/setup.json`.
pub fn read_setup_mode() -> Option<String> {
    let path = dirs::home_dir()?.join(".veld").join("setup.json");
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .get("mode")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

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
        1 => {
            let resolved = active_runs[0].clone();
            if !json {
                output::print_info(&format!("Using run '{}' (only active run).", resolved));
            }
            return Some(resolved);
        }
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
        let resolved = project_state.runs.keys().next().unwrap().clone();
        if !json {
            output::print_info(&format!("Using run '{}' (only run).", resolved));
        }
        return Some(resolved);
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
