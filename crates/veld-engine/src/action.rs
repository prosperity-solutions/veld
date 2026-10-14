//! Run one [`ActionRequest`] end to end.
//!
//! This is what used to be the body of `veld start`, `veld stop` and `veld
//! restart`, minus everything that drew on a terminal. It runs inside the engine
//! worker (`veld-daemon --engine-worker`), one process per action, so a daemon
//! restart or a `veld update` mid-start does not cut a run in half.
//!
//! It reports through [`EventSink`] and nothing else: progress, notices,
//! `--oneshot` output, and finally one [`ActionOutcome`]. Cancellation (the face's
//! Ctrl-C) arrives as SIGINT to the worker and is observed with
//! `tokio::signal::ctrl_c`, exactly where the CLI observed it — so the
//! orchestrator's own interrupt handling (`run_command_streaming` kills a
//! `--oneshot` terminal node's group on SIGINT) works unchanged.

use std::sync::Arc;

use tokio::sync::mpsc;
use veld_core::db::{Db, LogFilter, LogStream};
use veld_core::graph::{self, NodeSelection};
use veld_core::state::{EndDetail, EndReason, StartOrigin};

use crate::api::{
    ActionKind, ActionOutcome, ActionRequest, ActionSecrets, EngineEvent, FailureKind, NoticeLevel,
    OutputStream,
};
use crate::host::{EngineHost, share_release_notice};
use crate::orchestrator::{Orchestrator, OrchestratorError, StopResult};
use crate::preflight::{apply_answers, load_config, run_selections};
use crate::progress::ProgressEvent;

/// Where an action's events go. The worker writes them to the action's event
/// log; a test collects them.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: EngineEvent);
}

impl<F: Fn(EngineEvent) + Send + Sync> EventSink for F {
    fn emit(&self, event: EngineEvent) {
        self(event)
    }
}

fn notice(sink: &dyn EventSink, level: NoticeLevel, message: impl Into<String>) {
    sink.emit(EngineEvent::Notice {
        message: message.into(),
        level,
    });
}

fn failed(message: impl Into<String>) -> ActionOutcome {
    ActionOutcome::Failed {
        message: message.into(),
        kind: FailureKind::Error,
        failed_node: None,
    }
}

