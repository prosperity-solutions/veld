//! `veld start` — a face of the engine.
//!
//! Everything that decides something (which nodes, which run, which answers are
//! missing, what to spawn) is the engine's, in the daemon. What is left here is
//! what needs this terminal: the picker, the prompts, and drawing the progress
//! the engine reports.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use veld_core::config::VeldConfig;
use veld_core::graph::NodeSelection;
use veld_engine::api::{
    ActionOutcome, ActionRequest, ActionSecrets, AnswerProvenance, Choice, EngineEvent,
    NoticeLevel, OutputStream, PreflightPlan, PreflightRequest, PreflightResponse, SaveVarAnswers,
    SubmitAction, UnansweredVarWire, VarAnswer,
};
use veld_engine::progress::ProgressEvent;

use crate::engine_client::EngineClient;
use crate::output::{self, is_tty};

/// `veld start [node:variant...] [--preset <n>] [--name <n>] [-a] [--oneshot] [--debug]`
// One parameter per CLI flag, as with `share::run` and `logs::run`. Bundling
// them into a struct would put the flag list in two places and drift.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    selections: Vec<String>,
    preset: Option<String>,
    name: Option<String>,
    attach: bool,
    oneshot: bool,
    all_logs: bool,
    var: Vec<String>,
    debug: bool,
) -> i32 {
    if attach && oneshot {
        output::print_error("--attach and --oneshot cannot be combined.", false);
        return 1;
    }
    let Some(project_root) = super::project_root() else {
        return 1;
    };
    let client = match super::engine().await {
        Ok(c) => c,
        Err(code) => return code,
    };

    // Preflight, with the picker in the loop: a bare `veld start` at a terminal
    // is answered by a human choosing, then asked again with their choice.
    let mut plan_selections = selections;
    let mut plan_preset = preset;
    let interactive = is_tty();
    let ready = loop {
        let req = PreflightRequest {
            project_root: project_root.clone(),
            plan: PreflightPlan::Start {
                selections: plan_selections.clone(),
                preset: plan_preset.clone(),
                name: name.clone(),
                oneshot,
                all_logs,
            },
            var_flags: var.clone(),
            interactive,
        };
        match client.preflight(&req).await {
            Err(e) => return super::client_error(&e),
            Ok(PreflightResponse::Ready(ready)) => break *ready,
            Ok(PreflightResponse::Refused { message, payload }) => {
                match payload {
                    // The unattended no-selection answer is a payload for the
                    // agent that hit it, on stdout.
                    Some(p) => println!("{}", serde_json::to_string_pretty(&p).unwrap_or_default()),
                    None => output::print_error(&message, false),
                }
                return 1;
            }
            Ok(PreflightResponse::Choose(choice)) => match pick(&choice) {
                Some(Picked::Preset(p)) => plan_preset = Some(p),
                Some(Picked::Selections(s)) => plan_selections = s,
                None => return 1,
            },
            Ok(PreflightResponse::Unknown) => {
                output::print_error(
                    "The daemon answered in a way this veld does not know — update veld.",
                    false,
                );
                return 1;
            }
        }
    };
    for notice in &ready.notices {
        eprintln!("{}", output::one_line(notice));
    }

    let Some(mut answers) = gather_answers(&client, &project_root, &ready.unanswered, true).await
    else {
        return 1;
    };
    answers.extend(ready.flag_answers.iter().cloned());
    warn_stranded(
        &client,
        &project_root,
        PreflightPlan::Start {
            selections: plan_selections.clone(),
            preset: plan_preset.clone(),
            name: name.clone(),
            oneshot: false,
            all_logs: false,
        },
        &var,
        &ready,
        &answers,
    )
    .await;

    let run_name = ready.action.run().unwrap_or_default().to_owned();
    let action_id = match client
        .submit(&SubmitAction {
            request: ActionRequest {
                project_root: project_root.clone(),
                kind: ready.action.clone(),
                debug,
            },
            secrets: ActionSecrets {
                env: caller_env(&ready.env_names),
                var_answers: answers,
            },
        })
        .await
    {
        Ok(id) => id,
        Err(e) => return super::client_error(&e),
    };

    eprintln!(
        "{} Starting environment {}...",
        output::bold("veld"),
        output::bold(&format!("'{run_name}'")),
    );
    eprintln!();

    let client = Arc::new(client);
    let mode = if oneshot {
        Drive::Oneshot
    } else {
        Drive::Progress
    };
    let outcome = match drive(&client, &action_id, mode).await {
        Ok(o) => o,
        Err(code) => return code,
    };
    match outcome {
        ActionOutcome::Started { run } => {
            println!();
            print_start_receipt(&run);
            let privileged = super::read_setup_mode().as_deref() == Some("privileged");
            crate::hints::maybe_show_privileged_hint(if privileged { 443 } else { 18443 });
            // Only with a terminal to stream to, as before: without one there is
            // no Ctrl-C to end it, and a CI job passing `-a` must not block.
            if attach && is_tty() {
                return attach_logs(&client, &project_root, &run.name).await;
            }
            0
        }
        ActionOutcome::OneshotExited { exit_code, label } => {
            // An older daemon does not send the label.
            let label = if label.is_empty() {
                "The terminal node".to_owned()
            } else {
                label
            };
            eprintln!();
            if exit_code == 0 {
                eprintln!(
                    "  {} {} completed successfully (exit 0).",
                    output::checkmark(),
                    label
                );
            } else {
                output::print_error(&format!("{label} exited with code {exit_code}."), false);
            }
            exit_code
        }
        ActionOutcome::Failed {
            message,
            failed_node: Some(node),
            ..
        } => {
            output::print_error(&message, false);
            if is_tty() {
                print_failed_node_tail(&client, &project_root, &run_name, &node).await;
            }
            1
        }
        other => finish(other),
    }
}

