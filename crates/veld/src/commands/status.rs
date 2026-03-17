use veld_core::config;
use veld_core::process;
use veld_core::state::{NodeStatus, ProjectState, RunStatus};

use crate::output;

/// `veld status [--name <n>] [--outputs] [--json]`
pub async fn run(name: Option<String>, show_outputs: bool, json: bool) -> i32 {
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
        Some(r) => r.clone(),
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), json);
            return 1;
        }
    };

    // Check PID liveness for each node and compute effective statuses.
    let effective_statuses = compute_effective_statuses(&run_state);

    if json {
        // Build a modified run state with effective statuses for JSON output.
        let mut run_for_json = run_state.clone();
        for (key, effective) in &effective_statuses {
            if let Some(ns) = run_for_json.nodes.get_mut(*key) {
                ns.status = effective.clone();
            }
        }
        // If any node is dead, mark the run as degraded (failed).
        if effective_statuses
            .values()
            .any(|s| matches!(s, NodeStatus::Failed))
            && run_for_json.status == RunStatus::Running
        {
            run_for_json.status = RunStatus::Failed;
        }
        match serde_json::to_string_pretty(&run_for_json) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                output::print_error(&format!("JSON serialization failed: {e}"), json);
                return 1;
            }
        }
    } else {
        // Check if any nodes are dead.
        let has_dead = effective_statuses.values().any(|s| {
            matches!(s, NodeStatus::Failed)
                && run_state
                    .nodes
                    .values()
                    .any(|ns| ns.status == NodeStatus::Healthy || ns.status == NodeStatus::Starting)
        });

        let run_status_display = if has_dead && run_state.status == RunStatus::Running {
            output::red("degraded")
        } else {
            format_run_status(&run_state.status)
        };

        println!(
            "{} {}",
            output::bold("Environment:"),
            output::cyan(run_name),
        );
        println!("{} {}", output::bold("State:"), run_status_display,);
        println!();

        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut node_keys: Vec<&String> = run_state.nodes.keys().collect();
        node_keys.sort();
        for key in &node_keys {
            let ns = &run_state.nodes[*key];
            let effective = effective_statuses.get(key.as_str()).unwrap_or(&ns.status);
            let status_str = if *effective != ns.status {
                // The process died — show "dead" instead of the stored status.
                format!("{} {}", output::cross(), output::red("dead"))
            } else {
                format_node_status(&ns.status)
            };
            rows.push(vec![
                ns.node_name.clone(),
                ns.variant.clone(),
                status_str,
                ns.url.clone().unwrap_or_default(),
            ]);
        }

        output::print_table(&["NODE", "VARIANT", "STATUS", "URL"], &rows);

        // Show outputs per node when --outputs is passed.
        if show_outputs {
            println!();
            println!("{}", output::bold("Outputs:"));
            let mut any = false;
            for key in &node_keys {
                let ns = &run_state.nodes[*key];
                if ns.outputs.is_empty() {
                    continue;
                }
                any = true;
                println!();
                println!(
                    "  {}",
                    output::cyan(&format!("{}:{}", ns.node_name, ns.variant))
                );
                let mut okeys: Vec<&String> = ns.outputs.keys().collect();
                okeys.sort();
                for okey in okeys {
                    let val = if ns.sensitive_keys.contains(okey) {
                        "***".to_owned()
                    } else {
                        ns.outputs[okey].clone()
                    };
                    println!("    {} = {}", output::dim(okey), val);
                }
            }
            if !any {
                println!("  {}", output::dim("No outputs recorded."));
            }
        }
    }

    0
}

/// Check PID liveness for each node and return effective statuses.
/// If a node is supposedly running but the process is dead, mark it as Failed.
fn compute_effective_statuses(
    run_state: &veld_core::state::RunState,
) -> std::collections::HashMap<&str, NodeStatus> {
    let mut result = std::collections::HashMap::new();
    for (key, ns) in &run_state.nodes {
        let effective = match ns.status {
            NodeStatus::Healthy | NodeStatus::Starting | NodeStatus::HealthChecking => {
                if let Some(pid) = ns.pid {
                    if process::is_alive(pid) {
                        ns.status.clone()
                    } else {
                        NodeStatus::Failed
                    }
                } else {
                    ns.status.clone()
                }
            }
            _ => ns.status.clone(),
        };
        result.insert(key.as_str(), effective);
    }
    result
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
