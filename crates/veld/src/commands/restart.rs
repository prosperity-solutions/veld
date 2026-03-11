use veld_core::graph;
use veld_core::orchestrator::Orchestrator;
use veld_core::state::ProjectState;

use crate::output;

/// `veld restart [--name <n>] [--debug]`
pub async fn run(name: Option<String>, _debug: bool) -> i32 {
    if !super::require_setup(false).await {
        return 1;
    }

    let Some((config_path, config)) = super::load_config(false) else {
        return 1;
    };

    let run_name = name.as_deref().unwrap_or("default");

    output::print_info(&format!("Restarting environment '{run_name}'..."));

    let mut orchestrator = Orchestrator::new(config_path.clone(), config.clone());

    // First stop the existing run.
    if let Err(e) = orchestrator.stop(run_name).await {
        output::print_error(&format!("Failed to stop '{run_name}': {e}"), false);
        return 1;
    }

    // Re-read state to get the selections that were used.
    let project_state = match ProjectState::load(&orchestrator.project_root) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), false);
            return 1;
        }
    };

    let run_state = match project_state.get_run(run_name) {
        Some(r) => r,
        None => {
            output::print_error(&format!("Run '{run_name}' not found after stop."), false);
            return 1;
        }
    };

    // Reconstruct selections from the node states.
    let selections: Vec<veld_core::graph::NodeSelection> = run_state
        .nodes
        .values()
        .map(|ns| graph::NodeSelection {
            node: ns.node_name.clone(),
            variant: ns.variant.clone(),
        })
        .collect();

    // Start again with a fresh orchestrator.
    let mut orchestrator = Orchestrator::new(config_path, config);

    match orchestrator.start(&selections, run_name).await {
        Ok(new_run) => {
            output::print_success(&format!("Environment '{run_name}' restarted."));

            let urls: Vec<(&str, &str)> = new_run
                .nodes
                .values()
                .filter_map(|ns| ns.url.as_ref().map(|u| (ns.node_name.as_str(), u.as_str())))
                .collect();

            if !urls.is_empty() {
                println!();
                for (node, url) in &urls {
                    println!("  {} {}", output::cyan(node), url);
                }
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to restart '{run_name}': {e}"), false);
            1
        }
    }
}