/// The tail of a failed node's server log, so the user sees what went wrong.
/// Scoped to the run's latest instance by the engine's query, so a node that
/// failed after three lines is not padded out with a previous attempt's.
async fn print_failed_node_tail(
    client: &EngineClient,
    project_root: &std::path::Path,
    run: &str,
    node_key: &str,
) {
    let node = node_key.split(':').next().unwrap_or(node_key);
    let Ok(page) = client
        .get::<veld_engine::api::LogsPage>(
            "/api/engine/logs",
            &[
                ("project_root", project_root.to_string_lossy().into_owned()),
                ("name", run.to_owned()),
                ("node", node.to_owned()),
                ("lines", "20".to_owned()),
                ("source", "server".to_owned()),
            ],
        )
        .await
    else {
        return;
    };
    if page.rows.is_empty() {
        return;
    }
    eprintln!();
    eprintln!(
        "  {}",
        output::dim(&format!("Last log lines from {node_key}:"))
    );
    eprintln!();
    for row in &page.rows {
        eprintln!("    {}", output::dim(&row.line));
    }
    eprintln!();
    eprintln!(
        "  {}",
        output::dim(&format!("Full log: veld logs --name {run} --node {node}"))
    );
}

/// `--attach`: stream the run's logs until Ctrl-C, then stop it.
async fn attach_logs(client: &Arc<EngineClient>, project_root: &std::path::Path, run: &str) -> i32 {
    println!();
    output::print_info("Streaming logs (Ctrl+C to stop)...");
    println!();
    // `veld logs -f` for this run, from now: it returns on Ctrl-C (or when the
    // run ends on its own), and then the run is stopped.
    let _ = project_root;
    super::logs::run(super::logs::LogsOptions {
        name: Some(run.to_owned()),
        node: None,
        lines: 0,
        since: None,
        follow: true,
        json: false,
        // The nodes' own output, as `--attach` always streamed — not veld's
        // internal or client rows.
        source: super::logs::SourceFilter::Server,
        search: None,
        context_lines: 0,
        run: None,
        previous: false,
        all_runs: false,
        time_zone: None,
    })
    .await;
    println!();
    output::print_info("Stopping environment...");
    super::stop::stop_run(client, project_root, run).await;
    0
}

/// Print how an action ended that has nothing more specific to say, and return
/// the exit code.
pub(super) fn finish(outcome: ActionOutcome) -> i32 {
    let code = outcome.exit_code();
    match outcome {
        ActionOutcome::Failed { message, .. } => output::print_error(&message, false),
        ActionOutcome::Unknown => output::print_error(
            "The action ended in a way this veld does not know — update veld.",
            false,
        ),
        _ => {}
    }
    code
}

enum Picked {
    Preset(String),
    Selections(Vec<String>),
}

/// The human half of a bare `veld start`. Reads this checkout's veld.json for
/// the picker's `interpret_pick`, which is a parse of what was typed, not a
/// decision: the choice goes back to the engine's preflight.
fn pick(choice: &Choice) -> Option<Picked> {
    let (_, config) = super::parse_config(false)?;
    if choice.presets.is_empty() {
        let sels = interactive_node_variant_picker(&config)?;
        return Some(Picked::Selections(
            sels.iter().map(ToString::to_string).collect(),
        ));
    }
    let default = choice.presets.iter().find(|p| p.is_default);
    interactive_preset_selector(&config, &choice.presets, default).map(Picked::Preset)
}

/// The values of the env names the engine says veld.json reads — and only
/// those. Nothing else from this terminal reaches a node.
pub(super) fn caller_env(names: &[String]) -> BTreeMap<String, String> {
    names
        .iter()
        .filter_map(|n| std::env::var(n).ok().map(|v| (n.clone(), v)))
        .collect()
}

/// How [`drive`] shows an action's progress.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Drive {
    /// `veld start`: spinners at a terminal, NDJSON on stdout otherwise.
    Progress,
    /// `--oneshot`: as `Progress` but on stderr — stdout is the node's output —
    /// with the engine's notices as plain lines, as the CLI always printed them.
    Oneshot,
    /// `veld stop`/`restart`: only the engine's notices and warnings, as plain
    /// lines on stderr. These never streamed progress, and a script reading their
    /// stdout must not start seeing JSON.
    Quiet,
}

