//! `veld restart` — a face of the engine.

use std::sync::Arc;

use veld_engine::api::{
    ActionOutcome, ActionRequest, ActionSecrets, PreflightPlan, PreflightRequest,
    PreflightResponse, SubmitAction,
};

use crate::output;

/// `veld restart [--name <n>] [--debug]`
pub async fn run(name: Option<String>, debug: bool) -> i32 {
    let Some(project_root) = super::project_root() else {
        return 1;
    };
    let client = match super::engine().await {
        Ok(c) => Arc::new(c),
        Err(code) => return code,
    };
    let plan = PreflightPlan::Restart { name };
    // **Before the stop, not after**: a missing answer discovered inside the
    // engine's second start would take the environment down and leave the user
    // with an error. The engine re-checks, but this is where a human can answer.
    let ready = match client
        .preflight(&PreflightRequest {
            project_root: project_root.clone(),
            plan: plan.clone(),
            var_flags: Vec::new(),
            interactive: false,
        })
        .await
    {
        Ok(PreflightResponse::Ready(r)) => *r,
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
    // `veld restart` has no `--var`, so the refusal must not offer one.
    let Some(answers) =
        super::start::gather_answers(&client, &project_root, &ready.unanswered, false).await
    else {
        return 1;
    };
    super::start::warn_stranded(&client, &project_root, plan, &[], &ready, &answers).await;

    let run_name = ready.action.run().unwrap_or_default().to_owned();
    let action_id = match client
        .submit(&SubmitAction {
            request: ActionRequest {
                project_root: project_root.clone(),
                kind: ready.action.clone(),
                debug,
            },
            secrets: ActionSecrets {
                env: super::start::caller_env(&ready.env_names),
                var_answers: answers,
            },
        })
        .await
    {
        Ok(id) => id,
        Err(e) => return super::client_error(&e),
    };
    match super::start::drive(&client, &action_id, super::start::Drive::Quiet).await {
        Ok(ActionOutcome::Started { run }) => {
            output::print_success(&format!("Environment '{run_name}' restarted."));
            let urls: Vec<(&str, &str)> = run
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
        Ok(ActionOutcome::Failed { message, .. }) => {
            let reason = message.strip_prefix("Startup failed: ").unwrap_or(&message);
            output::print_error(&format!("Failed to restart '{run_name}': {reason}"), false);
            1
        }
        Ok(other) => super::start::finish(other),
        Err(code) => code,
    }
}
