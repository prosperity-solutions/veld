use veld_core::orchestrator::{Orchestrator, StopResult};

use crate::output;

/// `veld stop [--name <n>] [--all]`
pub async fn run(name: Option<String>, all: bool) -> i32 {
    let Some((config_path, config)) = super::load_config(false) else {
        return 1;
    };

    let mut orchestrator = Orchestrator::new(config_path, config);

    let project_state = match veld_core::state::ProjectState::load(&orchestrator.project_root) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), false);
            return 1;
        }
    };

    if all {
        let run_names: Vec<String> = project_state.runs.keys().cloned().collect();
        if run_names.is_empty() {
            output::print_info("No runs to stop.");
            return 0;
        }
        let mut stopped = 0;

        for rn in &run_names {
            match orchestrator.stop(rn).await {
                Ok(_) => stopped += 1,
                Err(e) => {
                    output::print_error(&format!("Failed to stop '{rn}': {e}"), false);
                }
            }
        }

        output::print_success(&format!("Stopped {stopped} environment(s)."));
        0
    } else {
        let run_name = match super::resolve_run_name(name, &project_state, false, false) {
            Some(n) => n,
            None => return 1,
        };
        let run_name = run_name.as_str();

        match orchestrator.stop(run_name).await {
            Ok(StopResult::Stopped) => {
                output::print_success(&format!("Environment '{run_name}' stopped."));
                0
            }
            Ok(StopResult::AlreadyStopped) => {
                output::print_info(&format!("Environment '{run_name}' is already stopped."));
                0
            }
            Err(e) => {
                output::print_error(&format!("Failed to stop '{run_name}': {e}"), false);
                1
            }
        }
    }
}