/// Follow an action to its end, drawing what it reports. Ctrl-C asks the engine
/// to cancel (it cleans up what it started and the stream ends `cancelled`); a
/// second Ctrl-C stops waiting.
pub(super) async fn drive(
    client: &Arc<EngineClient>,
    action_id: &str,
    mode: Drive,
) -> Result<ActionOutcome, i32> {
    let oneshot = mode == Drive::Oneshot;
    let (tx, rx) = mpsc::unbounded_channel::<Shown>();
    // In --oneshot mode stdout carries only the terminal node's output, so the
    // startup progress goes to stderr.
    let renderer = tokio::spawn(render_progress(rx, is_tty(), oneshot));
    let follower = {
        let client = Arc::clone(client);
        let id = action_id.to_owned();
        tokio::spawn(async move {
            let mut outcome = None;
            let r = client
                .follow(&id, |env| {
                    match env.event {
                        EngineEvent::Progress { progress } => match (mode, progress) {
                            (Drive::Quiet, ProgressEvent::Notice { message }) => {
                                let _ = tx.send(Shown::Line {
                                    stderr: true,
                                    line: format!("  {} {message}", output::dim("»")),
                                });
                            }
                            (Drive::Quiet, ProgressEvent::Warning { message }) => {
                                let _ = tx.send(Shown::Line {
                                    stderr: true,
                                    line: format!("  {} {message}", output::yellow("!")),
                                });
                            }
                            (Drive::Quiet, _) => {}
                            (_, progress) => {
                                let _ = tx.send(Shown::Progress(progress));
                            }
                        },
                        EngineEvent::Notice { message, level } if mode != Drive::Progress => {
                            let mark = match level {
                                NoticeLevel::Error => output::red("✗"),
                                NoticeLevel::Warning => output::yellow("!"),
                                _ => output::dim("»"),
                            };
                            let _ = tx.send(Shown::Line {
                                stderr: true,
                                line: format!("  {mark} {message}"),
                            });
                        }
                        EngineEvent::Notice { message, level } => {
                            let _ = tx.send(Shown::Progress(match level {
                                NoticeLevel::Warning | NoticeLevel::Error => {
                                    ProgressEvent::Warning { message }
                                }
                                _ => ProgressEvent::Notice { message },
                            }));
                        }
                        EngineEvent::Output { stream, line } => {
                            let _ = tx.send(Shown::Line {
                                stderr: stream == OutputStream::Stderr,
                                line,
                            });
                        }
                        EngineEvent::DependencyLog {
                            node,
                            variant,
                            line,
                        } => {
                            let _ = tx.send(Shown::Line {
                                stderr: true,
                                line: format!(
                                    "{} {}",
                                    output::dim(&format!("{node}:{variant}")),
                                    output::dim(&line)
                                ),
                            });
                        }
                        EngineEvent::Finished { outcome: o } => outcome = Some(o),
                        EngineEvent::Unknown => {}
                    }
                    true
                })
                .await;
            drop(tx);
            r.map(|()| outcome)
        })
    };
    tokio::pin!(follower);
    let mut cancelled = false;
    let result = loop {
        tokio::select! {
            r = &mut follower => break r,
            _ = tokio::signal::ctrl_c() => {
                if cancelled {
                    // Second Ctrl-C: stop waiting. The engine carries on — a
                    // cancelled start cleaning up, a stop finishing.
                    return Err(130);
                }
                cancelled = true;
                eprintln!();
                match client.cancel(action_id).await {
                    Ok(()) => eprintln!(
                        "  {} Interrupted — asking the engine to stop what it started...",
                        output::dim("»")
                    ),
                    // Refused (a stop runs to its end; a worker still starting has
                    // no pid yet): say why and keep waiting.
                    Err(e) => eprintln!(
                        "  {} {e}. Waiting for it — Ctrl+C again to stop waiting.",
                        output::dim("»")
                    ),
                }
            }
        }
    };
    let _ = renderer.await;
    match result {
        Ok(Ok(Some(outcome))) => Ok(outcome),
        Ok(Ok(None)) => {
            output::print_error(
                "The engine ended the action without saying how. Check `veld status`.",
                false,
            );
            Err(1)
        }
        Ok(Err(e)) => Err(super::client_error(&e)),
        Err(_) => Err(1),
    }
}

