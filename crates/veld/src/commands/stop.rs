use veld_core::orchestrator::{Orchestrator, StopResult};
use veld_core::share::DaemonClient;

use crate::output;

/// Best-effort: stop any P2P shares tied to a run so they don't outlive it.
/// Silent on failure (daemon may be down, or there may be no shares).
async fn unshare_run(run_id: &str) {
    let _ = DaemonClient::new().unshare_run(run_id).await;
}

/// `veld stop [--name <n>] [--all]`
pub async fn run(name: Option<String>, all: bool) -> i32 {
    let Some((config_path, config)) = super::load_config(false) else {
        return 1;
    };

    let mut orchestrator = match Orchestrator::new(config_path, config) {
        Ok(o) => o,
        Err(e) => {
            output::print_error(&format!("Failed to initialize: {e}"), false);
            return 1;
        }
    };

    let project_state = match orchestrator
        .db
        .load_project_state(&orchestrator.project_root)
    {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), false);
            return 1;
        }
    };

    if all {
        // Capture run ids before stopping so we can unshare afterwards.
        let run_ids: Vec<String> = project_state
            .runs
            .values()
            .map(|r| r.run_id.to_string())
            .collect();
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

        for run_id in &run_ids {
            unshare_run(run_id).await;
        }

        output::print_success(&format!("Stopped {stopped} environment(s)."));
        0
    } else {
        let run_name = match super::resolve_run_name(name, &project_state, false, false) {
            Some(n) => n,
            None => return 1,
        };
        let run_name = run_name.as_str();
        // Capture the run id before stopping (state may change afterwards).
        let run_id = project_state
            .runs
            .get(run_name)
            .map(|r| r.run_id.to_string());

        match orchestrator.stop(run_name).await {
            Ok(StopResult::Stopped) => {
                if let Some(run_id) = &run_id {
                    unshare_run(run_id).await;
                }
                output::print_success(&format!("Environment '{run_name}' stopped."));
                0
            }
            Ok(StopResult::AlreadyStopped) => {
                if let Some(run_id) = &run_id {
                    unshare_run(run_id).await;
                }
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
