pub mod action;
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
pub mod share;
pub mod start;
pub mod status;
pub mod stop;
pub mod ui;
pub mod uninstall;
pub mod update;
pub mod urls;
pub mod version;

use crate::output;

/// Open the central veld database. On failure prints an error and returns
/// `None`.
pub fn open_db(json: bool) -> Option<veld_core::db::Db> {
    match veld_core::db::Db::open() {
        Ok(db) => Some(db),
        Err(e) => {
            output::print_error(&format!("Failed to open veld database: {e}"), json);
            None
        }
    }
}

/// Read the setup mode from `~/.veld/setup.json`.
/// Delegates to the shared implementation in `veld-core` so the two never drift.
pub fn read_setup_mode() -> Option<String> {
    veld_core::setup::read_setup_mode()
}

/// Resolve the environment name to use. If `name` is given, use it directly.
/// Otherwise look at the project state, two-tiered: if exactly one environment
/// has a *live* run (starting/running/stopping), use that; only when zero are
/// live, fall back to a sole environment (stopped ones persist as history now,
/// so `veld restart`/`veld stop` can find last night's crashed environment).
/// The tiers are deliberately not collapsed — a crashed `dev` next to a
/// running `staging` must not turn a bare `veld stop` into an ambiguity error.
pub fn resolve_run_name(
    name: Option<String>,
    project_state: &veld_core::state::ProjectState,
    include_stopped: bool,
    json: bool,
) -> Option<String> {
    if let Some(n) = name {
        return Some(n);
    }

    // Tier 1: environments with a live run.
    let live: Vec<&String> = project_state
        .runs
        .iter()
        .filter(|(_, r)| r.is_live())
        .map(|(name, _)| name)
        .collect();

    match live.len() {
        1 => {
            let resolved = live[0].clone();
            if !json {
                output::print_info(&format!(
                    "Using environment '{resolved}' (only live environment)."
                ));
            }
            return Some(resolved);
        }
        n if n > 1 => {
            let mut names: Vec<&str> = live.iter().map(|s| s.as_str()).collect();
            names.sort();
            output::print_error(
                &format!(
                    "Multiple live environments found. Specify one with --name: {}",
                    names.join(", ")
                ),
                json,
            );
            return None;
        }
        _ => {}
    }

    // Tier 2: nothing live — fall back to a sole environment (its latest run
    // is history at this point).
    if include_stopped && project_state.runs.len() == 1 {
        let resolved = project_state.runs.keys().next().unwrap().clone();
        if !json {
            output::print_info(&format!(
                "Using environment '{resolved}' (only environment)."
            ));
        }
        return Some(resolved);
    }

    if include_stopped && project_state.runs.len() > 1 {
        let mut names: Vec<&str> = project_state.runs.keys().map(|s| s.as_str()).collect();
        names.sort();
        output::print_error(
            &format!(
                "Multiple environments found. Specify one with --name: {}",
                names.join(", ")
            ),
            json,
        );
        return None;
    }

    output::print_error("No environments found. Start one with `veld start`.", json);
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