/// Make sure every machine-overridable var the plan needs has an answer.
///
/// **The rule this exists to enforce: a value nobody chose is never written.**
/// Falling back to a declared `default` stores nothing, and a prompt's answer is
/// stored only when the human says so. The failure that motivated it was a
/// background process taking a default, which then looked exactly like a
/// deliberate choice for as long as the machine lived.
///
/// Each answer carries its provenance: one that reached the database is
/// `Saved`, one the human kept for this run only is `PerRun`. Labelling a saved
/// answer per-run makes the teardown warning fire for a var the user just set —
/// three review rounds found that bug in this seam. `None` means refuse.
pub(super) async fn gather_answers(
    client: &EngineClient,
    project_root: &std::path::Path,
    missing: &[UnansweredVarWire],
    // Whether the caller supports `--var`, so the refusal does not name a flag
    // the subcommand the user typed does not have.
    offer_var_flag: bool,
) -> Option<Vec<VarAnswer>> {
    if missing.is_empty() {
        return Some(Vec::new());
    }
    let missing: Vec<veld_core::values::UnansweredVar> =
        missing.iter().cloned().map(Into::into).collect();

    // Attended means "a human is reading this and can type": both streams the
    // prompt uses must be a terminal, and VELD_NON_INTERACTIVE unset. Not stdout
    // (`--oneshot … > out.json` from a real terminal is attended), and not stdin
    // alone (a process handed a tty stdin but writing stderr to a file must not
    // block on a question nobody can see).
    let attended = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr())
        && std::env::var("VELD_NON_INTERACTIVE")
            .map(|v| v.is_empty())
            .unwrap_or(true);
    if !attended {
        report_unanswered_vars(&missing, offer_var_flag);
        return None;
    }

    let mut answers: Vec<VarAnswer> = Vec::new();
    let mut to_store: Vec<(String, String)> = Vec::new();
    eprintln!();
    eprintln!(
        "{} value(s) this project needs on your machine:",
        missing.len()
    );
    for var in &missing {
        if let Some(stale) = &var.stale {
            eprintln!();
            eprintln!(
                "  {} is no longer one of the allowed values{}.",
                var.name,
                if stale.is_empty() {
                    String::new()
                } else {
                    format!(" (currently \"{stale}\")")
                }
            );
        }
        let Some(value) = prompt_for_var(var) else {
            eprintln!("Cancelled.");
            return None;
        };
        // Ask before writing. A "no" still starts the run — the answer is just
        // not this machine's answer.
        if prompt_yes_no(&format!("  Save `{}` for this machine?", var.name)) {
            to_store.push((var.name.clone(), value.clone()));
        } else {
            eprintln!("  Using it for this run only.");
        }
        answers.push(VarAnswer {
            name: var.name.clone(),
            value,
            provenance: AnswerProvenance::PerRun,
        });
    }

    if !to_store.is_empty() {
        // Counted, so the summary describes what happened rather than what was
        // attempted. Not fatal: the run has its answers; losing the *storage*
        // costs a re-prompt next time, not this start.
        match client
            .save_var_answers(&SaveVarAnswers {
                project_root: project_root.to_path_buf(),
                answers: to_store,
            })
            .await
        {
            Ok(saved) => {
                for (name, e) in &saved.failed {
                    eprintln!("Warning: could not save `{name}` for this machine: {e}");
                }
                for a in &mut answers {
                    if saved.saved.contains(&a.name) {
                        a.provenance = AnswerProvenance::Saved;
                    }
                }
                if !saved.saved.is_empty() {
                    eprintln!(
                        "Saved {} value(s) to {} — shared by every worktree of this project. \
                         Change with `veld config set`, clear with `veld config unset`.",
                        saved.saved.len(),
                        saved.project_id
                    );
                }
            }
            Err(e) => eprintln!(
                "Warning: nothing was saved for this machine ({e}). This run has your answers; \
                 the next one will ask again."
            ),
        }
    }
    eprintln!();
    Some(answers)
}

/// A per-run answer lives only in this action. `veld stop` is another one, so a
/// teardown that needs it will skip its `${vars.*}` steps. Say so now, while
/// `veld config set` is still an option.
pub(super) async fn warn_stranded(
    client: &EngineClient,
    project_root: &std::path::Path,
    plan: PreflightPlan,
    var: &[String],
    ready: &veld_engine::api::Preflight,
    answers: &[VarAnswer],
) {
    let prompt_only: Vec<String> = answers
        .iter()
        .filter(|a| a.provenance == AnswerProvenance::PerRun)
        .filter(|a| !ready.flag_answers.iter().any(|f| f.name == a.name))
        .map(|a| format!("{}={}", a.name, a.value))
        .collect();
    // The engine computed this for the `--var` answers; ask again with the
    // prompt answers the human kept for this run only, which strand the same way.
    let stranded = if prompt_only.is_empty() {
        ready.stranded_at_teardown.clone()
    } else {
        let mut flags = var.to_vec();
        flags.extend(prompt_only);
        match client
            .preflight(&PreflightRequest {
                project_root: project_root.to_path_buf(),
                plan,
                var_flags: flags,
                interactive: false,
            })
            .await
        {
            Ok(PreflightResponse::Ready(r)) => r.stranded_at_teardown,
            _ => ready.stranded_at_teardown.clone(),
        }
    };
    if stranded.is_empty() {
        return;
    }
    let them = if stranded.len() == 1 { "it" } else { "them" };
    eprintln!(
        "Warning: {} answered for this run only, but this project's teardown needs {them}. \
         `veld stop` is a separate action and cannot see a per-run answer, so its \
         `${{vars.*}}` steps will be skipped. Use `veld config set` for {them} if teardown has \
         to work.",
        stranded.join(", "),
    );
}

