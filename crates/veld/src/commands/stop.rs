//! `veld stop` — a face of the engine.

use std::sync::Arc;

use veld_engine::api::{
    ActionKind, ActionOutcome, ActionRequest, ActionSecrets, PreflightPlan, PreflightRequest,
    PreflightResponse, SubmitAction,
};

use crate::engine_client::EngineClient;
use crate::output;

/// `veld stop [--name <n>] [--all]`
pub async fn run(name: Option<String>, all: bool) -> i32 {
    let Some(project_root) = super::project_root() else {
        return 1;
    };
    let client = match super::engine().await {
        Ok(c) => Arc::new(c),
        Err(code) => return code,
    };
    stop(&client, &project_root, name, all).await
}

/// Stop `run` — `--attach`'s Ctrl-C.
pub(super) async fn stop_run(
    client: &Arc<EngineClient>,
    project_root: &std::path::Path,
    run: &str,
) -> i32 {
    stop(client, project_root, Some(run.to_owned()), false).await
}

async fn stop(
    client: &Arc<EngineClient>,
    project_root: &std::path::Path,
    name: Option<String>,
    all: bool,
) -> i32 {
    let ready = match client
        .preflight(&PreflightRequest {
            project_root: project_root.to_path_buf(),
            plan: PreflightPlan::Stop { name, all },
            var_flags: Vec::new(),
            interactive: false,
        })
        .await
    {
        Ok(PreflightResponse::Ready(r)) => *r,
        // Nothing to stop is not a failure.
        Ok(PreflightResponse::Refused { message, .. }) if message == "No runs to stop." => {
            output::print_info(&message);
            return 0;
        }
        Ok(PreflightResponse::Refused { message, .. }) => {
            output::print_error(&message, false);
            return 1;
        }
        Ok(_) => {
            output::print_error(
                "The daemon answered in a way this veld does not know — update veld.",
                false,
            );
            return 1;
        }
        Err(e) => return super::client_error(&e),
    };
    for note in &ready.notices {
        output::print_info(note);
    }
    let single = match &ready.action {
        ActionKind::Stop { run } => Some(run.clone()),
        _ => None,
    };
    let action_id = match client
        .submit(&SubmitAction {
            request: ActionRequest {
                project_root: project_root.to_path_buf(),
                kind: ready.action.clone(),
                debug: false,
            },
            // A teardown hook may read a declared env var too.
            secrets: ActionSecrets {
                env: super::start::caller_env(&ready.env_names),
                var_answers: Vec::new(),
            },
        })
        .await
    {
        Ok(id) => id,
        Err(e) => return super::client_error(&e),
    };
    let outcome = match super::start::drive(client, &action_id, super::start::Drive::Quiet).await {
        Ok(o) => o,
        Err(code) => return code,
    };
    match (outcome, single) {
        (ActionOutcome::Stopped { already: true, .. }, Some(run)) => {
            output::print_info(&format!("Environment '{run}' is already stopped."));
            0
        }
        (ActionOutcome::Stopped { .. }, Some(run)) => {
            output::print_success(&format!("Environment '{run}' stopped."));
            0
        }
        (ActionOutcome::Stopped { stopped, .. }, None) => {
            output::print_success(&format!("Stopped {} environment(s).", stopped.len()));
            0
        }
        (other, _) => super::start::finish(other),
    }
}
