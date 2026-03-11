use veld_core::config;
use veld_core::state::{NodeStatus, ProjectState, RunStatus};

use crate::output;

/// `veld status [--name <n>] [--json]`
pub async fn run(name: Option<String>, json: bool) -> i32 {
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

    let run_name = match super::resolve_run_name(name, &project_state, true, json) {
        Some(n) => n,
        None => return 1,
    };
    let run_name = run_name.as_str();

    let run_state = match project_state.get_run(run_name) {
        Some(r) => r,
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), json);
            return 1;
        }
    };

    if json {
        match serde_json::to_string_pretty(&run_state) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                output::print_error(&format!("JSON serialization failed: {e}"), true);
                return 1;
            }
        }
    } else {
        println!(
            "{} {}",
            output::bold("Environment:"),
            output::cyan(run_name),
        );
        println!(
            "{} {}",
            output::bold("State:"),
            format_run_status(&run_state.status),
        );
        println!();

        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut node_keys: Vec<&String> = run_state.nodes.keys().collect();
        node_keys.sort();
        for key in node_keys {
            let ns = &run_state.nodes[key];
            rows.push(vec![
                ns.node_name.clone(),
                ns.variant.clone(),
                format_node_status(&ns.status),
                ns.url.clone().unwrap_or_default(),
            ]);
        }

        output::print_table(&["NODE", "VARIANT", "STATUS", "URL"], &rows);
    }

    0
}

fn format_run_status(status: &RunStatus) -> String {
    match status {
        RunStatus::Running => output::green("running"),
        RunStatus::Starting => output::yellow("starting"),
        RunStatus::Stopping => output::yellow("stopping"),
        RunStatus::Stopped => output::dim("stopped"),
        RunStatus::Failed => output::red("failed"),
    }
}

fn format_node_status(status: &NodeStatus) -> String {
    match status {
        NodeStatus::Healthy => format!("{} {}", output::checkmark(), output::green("healthy")),
        NodeStatus::Starting => format!("{} {}", output::yellow("~"), output::yellow("starting")),
        NodeStatus::HealthChecking => {
            format!(
                "{} {}",
                output::yellow("~"),
                output::yellow("health-checking")
            )
        }
        NodeStatus::Pending => format!("{} {}", output::dim("-"), output::dim("pending")),
        NodeStatus::Stopped => format!("{} {}", output::dim("-"), output::dim("stopped")),
        NodeStatus::Failed => format!("{} {}", output::cross(), output::red("failed")),
        NodeStatus::Skipped => format!("{} {}", output::dim("-"), output::dim("skipped")),
    }
}