/// Print the final receipt after a successful start.
fn print_start_receipt(run_state: &veld_core::state::RunState) {
    use veld_core::state::NodeStatus;

    let skip_output_keys = ["port", "url", "exit_code"];

    // Build summary table rows in execution order.
    let mut summary_rows: Vec<Vec<String>> = Vec::new();
    for key in &run_state.execution_order {
        let Some(ns) = run_state.nodes.get(key) else {
            continue;
        };
        let label = format!("{}:{}", ns.node_name, ns.variant);
        let status = match ns.status {
            NodeStatus::Healthy => output::green("healthy"),
            NodeStatus::Skipped => output::dim("skipped"),
            NodeStatus::Failed => output::red("failed"),
            _ => output::dim(&format!("{:?}", ns.status).to_lowercase()),
        };
        // One row per routed http port. A node with a single port (or none)
        // still produces exactly one row, so the common case is unchanged;
        // a node with a secondary http port gets a `node:variant#port` row
        // rather than a hostname that serves but is never printed.
        let routed = ns.routed_urls();
        if routed.is_empty() {
            summary_rows.push(vec![label, status, "-".to_owned()]);
        } else {
            for (i, (port, url)) in routed.iter().enumerate() {
                let label = match port {
                    Some(port) => format!("{}:{}#{port}", ns.node_name, ns.variant),
                    None => label.clone(),
                };
                // The status belongs to the node, not to each of its ports —
                // repeating it would read as several nodes.
                let status = if i == 0 {
                    status.clone()
                } else {
                    String::new()
                };
                summary_rows.push(vec![label, status, (*url).to_owned()]);
            }
        }
    }

    output::print_table(&["Node", "Status", "URL"], &summary_rows);

    // Collect outputs (non-trivial only).
    let mut output_rows: Vec<Vec<String>> = Vec::new();
    for key in &run_state.execution_order {
        let Some(ns) = run_state.nodes.get(key) else {
            continue;
        };
        let label = format!("{}:{}", ns.node_name, ns.variant);
        let mut okeys: Vec<&String> = ns
            .outputs
            .keys()
            .filter(|k| !skip_output_keys.contains(&k.as_str()))
            .collect();
        okeys.sort();
        for okey in okeys {
            let val = if ns.sensitive_keys.contains(okey) {
                "***".to_owned()
            } else {
                ns.outputs[okey].clone()
            };
            output_rows.push(vec![label.clone(), okey.clone(), val]);
        }
    }

    if !output_rows.is_empty() {
        println!();
        output::print_table(&["Node", "Output", "Value"], &output_rows);
    }

    // Summary line.
    let url_count: usize = run_state
        .nodes
        .values()
        .map(|ns| ns.routed_urls().len())
        .sum();
    println!();
    if url_count > 0 {
        output::print_success(&format!(
            "Environment '{}' started. {url_count} URL(s) active.",
            run_state.name,
        ));
    } else {
        output::print_success(&format!(
            "Environment '{}' started (no URLs exposed).",
            run_state.name,
        ));
    }
}

