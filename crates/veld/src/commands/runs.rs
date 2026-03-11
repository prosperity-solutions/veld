use veld_core::config;
use veld_core::state::{ProjectState, RunStatus};

use crate::output;

/// `veld runs [--all] [--name <n>] [--json]`
pub async fn list(all: bool, name: Option<&str>, json: bool) -> i32 {
    if !super::require_setup(json).await {
        return 1;
    }

    let Some((config_path, _cfg)) = super::load_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let project_state = match ProjectState::load(&project_root) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), json);
            return 1;
        }
    };

    let mut runs: Vec<(&String, &veld_core::state::RunState)> = project_state.runs.iter().collect();

    // Filter by name if given.
    if let Some(filter_name) = name {
        runs.retain(|(n, _)| *n == filter_name);
    }

    // Unless --all, only show active (non-stopped) runs.
    if !all {
        runs.retain(|(_, r)| r.status != RunStatus::Stopped);
    }

    runs.sort_by_key(|(n, _)| (*n).clone());

    if json {
        let payload: Vec<serde_json::Value> = runs
            .iter()
            .map(|(_, r)| {
                let mut node_keys: Vec<String> = r.nodes.keys().cloned().collect();
                node_keys.sort();
                serde_json::json!({
                    "name": r.name,
                    "status": format!("{:?}", r.status).to_lowercase(),
                    "created_at": r.created_at.to_rfc3339(),
                    "nodes": node_keys,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else if runs.is_empty() {
        output::print_info("No runs found.");
    } else {
        let rows: Vec<Vec<String>> = runs
            .iter()
            .map(|(_, r)| {
                let node_list: Vec<String> = r.nodes.keys().cloned().collect();
                vec![
                    r.name.clone(),
                    format!("{:?}", r.status).to_lowercase(),
                    r.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    node_list.join(", "),
                ]
            })
            .collect();
        output::print_table(&["NAME", "STATE", "STARTED", "NODES"], &rows);
    }

    0
}

/// `veld runs purge --name <n>`
pub async fn purge(name: &str) -> i32 {
    if !super::require_setup(false).await {
        return 1;
    }

    let Some((config_path, _cfg)) = super::load_config(false) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let mut project_state = match ProjectState::load(&project_root) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), false);
            return 1;
        }
    };

    if project_state.runs.remove(name).is_none() {
        output::print_error(&format!("Run '{name}' not found."), false);
        return 1;
    }

    // Remove log directory.
    let log_dir = veld_core::logging::log_dir(&project_root, name);
    if log_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&log_dir) {
            output::print_error(
                &format!("Failed to remove logs at {}: {e}", log_dir.display()),
                false,
            );
        }
    }

    if let Err(e) = project_state.save(&project_root) {
        output::print_error(&format!("Failed to save state: {e}"), false);
        return 1;
    }

    output::print_success(&format!("Purged run '{name}'."));
    0
}
