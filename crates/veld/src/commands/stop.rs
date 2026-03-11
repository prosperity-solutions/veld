use veld_core::orchestrator::Orchestrator;

use crate::output;

/// `veld stop [--name <n>] [--all]`
pub async fn run(name: Option<String>, all: bool) -> i32 {
    if !super::require_setup(false).await {
        return 1;
    }

    let Some((config_path, config)) = super::load_config(false) else {
        return 1;
    };

    let mut orchestrator = Orchestrator::new(config_path, config);

    if all {
        // Stop all runs by loading state and iterating.
        let project_state = match veld_core::state::ProjectState::load(&orchestrator.project_root) {
            Ok(s) => s,
            Err(e) => {
                output::print_error(&format!("Failed to load state: {e}"), false);
                return 1;
            }
        };

        let run_names: Vec<String> = project_state.runs.keys().cloned().collect();
        let mut stopped = 0;

        for rn in &run_names {
            match orchestrator.stop(rn).await {
                Ok(()) => stopped += 1,
                Err(e) => {
                    output::print_error(&format!("Failed to stop '{rn}': {e}"), false);
                }
            }
        }

        output::print_success(&format!("Stopped {stopped} environment(s)."));
        0
    } else {
        let run_name = name.as_deref().unwrap_or("default");

        match orchestrator.stop(run_name).await {
            Ok(()) => {
                output::print_success(&format!("Environment '{run_name}' stopped."));
                0
            }
            Err(e) => {
                output::print_error(&format!("Failed to stop '{run_name}': {e}"), false);
                1
            }
        }
    }
}