/// Render live progress events from the orchestrator.
///
/// TTY mode: Uses `indicatif::MultiProgress` for concurrent node spinners.
/// Non-TTY/JSON mode: Emits NDJSON for agent consumption.
async fn render_progress(mut rx: mpsc::UnboundedReceiver<Shown>, tty: bool, json_stderr: bool) {
    let mut ctx = TtyProgressCtx::new();

    while let Some(item) = rx.recv().await {
        let event = match item {
            Shown::Progress(event) => event,
            // Program output, in the order it arrived relative to progress: one
            // queue, so a `--oneshot` node's stderr cannot overtake the progress
            // lines that came before it.
            Shown::Line { stderr, line } => {
                // stdout stays stdout: `--oneshot`'s stdout is the node's output
                // and nothing else, and indicatif draws on stderr.
                if stderr && tty && !ctx.bars.is_empty() {
                    ctx.say(line);
                } else if stderr {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
                continue;
            }
        };
        if tty {
            render_progress_tty(&event, &mut ctx);
        } else {
            // NDJSON for non-TTY / agent mode. In --oneshot mode this startup
            // progress is chrome, not program output, so route it to stderr to
            // keep stdout carrying only the terminal node's output.
            if let Ok(json) = serde_json::to_string(&event) {
                if json_stderr {
                    eprintln!("{json}");
                } else {
                    println!("{json}");
                }
            }
        }
    }

    // Clean up any spinners left running (e.g., from aborted parallel tasks
    // that never emitted a completion event).
    for (_key, state) in ctx.bars.drain() {
        state.bar.finish_and_clear();
    }
}

/// What the renderer shows: the engine's progress, or a line of program output.
enum Shown {
    Progress(ProgressEvent),
    Line { stderr: bool, line: String },
}

/// State tracked across TTY progress events. Uses `indicatif::MultiProgress`
/// to show concurrent spinners for parallel node execution within a stage.
struct TtyProgressCtx {
    multi: indicatif::MultiProgress,
    /// Active spinner bars keyed by `"node:variant"`.
    bars: std::collections::HashMap<String, NodeBarState>,
    total: usize,
}

/// Per-node state for its progress bar.
struct NodeBarState {
    bar: indicatif::ProgressBar,
    index: usize,
    label: String,
    port: Option<u16>,
    phase: u8,
    phase_desc: String,
}

impl TtyProgressCtx {
    /// Print a line above the spinners — or plainly when none is on screen:
    /// indicatif's `println` with nothing drawn leaves the cursor mid-line, and
    /// the next line printed outside it lands glued to the end of this one.
    fn say(&self, line: impl Into<String>) {
        let line = line.into();
        if self.bars.is_empty() {
            eprintln!("{line}");
        } else {
            let _ = self.multi.println(line);
        }
    }

    fn new() -> Self {
        Self {
            multi: indicatif::MultiProgress::new(),
            bars: std::collections::HashMap::new(),
            total: 0,
        }
    }
}

impl NodeBarState {
    /// Build the full status message for the spinner.
    fn build_message(&self, total: usize, suffix: &str) -> String {
        let step = output::step(self.index, total, &output::pad_right(&self.label, 30));
        let mut msg = step;
        if let Some(port) = self.port {
            msg.push_str(&format!(" {}", output::dim(&format!("port {port}"))));
        }
        if !self.phase_desc.is_empty() {
            msg.push_str(&format!(
                " {}",
                output::dim(&format!("[phase {}: {}]", self.phase, self.phase_desc)),
            ));
        }
        if !suffix.is_empty() {
            msg.push_str(&format!(" {}", output::dim(suffix)));
        }
        msg
    }

    /// Update the spinner's message with the given suffix.
    fn redraw(&self, total: usize, suffix: &str) {
        self.bar.set_message(self.build_message(total, suffix));
    }
}

/// Render a single progress event for TTY output.
fn render_progress_tty(event: &ProgressEvent, ctx: &mut TtyProgressCtx) {
    match event {
        ProgressEvent::PlanResolved {
            total_nodes,
            stages,
        } => {
            ctx.total = *total_nodes;
            ctx.say(format!(
                "  {} {total_nodes} node(s) in {stages} stage(s)\n",
                output::dim("plan:"),
            ));
        }
        ProgressEvent::NodeStarting {
            node,
            variant,
            index,
            total,
        } => {
            let key = format!("{node}:{variant}");
            let bar = ctx.multi.add(indicatif::ProgressBar::new_spinner());
            bar.enable_steady_tick(std::time::Duration::from_millis(200));
            let state = NodeBarState {
                bar,
                index: *index,
                label: key.clone(),
                port: None,
                phase: 0,
                phase_desc: String::new(),
            };
            state.redraw(*total, "starting...");
            ctx.bars.insert(key, state);
        }
        ProgressEvent::PortAllocated {
            node,
            variant,
            port,
        } => {
            let key = format!("{node}:{variant}");
            if let Some(state) = ctx.bars.get_mut(&key) {
                state.port = Some(*port);
                state.redraw(ctx.total, "starting...");
            }
        }
        ProgressEvent::ReadinessProbePhase {
            node,
            variant,
            phase,
            description,
        } => {
            let key = format!("{node}:{variant}");
            if let Some(state) = ctx.bars.get_mut(&key) {
                state.phase = *phase;
                state.phase_desc = description.clone();
                state.redraw(ctx.total, "");
            }
        }
        ProgressEvent::ReadinessProbeAttempt {
            node,
            variant,
            phase: _,
            attempt,
        } => {
            let key = format!("{node}:{variant}");
            if let Some(state) = ctx.bars.get(&key) {
                state.redraw(ctx.total, &format!("attempt {attempt}"));
            }
        }
        ProgressEvent::ReadinessProbePassed {
            node: _,
            variant: _,
            phase: _,
        } => {
            // Phase pass is shown implicitly by the next event.
        }
        ProgressEvent::NodeHealthy {
            node,
            variant,
            url,
            elapsed_ms,
        } => {
            let key = format!("{node}:{variant}");
            let detail = match url {
                Some(u) => u.clone(),
                None => "healthy".to_owned(),
            };
            let elapsed = format!("{elapsed_ms}ms");
            let finish_msg = format!(
                "  {} {} {}",
                output::checkmark(),
                output::pad_right(&key, 30),
                output::dim(&format!("{detail} ({elapsed})")),
            );
            if let Some(state) = ctx.bars.remove(&key) {
                state.bar.finish_with_message(finish_msg);
            }
        }
        ProgressEvent::NodeSkipped { node, variant } => {
            let key = format!("{node}:{variant}");
            let finish_msg = format!(
                "  {} {} {}",
                output::dim("~"),
                output::pad_right(&key, 30),
                output::dim("skipped (skip_if passed)"),
            );
            if let Some(state) = ctx.bars.remove(&key) {
                state.bar.finish_with_message(finish_msg);
            }
        }
        ProgressEvent::NodeFailed {
            node,
            variant,
            error,
        } => {
            let key = format!("{node}:{variant}");
            let finish_msg = format!(
                "  {} {} {}",
                output::cross(),
                output::pad_right(&key, 30),
                output::red(error),
            );
            if let Some(state) = ctx.bars.remove(&key) {
                state.bar.finish_with_message(finish_msg);
            }
        }
        ProgressEvent::CommandRunning { node, variant } => {
            let key = format!("{node}:{variant}");
            if let Some(state) = ctx.bars.get(&key) {
                state.redraw(ctx.total, "running...");
            }
        }
        ProgressEvent::NodeLogLines {
            node,
            variant,
            lines,
        } => {
            let label = output::dim(&format!("{node}:{variant}"));
            for line in lines {
                // Lines are verbatim; printed through `multi` so they interleave
                // with the spinners instead of overwriting them.
                ctx.say(format!("  {label} {}", output::dim(line)));
            }
        }
        ProgressEvent::SetupStepStarting { name, index, total } => {
            let bar = ctx.multi.add(indicatif::ProgressBar::new_spinner());
            bar.enable_steady_tick(std::time::Duration::from_millis(200));
            bar.set_message(format!(
                "  {} {}",
                output::dim(&format!("setup ({index}/{total}):")),
                name,
            ));
            ctx.bars.insert(
                format!("setup:{name}"),
                NodeBarState {
                    bar,
                    index: *index,
                    label: name.clone(),
                    port: None,
                    phase: 0,
                    phase_desc: String::new(),
                },
            );
        }
        ProgressEvent::SetupStepCompleted { name, elapsed_ms } => {
            let key = format!("setup:{name}");
            let finish_msg = format!(
                "  {} {} {}",
                output::checkmark(),
                output::pad_right(name, 30),
                output::dim(&format!("({elapsed_ms}ms)")),
            );
            if let Some(state) = ctx.bars.remove(&key) {
                state.bar.finish_with_message(finish_msg);
            }
        }
        ProgressEvent::SetupStepFailed { name, error } => {
            let key = format!("setup:{name}");
            let finish_msg = format!(
                "  {} {} {}",
                output::cross(),
                output::pad_right(name, 30),
                output::red(error),
            );
            if let Some(state) = ctx.bars.remove(&key) {
                state.bar.finish_with_message(finish_msg);
            }
        }
        ProgressEvent::TeardownStepRunning { name, index, total } => {
            ctx.say(format!(
                "  {} {}",
                output::dim(&format!("teardown ({index}/{total}):")),
                name,
            ));
        }
        ProgressEvent::TeardownStepCompleted { name } => {
            ctx.say(format!("  {} {}", output::checkmark(), name,));
        }
        ProgressEvent::Notice { message } => {
            ctx.say(format!("  {} {message}", output::dim("»")));
        }
        ProgressEvent::Warning { message } => {
            ctx.say(format!("  {} {}", output::yellow("!"), message));
        }
        ProgressEvent::Unknown => {}
    }
}

/// The refusal shown when there is no way to ask.
///
/// Names every var and prints the exact command, because the alternative — the
/// process quietly choosing for the machine — is the thing this whole path is
/// built to prevent. Goes to stderr so a `--oneshot` stdout capture stays clean.
fn report_unanswered_vars(missing: &[veld_core::values::UnansweredVar], offer_var_flag: bool) {
    let mut detail = String::from(
        "This project needs values that are specific to your machine, and there is no \
         terminal here to ask on.\n\n",
    );
    for var in missing {
        detail.push_str(&format!("  {} — {}\n", var.name, var.question()));
        if let Some(choices) = &var.choices {
            detail.push_str(&format!("      one of: {}\n", choices.join(", ")));
        }
        if var.stale.is_some() {
            detail.push_str("      (the stored answer is no longer an allowed value)\n");
        }
    }
    detail.push_str("\nTo fix this, either:\n");
    // A declared secret must never be told to put its value on a command line.
    // `veld config set X <value>` and `--var X=…` both land in the process table
    // (world-readable) and in shell history — the exact leak `secret: true`
    // exists to prevent, and which `secret-in-command` makes a lint error
    // elsewhere. Point those at a source instead.
    let (secret, plain): (Vec<_>, Vec<_>) = missing.iter().partition(|v| v.secret);
    for var in &plain {
        detail.push_str(&format!("  - veld config set {} <value>\n", var.name));
    }
    for var in &secret {
        detail.push_str(&format!(
            "  - veld config set {name} --env NAME   (or --file PATH, or --shell 'op read …')\n    \
             `{name}` is declared secret, so veld stores where to read it, not the value — \
             a value on the command line is readable by every process on this machine\n",
            name = var.name
        ));
    }
    // Only when the subcommand actually has the flag: `veld restart` does not,
    // and a refusal whose whole job is naming the exact command must not name
    // one that does not exist.
    if offer_var_flag && !plain.is_empty() {
        detail.push_str("  - or pass `--var NAME=VALUE` for a one-off run (not saved)\n");
    }
    output::print_error(detail.trim_end(), false);
}

/// Ask for one value. Returns `None` on EOF, matching the repo's `Cancelled.`
/// convention rather than erroring.
fn prompt_for_var(var: &veld_core::values::UnansweredVar) -> Option<String> {
    use std::io::{BufRead as _, Write as _};
    loop {
        eprintln!();
        eprintln!("  {}", var.question());
        if let Some(choices) = &var.choices {
            eprintln!("  one of: {}", choices.join(", "));
        }
        if var.secret {
            eprintln!(
                "  (declared secret — it will be stored in veld's database only if you say yes)"
            );
        }
        eprint!("  {} = ", var.name);
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).ok()? == 0 {
            return None;
        }
        let value = line.trim().to_owned();
        if value.is_empty() {
            eprintln!("  A value is required — there is no default for this one.");
            continue;
        }
        match &var.choices {
            Some(choices) if !choices.iter().any(|c| c == &value) => {
                eprintln!("  \"{value}\" is not one of: {}", choices.join(", "));
            }
            _ => return Some(value),
        }
    }
}