/// Resolves once the face has asked to cancel: SIGINT seen by a `ctrl_c`
/// listener, or one the host remembered from before any listener existed (a
/// signal between a check and the first poll of a fresh `ctrl_c()` is otherwise
/// lost — tokio's listener starts at the current signal version).
async fn cancelled(host: &dyn EngineHost) {
    let remembered = async {
        loop {
            if host.cancel_requested() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = remembered => {}
    }
}

/// Run `request`. The caller has already installed `secrets.env` with
/// [`veld_core::values::set_caller_env`] — once per process, before anything
/// resolves a value.
pub async fn run(
    request: ActionRequest,
    secrets: ActionSecrets,
    host: Arc<dyn EngineHost>,
    sink: Arc<dyn EventSink>,
) -> ActionOutcome {
    let (config_path, cfg) = match load_config(&request.project_root) {
        Ok(pair) => pair,
        Err(e) => return failed(e),
    };
    let ctx = Ctx {
        config_path,
        cfg,
        debug: request.debug,
        secrets,
        host,
        sink,
    };
    match request.kind {
        ActionKind::Start {
            run,
            selections,
            origin_preset,
            oneshot,
            all_logs,
        } => start(&ctx, &run, &selections, origin_preset, oneshot, all_logs).await,
        ActionKind::Stop { run } => stop(&ctx, &[run]).await,
        ActionKind::StopAll => match Db::open().and_then(|db| {
            db.load_project_state(&veld_core::config::project_root(&ctx.config_path))
        }) {
            Ok(state) => {
                let mut runs: Vec<String> = state.runs.keys().cloned().collect();
                runs.sort();
                stop(&ctx, &runs).await
            }
            Err(e) => failed(format!("Failed to load state: {e}")),
        },
        ActionKind::Restart { run } => restart(&ctx, &run).await,
        ActionKind::Unknown => failed(
            "This veld daemon does not know that action — update veld so the daemon and the \
             command that asked are the same version.",
        ),
        ActionKind::Action { run, action, node } => {
            crate::run_action::run_unattended(&ctx.config_path, ctx.cfg, &run, &action, node).await
        }
    }
}

struct Ctx {
    config_path: std::path::PathBuf,
    cfg: veld_core::config::VeldConfig,
    debug: bool,
    secrets: ActionSecrets,
    host: Arc<dyn EngineHost>,
    sink: Arc<dyn EventSink>,
}

impl Ctx {
    fn orchestrator(&self) -> Result<Orchestrator, ActionOutcome> {
        let mut o = Orchestrator::new(self.config_path.clone(), self.cfg.clone())
            .map_err(|e| failed(format!("Failed to initialize: {e}")))?;
        o.set_debug(self.debug);
        o.set_host(Arc::clone(&self.host));
        apply_answers(&mut o, &self.secrets.var_answers);
        Ok(o)
    }

    /// Wire the orchestrator's progress channel into the sink. Returns the task
    /// that forwards; await it after `close_progress_sender` so nothing is lost.
    fn forward_progress(&self, o: &mut Orchestrator) -> tokio::task::JoinHandle<()> {
        let (tx, mut rx) = mpsc::unbounded_channel::<ProgressEvent>();
        o.set_progress_sender(tx);
        let sink = Arc::clone(&self.sink);
        tokio::spawn(async move {
            while let Some(progress) = rx.recv().await {
                sink.emit(EngineEvent::Progress { progress });
            }
        })
    }

    fn observe_stats(&self, o: &mut Orchestrator, run: &str) {
        if let Some(observer) =
            self.host
                .command_stats_observer(&o.db, &o.project_root.clone(), run)
        {
            o.with_step_observer(observer);
        }
    }
}

/// A start's outcome — always through here, so the run is always [`redacted`].
fn started(run: veld_core::state::RunState) -> ActionOutcome {
    ActionOutcome::Started {
        run: Box::new(redacted(run)),
    }
}

/// The run as a face may see it: sensitive outputs masked.
///
/// The outcome is stored (`engine_actions.outcome_json`, the `finished` event)
/// and streamed, while the run's own rows keep sensitive outputs encrypted at
/// rest. The in-memory `RunState` holds them in plaintext, so it is masked here,
/// before it leaves the worker — no face needs the value, they print `***`.
fn redacted(mut run: veld_core::state::RunState) -> veld_core::state::RunState {
    for node in run.nodes.values_mut() {
        node.outputs = node.display_outputs();
    }
    run
}

fn parse_selections(
    tokens: &[String],
    cfg: &veld_core::config::VeldConfig,
) -> Result<Vec<NodeSelection>, String> {
    let parsed: Result<Vec<NodeSelection>, _> =
        tokens.iter().map(|s| graph::parse_selection(s)).collect();
    parsed
        .and_then(|p| graph::resolve_selections(&p, cfg))
        .map_err(|e| e.to_string())
}

/// Refuse when a var has no answer. Preflight asked already; this catches a
/// veld.json that changed in between — the worker never prompts.
fn check_answers(o: &Orchestrator, selections: &[NodeSelection]) -> Result<(), ActionOutcome> {
    match o.unanswered_vars(selections) {
        Ok(missing) if missing.is_empty() => Ok(()),
        Ok(missing) => Err(ActionOutcome::Failed {
            message: format!(
                "This project needs values for {} on this machine, and nothing answered them. \
                 Run the command again from a terminal, or set them with `veld config set`.",
                missing
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            kind: FailureKind::NeedsAnswers,
            failed_node: None,
        }),
        Err(e) => Err(failed(e.to_string())),
    }
}

fn startup_failure(e: &OrchestratorError) -> String {
    match e {
        OrchestratorError::SetupFailed {
            failure_message: Some(msg),
            ..
        } => format!("Startup failed: {msg}"),
        other => format!("Startup failed: {other}"),
    }
}

async fn start(
    ctx: &Ctx,
    run: &str,
    tokens: &[String],
    origin_preset: Option<String>,
    oneshot: Option<String>,
    all_logs: bool,
) -> ActionOutcome {
    let selections = match parse_selections(tokens, &ctx.cfg) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let terminal = match oneshot.as_deref().map(graph::parse_selection) {
        None => None,
        Some(Ok(sel)) => Some(sel),
        Some(Err(e)) => return failed(e.to_string()),
    };
    let mut o = match ctx.orchestrator() {
        Ok(o) => o,
        Err(out) => return out,
    };
    if let Err(out) = check_answers(&o, &selections) {
        return out;
    }
    o.set_terminal_node(terminal.clone());
    if terminal.is_some() {
        let sink = Arc::clone(&ctx.sink);
        let sink_err = Arc::clone(&ctx.sink);
        o.set_terminal_echo(veld_core::process::StreamEcho {
            stdout: Arc::new(move |line: &str| {
                sink.emit(EngineEvent::Output {
                    stream: OutputStream::Stdout,
                    line: line.to_owned(),
                })
            }),
            stderr: Arc::new(move |line: &str| {
                sink_err.emit(EngineEvent::Output {
                    stream: OutputStream::Stderr,
                    line: line.to_owned(),
                })
            }),
        });
    }
    ctx.observe_stats(&mut o, run);
    let forward = ctx.forward_progress(&mut o);

    // A cancel that arrived before the listener below existed.
    if ctx.host.cancel_requested() {
        return ActionOutcome::Cancelled;
    }
    let origin = Some(StartOrigin::new(origin_preset, &selections));
    let result = tokio::select! {
        result = o.start(&selections, run, origin) => result,
        _ = cancelled(&*ctx.host) => {
            o.close_progress_sender();
            let _ = forward.await;
            notice(&*ctx.sink, NoticeLevel::Info,
                "Interrupted — stopping partially started environment...");
            match o.stop(run).await {
                Ok(_) => notice(&*ctx.sink, NoticeLevel::Info,
                    format!("Environment '{run}' cleaned up.")),
                Err(e) => notice(&*ctx.sink, NoticeLevel::Error, format!("Cleanup failed: {e}")),
            }
            return ActionOutcome::Cancelled;
        }
    };

    let run_state = match result {
        Ok(state) => state,
        Err(e) => {
            // Best-effort teardown, as `veld start` always did: the orchestrator's
            // own failure paths reap node hooks but not the project's `teardown`
            // steps, so a setup step that created a network or a container
            // before a later one failed would leave it behind — and the retry
            // fails on its name. With progress still open, so a skipped hook says so.
            //
            // Not when the run was superseded: a concurrent start of the same
            // name replaced it, and `stop(run)` would stop that newer run.
            if !matches!(e, OrchestratorError::Superseded(_)) {
                let _ = o.stop(run).await;
            }
            o.close_progress_sender();
            let _ = forward.await;
            let failed_node = match &e {
                OrchestratorError::NodeFailed { node, variant, .. } => {
                    Some(format!("{node}:{variant}"))
                }
                _ => None,
            };
            return ActionOutcome::Failed {
                message: startup_failure(&e),
                kind: FailureKind::Error,
                failed_node,
            };
        }
    };

    let Some(sel) = terminal else {
        o.close_progress_sender();
        let _ = forward.await;
        return started(run_state);
    };

    // --oneshot: dependencies are up; run the terminal node to completion, tear
    // everything down, and report its code.
    let label = sel.to_string();
    for key in &run_state.execution_order {
        let Some(ns) = run_state.nodes.get(key) else {
            continue;
        };
        if ns.node_name == sel.node && ns.variant == sel.variant {
            continue;
        }
        notice(
            &*ctx.sink,
            NoticeLevel::Info,
            format!(
                "{}:{} {} {}",
                ns.node_name,
                ns.variant,
                format!("{:?}", ns.status).to_lowercase(),
                ns.url.as_deref().unwrap_or("-")
            ),
        );
    }
    notice(
        &*ctx.sink,
        NoticeLevel::Info,
        format!("Running {label} (Ctrl+C to abort)..."),
    );

    let tailer = all_logs.then(|| {
        let db = o.db.clone();
        let root = o.project_root.clone();
        let deps: Vec<(String, String)> = run_state
            .nodes
            .values()
            .filter(|ns| !(ns.node_name == sel.node && ns.variant == sel.variant))
            .map(|ns| (ns.node_name.clone(), ns.variant.clone()))
            .collect();
        let sink = Arc::clone(&ctx.sink);
        let run = run.to_owned();
        tokio::spawn(async move { follow_dep_logs(&db, &deps, &root, &run, &*sink).await })
    });

    // SIGINT during the terminal node is handled inside `run_command_streaming`
    // (kills the node's group, reports 130), so this is not raced against it.
    let result = o.run_terminal(run, &sel).await;
    if let Some(t) = tailer {
        t.abort();
    }
    let exit_code = match result {
        Ok(code) => code,
        Err(e) => {
            notice(
                &*ctx.sink,
                NoticeLevel::Error,
                format!("Failed to run {label}: {e}"),
            );
            // Record the failure before the teardown below finalizes, or history
            // reads `stopped` for a run that failed to execute.
            if let Ok(Some(r)) = o.db.get_run(&o.project_root, run) {
                let detail = EndDetail {
                    exit_code: Some(127),
                    message: Some(format!("terminal node failed to run: {e}")),
                    ..Default::default()
                };
                let _ =
                    o.db.begin_ending(&r.run_id, EndReason::Failed, Some(&detail));
            }
            127
        }
    };

    notice(&*ctx.sink, NoticeLevel::Info, "Tearing down environment...");
    match o.stop(run).await {
        Ok(_) => notice(
            &*ctx.sink,
            NoticeLevel::Info,
            format!("Environment '{run}' torn down."),
        ),
        Err(e) => notice(
            &*ctx.sink,
            NoticeLevel::Error,
            format!("Teardown failed: {e}"),
        ),
    }
    o.close_progress_sender();
    let _ = forward.await;
    ActionOutcome::OneshotExited { exit_code, label }
}

/// Tail dependency server logs until aborted (`--oneshot --all-logs`).
async fn follow_dep_logs(
    db: &Db,
    targets: &[(String, String)],
    project_root: &std::path::Path,
    run: &str,
    sink: &dyn EventSink,
) {
    let mut last_id = db.max_log_id().unwrap_or(0);
    let wanted: std::collections::HashSet<(String, String)> = targets.iter().cloned().collect();
    let filter = LogFilter {
        node: None,
        variant: None,
        streams: Some(vec![LogStream::Server.as_str()]),
        run_id: None,
    };
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tick.tick().await;
        let Ok(rows) = db.logs_after_id(project_root, run, &filter, last_id) else {
            continue;
        };
        for row in rows {
            last_id = row.id;
            let (Some(node), Some(variant)) = (row.node, row.variant) else {
                continue;
            };
            if wanted.contains(&(node.clone(), variant.clone())) {
                sink.emit(EngineEvent::DependencyLog {
                    node,
                    variant,
                    line: row.line,
                });
            }
        }
    }
}

/// Release a stopped run's shares and say so if anything happened.
async fn release_shares(ctx: &Ctx, run_id: &str, whose: &str, now: &str) {
    let release = ctx.host.release_shares(run_id).await;
    if let Some(message) = share_release_notice(&release, whose, now) {
        notice(&*ctx.sink, NoticeLevel::Warning, message);
    }
}

async fn stop(ctx: &Ctx, runs: &[String]) -> ActionOutcome {
    let mut o = match ctx.orchestrator() {
        Ok(o) => o,
        Err(out) => return out,
    };
    let forward = ctx.forward_progress(&mut o);
    let state = o.db.load_project_state(&o.project_root).ok();
    let mut stopped = Vec::new();
    let mut already = runs.len() == 1;
    let mut errors = Vec::new();
    for run in runs {
        let run_id = state
            .as_ref()
            .and_then(|s| s.runs.get(run))
            .map(|r| r.run_id.to_string());
        match o.stop(run).await {
            Ok(result) => {
                if result == StopResult::Stopped {
                    already = false;
                }
                stopped.push(run.clone());
            }
            Err(e) => errors.push(format!("Failed to stop '{run}': {e}")),
        }
        // Shares are keyed by run id and live in the daemon's memory; nothing
        // else releases a cleanly stopped run's.
        if let Some(id) = run_id {
            release_shares(ctx, &id, "the stopped run", "whatever listens there next").await;
        }
    }
    o.close_progress_sender();
    let _ = forward.await;
    if !errors.is_empty() && stopped.is_empty() {
        return failed(errors.join("\n"));
    }
    for e in errors {
        notice(&*ctx.sink, NoticeLevel::Error, e);
    }
    ActionOutcome::Stopped { stopped, already }
}

async fn restart(ctx: &Ctx, run: &str) -> ActionOutcome {
    let mut o = match ctx.orchestrator() {
        Ok(o) => o,
        Err(out) => return out,
    };
    let state = match o.db.load_project_state(&o.project_root) {
        Ok(s) => s,
        Err(e) => return failed(format!("Failed to load state: {e}")),
    };
    let Some(run_state) = state.get_run(run) else {
        return failed(format!("Run '{run}' not found."));
    };
    // The origin travels verbatim or not at all: node rows are the dependency
    // closure, so rebuilding an origin from them would record a wider selection
    // than was asked for and make every restarted run read "redefined since start".
    let selections = run_selections(run_state);
    let origin = run_state
        .graph_snapshot
        .as_ref()
        .and_then(|s| s.started_from.clone());
    let prior_run_id = run_state.run_id.to_string();

    // **Before the stop**: a missing answer discovered inside the second start
    // would take the environment down and leave the user with an error.
    if let Err(out) = check_answers(&o, &selections) {
        return out;
    }
    // Interrupted before anything changed: leave the run as it is.
    if ctx.host.cancel_requested() {
        return ActionOutcome::Cancelled;
    }

    notice(
        &*ctx.sink,
        NoticeLevel::Info,
        format!("Restarting environment '{run}'..."),
    );
    let forward = ctx.forward_progress(&mut o);
    if let Err(e) = o.stop(run).await {
        o.close_progress_sender();
        let _ = forward.await;
        return failed(format!("Failed to stop '{run}': {e}"));
    }
    o.close_progress_sender();
    let _ = forward.await;

    // Between the stop and the start, bounded by the host: the new run rebinds
    // the same ports, so a surviving web share would quietly re-point its public
    // URL at the new processes.
    release_shares(ctx, &prior_run_id, "the previous run", "the restarted run").await;

    // Ctrl-C during the stop phase: the stop ran to its end (it cannot be cut
    // short), and the start the user interrupted does not happen.
    if ctx.host.cancel_requested() {
        notice(
            &*ctx.sink,
            NoticeLevel::Info,
            format!("Environment '{run}' stopped; not started again (interrupted)."),
        );
        return ActionOutcome::Cancelled;
    }

    let mut o = match ctx.orchestrator() {
        Ok(o) => o,
        Err(out) => return out,
    };
    ctx.observe_stats(&mut o, run);
    let forward = ctx.forward_progress(&mut o);
    let result = tokio::select! {
        result = o.start(&selections, run, origin) => result,
        _ = cancelled(&*ctx.host) => {
            o.close_progress_sender();
            let _ = forward.await;
            notice(&*ctx.sink, NoticeLevel::Info,
                "Interrupted — stopping partially started environment...");
            let _ = o.stop(run).await;
            return ActionOutcome::Cancelled;
        }
    };
    o.close_progress_sender();
    let _ = forward.await;
    match result {
        Ok(state) => started(state),
        Err(e) => failed(startup_failure(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The outcome is stored and streamed; a sensitive output must leave the
    /// worker masked, never as the plaintext the in-memory run holds.
    #[test]
    fn a_started_outcome_carries_no_sensitive_output() {
        let mut run = veld_core::state::RunState::new("dev", "proj");
        let mut node = veld_core::state::NodeState::new("db", "local");
        node.outputs.insert("PASSWORD".into(), "hunter2".into());
        node.outputs.insert("HOST".into(), "localhost".into());
        node.sensitive_keys = vec!["PASSWORD".into()];
        run.nodes.insert("db:local".into(), node);

        let outcome = started(run);
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
        assert!(
            json.contains("localhost"),
            "non-sensitive outputs stay: {json}"
        );
    }

    struct Asked;
    impl EngineHost for Asked {
        fn release_shares<'a>(
            &'a self,
            _: &'a str,
        ) -> crate::host::HostFuture<'a, crate::host::ShareRelease> {
            Box::pin(async { crate::host::ShareRelease::NotRunning })
        }
        fn cancel_requested(&self) -> bool {
            true
        }
    }

    /// A cancel the host remembered resolves the wait without any new signal —
    /// the one that arrived before the engine was listening.
    #[tokio::test]
    async fn a_remembered_cancel_ends_the_wait() {
        tokio::time::timeout(std::time::Duration::from_secs(2), cancelled(&Asked))
            .await
            .expect("a remembered cancel must be seen");
    }
}
