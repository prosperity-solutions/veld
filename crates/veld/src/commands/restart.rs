use veld_core::graph;
use veld_core::orchestrator::Orchestrator;
use veld_core::state::ProjectState;

use crate::output;

/// `veld restart [--name <n>] [--debug]`
pub async fn run(name: Option<String>, debug: bool) -> i32 {
    if !super::require_setup(false).await {
        return 1;
    }

    let Some((config_path, config)) = super::load_config(false) else {
        return 1;
    };

    let mut orchestrator = Orchestrator::new(config_path.clone(), config.clone());

    let project_state = match ProjectState::load(&orchestrator.project_root) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), false);
            return 1;
        }
    };

    let run_name = match super::resolve_run_name(name, &project_state, false, false) {
        Some(n) => n,
        None => return 1,
    };
    let run_name = run_name.as_str();

    // Save the selections BEFORE stopping (stop removes the run from state).
    let selections: Vec<graph::NodeSelection> = match project_state.get_run(run_name) {
        Some(run_state) => run_state
            .nodes
            .values()
            .map(|ns| graph::NodeSelection {
                node: ns.node_name.clone(),
                variant: ns.variant.clone(),
            })
            .collect(),
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), false);
            return 1;
        }
    };

    output::print_info(&format!("Restarting environment '{run_name}'..."));

    // Stop the existing run (this removes it from state).
    if let Err(e) = orchestrator.stop(run_name).await {
        output::print_error(&format!("Failed to stop '{run_name}': {e}"), false);
        return 1;
    }

    // Start again with a fresh orchestrator.
    let mut orchestrator = Orchestrator::new(config_path, config);
    orchestrator.set_debug(debug);

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