/// `[Y/n]` confirm on stderr.
fn prompt_yes_no(question: &str) -> bool {
    use std::io::{BufRead as _, Write as _};
    eprint!("{question} [Y/n] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        // EOF mid-prompt is not consent.
        return false;
    }
    !matches!(line.trim().to_ascii_lowercase().as_str(), "n" | "no")
}

/// Interactive preset selector.
///
/// Accepts a **key**, not a list position. That is the entire point: the number
/// beside a preset is assigned by [`veld_core::presets`] and does not move when
/// other presets are added, so it survives in someone's muscle memory, in a
/// runbook, and in a message to a colleague. A name is accepted too, and an
/// empty line takes the default when one is declared.
fn interactive_preset_selector(
    config: &VeldConfig,
    presets: &[veld_core::presets::ResolvedPreset],
    default: Option<&veld_core::presets::ResolvedPreset>,
) -> Option<String> {
    use std::io::{self, BufRead, Write};

    let show_groups = veld_core::presets::has_groups(config);

    println!("{}", output::bold("Available presets:"));
    let mut current_group: Option<Option<String>> = None;
    for preset in presets {
        if show_groups && current_group.as_ref() != Some(&preset.group) {
            println!();
            println!(
                "  {}",
                output::bold(&output::one_line(
                    preset.group.as_deref().unwrap_or("Other")
                )),
            );
            current_group = Some(preset.group.clone());
        } else if !show_groups && current_group.is_none() {
            println!();
            current_group = Some(None);
        }

        let name = if preset.label.is_some() {
            format!(
                " {}",
                output::dim(&format!("({})", output::one_line(&preset.name)))
            )
        } else {
            String::new()
        };
        let marker = if preset.is_default {
            format!(" {}", output::green("(default)"))
        } else {
            String::new()
        };
        println!(
            "  {} {}{name}{marker}",
            output::cyan(&format!("[{}]", preset.key)),
            output::one_line(preset.display_label()),
        );
        if let Some(when) = &preset.when_to_use {
            println!("      {}", output::dim(&output::one_line(when)));
        }
    }
    println!();

    let prompt = match default {
        Some(d) => format!(
            "Select a preset by number or name [{} = {}]: ",
            output::bold("enter"),
            // Sanitised like every other config-authored string here. This is the
            // one line where the user commits to a choice, and it is printed
            // without a trailing newline — so an unsanitised `label` carrying
            // `ESC [2K` + `CR` could make the prompt name one preset while enter
            // starts another.
            output::one_line(d.display_label())
        ),
        None => "Select a preset by number or name: ".to_owned(),
    };
    print!("{prompt}");
    io::stdout().flush().ok()?;

    // EOF (Ctrl-D) and a read error land here. Say "Cancelled." rather than
    // exiting 1 with nothing printed, which is what a bare `?` used to do — and
    // which reads as a crash.
    let stdin = io::stdin();
    let Some(Ok(line)) = stdin.lock().lines().next() else {
        println!();
        output::print_info("Cancelled.");
        return None;
    };

    match veld_core::presets::interpret_pick(&line, config) {
        veld_core::presets::Pick::Chosen(hit) => Some(hit.name),
        veld_core::presets::Pick::Cancelled => {
            output::print_info("Cancelled.");
            None
        }
        veld_core::presets::Pick::NotFound => {
            output::print_error(
                &format!(
                    "No preset `{}`. Type one of the numbers above, or a preset name.",
                    output::one_line(line.trim())
                ),
                false,
            );
            None
        }
    }
}

/// Interactive node+variant picker for TTY mode when no presets are defined.
fn interactive_node_variant_picker(config: &VeldConfig) -> Option<Vec<NodeSelection>> {
    use std::io::{self, BufRead, Write};

    let mut node_names: Vec<&String> = config.nodes.keys().collect();
    node_names.sort();

    if node_names.is_empty() {
        output::print_error("No nodes defined in config.", false);
        return None;
    }

    // Display available nodes.
    println!("{}", output::bold("Available nodes:"));
    println!();
    for (i, name) in node_names.iter().enumerate() {
        let node_cfg = &config.nodes[*name];
        let mut variant_names: Vec<&String> = node_cfg.variants.keys().collect();
        variant_names.sort();
        let variants_str = variant_names
            .iter()
            .map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {} {} {}",
            output::dim(&format!("[{}]", i + 1)),
            name,
            output::dim(&format!("({})", variants_str)),
        );
    }
    println!();
    print!(
        "Select nodes to start (1-{}, comma-separated): ",
        node_names.len()
    );
    io::stdout().flush().ok()?;

    let stdin = io::stdin();
    let line = stdin.lock().lines().next()?.ok()?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        output::print_info("Cancelled.");
        return None;
    }

    // Parse selected indices.
    let indices: Vec<usize> = trimmed
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .collect();

    if indices.is_empty() {
        output::print_info("Cancelled.");
        return None;
    }

    let mut selections = Vec::new();

    for idx in &indices {
        if *idx == 0 || *idx > node_names.len() {
            output::print_error(
                &format!(
                    "Invalid selection: {}. Must be 1-{}.",
                    idx,
                    node_names.len()
                ),
                false,
            );
            return None;
        }
        let node_name = node_names[*idx - 1];
        let node_cfg = &config.nodes[node_name];
        let mut variant_names: Vec<&String> = node_cfg.variants.keys().collect();
        variant_names.sort();

        let variant = if variant_names.len() == 1 {
            // Auto-select the only variant.
            variant_names[0].clone()
        } else {
            // Ask user which variant.
            println!();
            println!(
                "{} {}",
                output::bold("Variants for"),
                output::bold(node_name),
            );
            for (vi, v) in variant_names.iter().enumerate() {
                println!("  {} {}", output::dim(&format!("[{}]", vi + 1)), v);
            }
            print!(
                "Select variant for {} (1-{}): ",
                node_name,
                variant_names.len()
            );
            io::stdout().flush().ok()?;

            let vline = io::stdin().lock().lines().next()?.ok()?;
            let vidx: usize = vline.trim().parse().ok()?;
            if vidx == 0 || vidx > variant_names.len() {
                output::print_error(
                    &format!(
                        "Invalid variant selection: {}. Must be 1-{}.",
                        vidx,
                        variant_names.len()
                    ),
                    false,
                );
                return None;
            }
            variant_names[vidx - 1].clone()
        };

        selections.push(NodeSelection {
            node: node_name.clone(),
            variant,
        });
    }

    if selections.is_empty() {
        output::print_info("Cancelled.");
        return None;
    }

    Some(selections)
}
