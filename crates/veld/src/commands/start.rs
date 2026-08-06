use veld_core::config::VeldConfig;
use veld_core::graph::{self, NodeSelection};
use veld_core::orchestrator::Orchestrator;
use veld_core::progress::ProgressEvent;
use veld_core::url::generate_run_name;

use tokio::sync::mpsc;

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
    _debug: bool,
) -> i32 {
    let Some((config_path, config)) = super::parse_config(false) else {
        return 1;
    };

    // Determine what to start. The second half of the pair is the preset's
    // config name when one was involved — recorded on the run so a later reader
    // can say what it was started *from*, not only what it resolved to. `--preset`
    // may name a key, so this is `chosen.name`, never the token as typed.
    let (parsed_selections, origin_preset) = if let Some(ref token) = preset {
        // `--preset` takes a name or the key `veld presets` printed, resolved here
        // rather than passed straight to `expand_preset`.
        //
        // Name-first, unlike the picker: `--preset` is what scripts, CI and the
        // desktop UI pass, and all of those predate keys existing. The two orders
        // agree for every config that does not both name a preset like a number and
        // pin that number on a different preset — a config `veld lint` warns about.
        // See `presets::find_by_name_then_key`.
        let Some(chosen) = veld_core::presets::find_by_name_then_key(&config, token) else {
            output::print_error(&unknown_preset_message(&config, token), false);
            return 1;
        };
        match expand_and_resolve(&chosen.name, &config) {
            Some(sels) => (sels, Some(chosen.name.clone())),
            None => return 1,
        }
    } else if selections.is_empty() {
        match handle_no_selections(&config) {
            Some(pair) => pair,
            None => return 1,
        }
    } else {
        let raw: Result<Vec<NodeSelection>, _> = selections
            .iter()
            .map(|s| graph::parse_selection(s))
            .collect();
        match raw {
            Ok(parsed) => match graph::resolve_selections(&parsed, &config) {
                // Explicit tokens: `preset: None` is the honest record, and the
                // absence is meaningful — this run did not come from a preset.
                Ok(resolved) => (resolved, None),
                Err(e) => {
                    output::print_error(&format!("{e}"), false);
                    return 1;
                }
            },
            Err(e) => {
                output::print_error(&format!("{e}"), false);
                return 1;
            }
        }
    };

    // Validate: non-localhost URL templates require privileged mode.
    let non_localhost = find_non_localhost_domains(&parsed_selections, &config);
    if !non_localhost.is_empty() {
        let mode = super::read_setup_mode().unwrap_or_else(|| "auto".to_owned());
        if mode != "privileged" {
            let mut detail =
                String::from("Custom apex domains are only supported in privileged mode.\n");
            detail.push_str("\n  Affected nodes:\n");
            for (label, hostname) in &non_localhost {
                detail.push_str(&format!("    - {label} => {hostname}\n"));
            }
            detail.push_str(
                "\n  In unprivileged/auto mode, veld cannot write to /etc/hosts or manage\n  \
                 system DNS, so only .localhost domains work (RFC 6761).\n\
                 \n  To fix this, either:\n  \
                 - Change your url_template to use .localhost (e.g. {service}.{run}.{project}.localhost)\n  \
                 - Run `veld setup privileged` (one-time sudo) to enable custom domains",
            );
            output::print_error(&detail, false);
            return 1;
        }
    }

    // --oneshot: validate and pick the terminal command node.
    let terminal_sel = if oneshot {
        if attach {
            output::print_error("--attach and --oneshot cannot be combined.", false);
            return 1;
        }
        if parsed_selections.len() != 1 {
            output::print_error(
                "--oneshot requires exactly one command-type selection (the terminal node whose \
                 exit ends the run). Its dependencies are started automatically.",
                false,
            );
            return 1;
        }
        let sel = parsed_selections[0].clone();
        // Resolved: `type` is hoistable to node level (F3), so a raw read would
        // misclassify a node that declares it once for all its variants.
        let is_command = config
            .resolved(&sel.node, &sel.variant)
            .map(|r| r.step_type == veld_core::config::StepType::Command)
            .unwrap_or(false);
        if !is_command {
            output::print_error(
                &format!(
                    "--oneshot requires a command-type node; '{}:{}' is a start_server (it never \
                     exits, so it cannot be the terminal node).",
                    sel.node, sel.variant
                ),
                false,
            );
            return 1;
        }
        Some(sel)
    } else {
        if all_logs {
            output::print_error("--all-logs only applies with --oneshot.", false);
            return 1;
        }
        None
    };

    let project_root = veld_core::config::project_root(&config_path);
    let run_name = match name {
        Some(ref n) => n.clone(),
        None => generate_run_name(&project_root),
    };
    let run_name_str = run_name.as_str();

    // Parsed before the config is moved into the orchestrator, because
    // validating a `--var` name needs the declarations. Refusing an unknown one
    // here also means we refuse *before* anything is built or spawned.
    let var_answers = match parse_var_answers(&var, &config) {
        Ok(answers) => answers,
        Err(e) => {
            output::print_error(&e, false);
            return 1;
        }
    };

    // Build the orchestrator.
    let foreground = attach && is_tty();
    let mut orchestrator = match Orchestrator::new(config_path.clone(), config) {
        Ok(o) => o,
        Err(e) => {
            output::print_error(&format!("Failed to initialize: {e}"), false);
            return 1;
        }
    };
    orchestrator.set_debug(_debug);
    orchestrator.set_foreground(foreground);
    orchestrator.set_terminal_node(terminal_sel.clone());

    // Per-run answers, above the stored ones and never written anywhere.
    if !var_answers.is_empty() {
        orchestrator.set_var_answers(var_answers);
    }

    // Pre-flight: every machine-overridable var this plan needs must have an
    // answer before the first process spawns.
    if !resolve_machine_vars(&mut orchestrator, &parsed_selections, &project_root).await {
        return 1;
    }
    // A `--var` answer does not survive this process, and `veld stop` runs in
    // another one. Say so now, while `veld config set` is still an option.
    let stranded = orchestrator.flag_answers_needed_at_teardown(&parsed_selections);
    if !stranded.is_empty() {
        eprintln!(
            "Warning: {} answered with --var, but this project's teardown needs {}. \
             `veld stop` runs in a different process and cannot see a --var answer, so its \
             `${{vars.*}}` steps will be skipped. Use `veld config set` for {} if teardown \
             has to work.",
            stranded.join(", "),
            if stranded.len() == 1 { "it" } else { "them" },
            if stranded.len() == 1 { "it" } else { "them" },
        );
    }

    // Set up live progress channel.
    let (progress_tx, progress_rx) = mpsc::unbounded_channel::<ProgressEvent>();
    orchestrator.set_progress_sender(progress_tx);
    let tty = is_tty();
    // In --oneshot mode, stdout must carry ONLY the terminal node's output, so
    // the startup progress stream goes to stderr.
    let progress_handle = tokio::spawn(render_progress(progress_rx, tty, terminal_sel.is_some()));

    eprintln!(
        "{} Starting environment {}...",
        output::bold("veld"),
        output::bold(&format!("'{run_name_str}'")),
    );
    eprintln!();

    // Run start with Ctrl+C interception so we can clean up on interrupt.
    let start_result = tokio::select! {
        result = orchestrator.start(
            &parsed_selections,
            run_name_str,
            // Always recorded here: a `veld start` always knows what it was asked
            // for, even when that was explicit tokens (`preset: None`).
            Some(veld_core::state::StartOrigin::new(
                origin_preset.clone(),
                &parsed_selections,
            )),
        ) => result,
        _ = tokio::signal::ctrl_c() => {
            orchestrator.close_progress_sender();
            let _ = progress_handle.await;
            eprintln!();
            // Interrupt/cleanup messages are diagnostics → stderr (keeps stdout
            // clean, notably for --oneshot where stdout is the node's output).
            eprintln!(
                "  {} Interrupted — stopping partially started environment...",
                output::dim("»")
            );
            match orchestrator.stop(run_name_str).await {
                Ok(_) => eprintln!(
                    "  {} Environment '{run_name_str}' cleaned up.",
                    output::checkmark()
                ),
                Err(e) => output::print_error(&format!("Cleanup failed: {e}"), false),
            }
            return 130; // Standard exit code for SIGINT
        }
    };

    match start_result {
        Ok(run_state) => {
            // Drop the progress sender so the renderer can finish.
            orchestrator.close_progress_sender();
            let _ = progress_handle.await;

            // --oneshot: dependencies are up; now run the terminal node to
            // completion, tear everything down, and exit with its code.
            if let Some(ref sel) = terminal_sel {
                return run_oneshot_terminal(
                    &mut orchestrator,
                    &run_state,
                    sel,
                    run_name_str,
                    &project_root,
                    all_logs,
                )
                .await;
            }

            // Final receipt: summary table.
            println!();
            print_start_receipt(&run_state);

            // Show setup hint if in unprivileged mode.
            crate::hints::maybe_show_privileged_hint(orchestrator.https_port);

            // Foreground mode: tail logs and stop on Ctrl+C.
            if foreground {
                println!();
                output::print_info("Streaming logs (Ctrl+C to stop)...");
                println!();

                let project_root = veld_core::config::project_root(&config_path);
                let targets: Vec<(String, String)> = run_state
                    .nodes
                    .values()
                    .map(|ns| (ns.node_name.clone(), ns.variant.clone()))
                    .collect();

                // Stream logs until Ctrl+C.
                follow_logs_until_interrupt(
                    &orchestrator.db,
                    &targets,
                    &project_root,
                    run_name_str,
                )
                .await;

                // Ctrl+C received — stop the environment.
                println!();
                output::print_info("Stopping environment...");
                let _ = orchestrator.stop(run_name_str).await;
                output::print_success(&format!("Environment '{}' stopped.", run_name_str));
            }

            0
        }
        Err(e) => {
            orchestrator.close_progress_sender();
            let _ = progress_handle.await;
            // Surface failureMessage for setup step failures.
            if let veld_core::orchestrator::OrchestratorError::SetupFailed {
                failure_message: Some(ref msg),
                ..
            } = e
            {
                output::print_error(&format!("Startup failed: {msg}"), false);
            } else {
                output::print_error(&format!("Startup failed: {e}"), false);
            }
            // Dump the tail of the service log so the user can see what went wrong.
            if tty {
                if let veld_core::orchestrator::OrchestratorError::NodeFailed {
                    ref node,
                    ref variant,
                    ..
                } = e
                {
                    // Scoped to this run. Unscoped, the tail spans every run
                    // ever started under this name — and now that a `command`
                    // node produces log rows at all, a node that failed after
                    // three lines would have the other seventeen filled in from
                    // a previous attempt and presented as this failure's.
                    let run_id = orchestrator
                        .db
                        .get_run(&project_root, run_name_str)
                        .ok()
                        .flatten()
                        .map(|r| r.run_id.to_string());
                    let filter = veld_core::db::LogFilter {
                        node: Some(node.clone()),
                        variant: Some(variant.clone()),
                        streams: Some(vec![veld_core::db::LogStream::Server.as_str()]),
                        run_id,
                    };
                    if let Ok(rows) =
                        orchestrator
                            .db
                            .tail_logs(&project_root, run_name_str, &filter, 20)
                    {
                        if !rows.is_empty() {
                            eprintln!();
                            eprintln!(
                                "  {}",
                                output::dim(&format!("Last log lines from {node}:{variant}:"))
                            );
                            eprintln!();
                            for row in &rows {
                                eprintln!("    {}", output::dim(&row.line));
                            }
                            eprintln!();
                            eprintln!(
                                "  {}",
                                output::dim(&format!(
                                    "Full log: veld logs --name {run_name_str} --node {node}"
                                ))
                            );
                        }
                    }
                }
            }

            // Best-effort teardown.
            let _stop_result = orchestrator.stop(run_name_str).await;
            1
        }
    }
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
        let url = ns.url.as_deref().unwrap_or("-").to_owned();
        summary_rows.push(vec![label, status, url]);
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
    let url_count = run_state
        .nodes
        .values()
        .filter(|ns| ns.url.is_some())
        .count();
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
async fn render_progress(
    mut rx: mpsc::UnboundedReceiver<ProgressEvent>,
    tty: bool,
    json_stderr: bool,
) {
    let mut ctx = TtyProgressCtx::new();

    while let Some(event) = rx.recv().await {
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
            let _ = ctx.multi.println(format!(
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
                let _ = ctx
                    .multi
                    .println(format!("  {label} {}", output::dim(line)));
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
            let _ = ctx.multi.println(format!(
                "  {} {}",
                output::dim(&format!("teardown ({index}/{total}):")),
                name,
            ));
        }
        ProgressEvent::TeardownStepCompleted { name } => {
            let _ = ctx
                .multi
                .println(format!("  {} {}", output::checkmark(), name,));
        }
        ProgressEvent::Notice { message } => {
            let _ = ctx
                .multi
                .println(format!("  {} {message}", output::dim("»")));
        }
    }
}

/// Tail server logs from the database, printing timestamped lines with node
/// labels, until Ctrl+C.
async fn follow_logs_until_interrupt(
    db: &veld_core::db::Db,
    targets: &[(String, String)],
    project_root: &std::path::Path,
    run_name: &str,
) {
    // Skip historical output: start after the current newest row.
    let mut last_id = db.max_log_id().unwrap_or(0);
    let target_set: std::collections::HashSet<(String, String)> = targets.iter().cloned().collect();
    let filter = veld_core::db::LogFilter {
        node: None,
        variant: None,
        streams: Some(vec![veld_core::db::LogStream::Server.as_str()]),
        run_id: None,
    };

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let rows = match db.logs_after_id(project_root, run_name, &filter, last_id) {
                    Ok(rows) => rows,
                    Err(_) => continue,
                };
                for row in rows {
                    last_id = row.id;
                    let (Some(node), Some(variant)) = (row.node.as_deref(), row.variant.as_deref())
                    else {
                        continue;
                    };
                    if !target_set.contains(&(node.to_owned(), variant.to_owned())) {
                        continue;
                    }
                    let label = output::cyan(&format!("{node}:{variant}"));
                    println!("{label} [{}] {}", row.ts, row.line);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                return;
            }
        }
    }
}

/// Run the terminal one-off node (`--oneshot`) after its dependencies are
/// healthy: stream its output, tear everything down in reverse order, and
/// return its exit code so CI/callers see pass/fail directly.
async fn run_oneshot_terminal(
    orchestrator: &mut Orchestrator,
    run_state: &veld_core::state::RunState,
    sel: &NodeSelection,
    run_name: &str,
    project_root: &std::path::Path,
    all_logs: bool,
) -> i32 {
    let label = format!("{}:{}", sel.node, sel.variant);

    // Everything veld prints here is chrome and goes to STDERR: stdout must
    // carry only the terminal node's own output (streamed by run_terminal), so
    // an agent or CI job that captures stdout gets just the test results.
    eprintln!();
    print_deps_summary_stderr(run_state, sel);
    eprintln!();
    eprintln!(
        "  {} Running {} (Ctrl+C to abort)...",
        output::dim("»"),
        output::bold(&label)
    );
    eprintln!();

    // Optionally interleave dependency logs — also on stderr, keeping stdout
    // the terminal node's output only.
    let tailer = if all_logs {
        let db = orchestrator.db.clone();
        let pr = project_root.to_path_buf();
        let rn = run_name.to_owned();
        let dep_targets: Vec<(String, String)> = run_state
            .nodes
            .values()
            .filter(|ns| !(ns.node_name == sel.node && ns.variant == sel.variant))
            .map(|ns| (ns.node_name.clone(), ns.variant.clone()))
            .collect();
        Some(tokio::spawn(async move {
            follow_dep_logs(&db, &dep_targets, &pr, &rn).await;
        }))
    } else {
        None
    };

    let result = orchestrator.run_terminal(run_name, sel).await;

    if let Some(handle) = tailer {
        handle.abort();
    }

    let exit_code = match result {
        Ok(code) => code,
        Err(e) => {
            output::print_error(&format!("Failed to run {label}: {e}"), false);
            // The terminal node never produced an exit — record the failure
            // intent before the teardown below finalizes, or history would
            // read `stopped` for a run that actually failed to execute.
            if let Ok(Some(run)) = orchestrator.db.get_run(project_root, run_name) {
                let detail = veld_core::state::EndDetail {
                    exit_code: Some(127),
                    message: Some(format!("terminal node failed to run: {e}")),
                    ..Default::default()
                };
                let _ = orchestrator.db.begin_ending(
                    &run.run_id,
                    veld_core::state::EndReason::Failed,
                    Some(&detail),
                );
            }
            127
        }
    };

    // Tear down all dependencies in reverse order, regardless of outcome.
    eprintln!();
    eprintln!("  {} Tearing down environment...", output::dim("»"));
    match orchestrator.stop(run_name).await {
        Ok(_) => eprintln!(
            "  {} Environment '{run_name}' torn down.",
            output::checkmark()
        ),
        Err(e) => output::print_error(&format!("Teardown failed: {e}"), false),
    }

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

/// Print a compact summary of the started dependencies to **stderr** (stdout is
/// reserved for the terminal node's own output in `--oneshot` mode).
fn print_deps_summary_stderr(run_state: &veld_core::state::RunState, terminal: &NodeSelection) {
    use veld_core::state::{NodeStatus, RunState};

    let term_key = RunState::node_key(&terminal.node, &terminal.variant);
    for key in &run_state.execution_order {
        if key == &term_key {
            continue;
        }
        let Some(ns) = run_state.nodes.get(key) else {
            continue;
        };
        let status = match ns.status {
            NodeStatus::Healthy => output::green("healthy"),
            NodeStatus::Skipped => output::dim("skipped"),
            _ => output::dim(&format!("{:?}", ns.status).to_lowercase()),
        };
        eprintln!(
            "  {} {} {}",
            output::dim(&format!("{}:{}", ns.node_name, ns.variant)),
            status,
            output::dim(ns.url.as_deref().unwrap_or("-")),
        );
    }
}

/// Tail dependency server logs until the task is aborted. Used by
/// `--oneshot --all-logs` to interleave dependency output with the terminal
/// node's own (already-streamed) output.
async fn follow_dep_logs(
    db: &veld_core::db::Db,
    targets: &[(String, String)],
    project_root: &std::path::Path,
    run_name: &str,
) {
    let mut last_id = db.max_log_id().unwrap_or(0);
    let target_set: std::collections::HashSet<(String, String)> = targets.iter().cloned().collect();
    let filter = veld_core::db::LogFilter {
        node: None,
        variant: None,
        streams: Some(vec![veld_core::db::LogStream::Server.as_str()]),
        run_id: None,
    };

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        interval.tick().await;
        let rows = match db.logs_after_id(project_root, run_name, &filter, last_id) {
            Ok(rows) => rows,
            Err(_) => continue,
        };
        for row in rows {
            last_id = row.id;
            let (Some(node), Some(variant)) = (row.node.as_deref(), row.variant.as_deref()) else {
                continue;
            };
            if !target_set.contains(&(node.to_owned(), variant.to_owned())) {
                continue;
            }
            let plabel = output::dim(&format!("{node}:{variant}"));
            eprintln!("{plabel} {}", output::dim(&row.line));
        }
    }
}

/// Expand a preset by name and resolve it to concrete selections, reporting the
/// failure. `None` means the caller should exit non-zero.
fn expand_and_resolve(name: &str, config: &VeldConfig) -> Option<Vec<NodeSelection>> {
    match graph::expand_preset(name, config) {
        Ok(sels) => match graph::resolve_selections(&sels, config) {
            Ok(resolved) => Some(resolved),
            Err(e) => {
                output::print_error(&format!("Invalid preset `{name}`: {e}"), false);
                None
            }
        },
        Err(e) => {
            output::print_error(&format!("Invalid preset `{name}`: {e}"), false);
            None
        }
    }
}

/// What to say when `--preset` names nothing. Lists what *is* available, keys
/// included, because the next thing the reader needs is the thing they should
/// have typed.
fn unknown_preset_message(config: &VeldConfig, token: &str) -> String {
    let available = veld_core::presets::resolve(config);
    if available.is_empty() {
        return format!("Unknown preset `{token}` — this project defines no presets.");
    }
    let list = available
        .iter()
        .map(|p| format!("{} ({})", output::one_line(&p.name), p.key))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Unknown preset `{}`. Available (name and key): {list}",
        output::one_line(token)
    )
}

/// Parse `--var NAME=VALUE` pairs, refusing a name the config does not declare
/// machine-overridable.
///
/// Silence here is the worst option: resolution only consults an override for a
/// var that is still declared `machine`, so `--var typo=x` — or a `--var` for an
/// ordinary var — would otherwise be dropped without a word. The run starts, the
/// config's value wins, and the snapshot does not record the flag either, so
/// nothing anywhere says the answer was ignored. `veld config set` already
/// refuses the same input and lists the declared names.
fn parse_var_answers(
    pairs: &[String],
    config: &VeldConfig,
) -> Result<veld_core::values::VarOverrides, String> {
    let declared: Vec<&str> = {
        let mut v: Vec<&str> = config
            .vars
            .iter()
            .flatten()
            .filter(|(_, d)| d.machine().is_some())
            .map(|(n, _)| n.as_str())
            .collect();
        v.sort_unstable();
        v
    };
    let mut out = veld_core::values::VarOverrides::new();
    for pair in pairs {
        let Some((name, value)) = pair.split_once('=') else {
            return Err(format!(
                "--var expects NAME=VALUE, got `{pair}`. An empty value is written `NAME=`"
            ));
        };
        if name.is_empty() {
            return Err(format!("--var `{pair}` has no name before the `=`"));
        }
        if !declared.contains(&name) {
            return Err(format!(
                "--var `{name}` is not declared machine-overridable in this project, so it \
                 would be ignored. Machine-overridable vars: {}",
                if declared.is_empty() {
                    "(none)".to_owned()
                } else {
                    declared.join(", ")
                }
            ));
        }
        // Sensitivity is applied at resolution from the *declaration*, so a
        // `--var` answer for a secret var is still redacted. The flag itself is
        // visible in the process table, which is why it is documented as the
        // wrong way to pass a secret.
        out.insert(
            name.to_owned(),
            veld_core::config::ConfigValue::literal(value),
        );
    }
    Ok(out)
}

/// Make sure every machine-overridable var this plan needs has an answer.
///
/// **The rule this function exists to enforce: a value nobody chose is never
/// written.** Falling back to a declared `default` stores nothing, and a prompt's
/// answer is stored only when the human says so. The failure that motivated it
/// was a background process taking a default, which then looked exactly like a
/// deliberate choice for as long as the machine lived.
pub(super) async fn resolve_machine_vars(
    orchestrator: &mut Orchestrator,
    selections: &[NodeSelection],
    project_root: &std::path::Path,
) -> bool {
    let missing = match orchestrator.unanswered_vars(selections) {
        Ok(m) => m,
        Err(e) => {
            output::print_error(&format!("{e}"), false);
            return false;
        }
    };
    if missing.is_empty() {
        return true;
    }

    // Attended means "a human is reading this and can type". Without a terminal
    // there is no channel, and inventing one — resolving a default, or worse
    // persisting a guess — is the documented failure.
    //
    // Gated on **both streams this prompt actually uses**, and on neither of the
    // ones it doesn't.
    //
    // Not stdout (`output::is_tty()`): the question goes to stderr, so
    // `veld start --oneshot … > out.json` from a real terminal was declared
    // unattended and refused with a human sitting right there.
    //
    // Not stdin alone either, which is the trap on the way back: the daemon
    // spawns `veld start` with stdout nulled and stderr redirected to a log file
    // but **inherits stdin**, so a daemon launched from a terminal hands its tty
    // to every run it starts. Gating on stdin alone would make that run believe a
    // human was watching, write the question into a log file nobody reads, and
    // block on `read_line` forever — while the UI, which already got its
    // `202 ACCEPTED`, showed a run that never starts. Requiring stderr to be a
    // terminal is what distinguishes the two: a human can only answer a question
    // they can see. (`spawn_veld_in` also nulls stdin now; this is the half that
    // does not depend on the spawner getting it right.)
    let attended = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr())
        && std::env::var("VELD_NON_INTERACTIVE")
            .map(|v| v.is_empty())
            .unwrap_or(true);
    if !attended {
        report_unanswered_vars(&missing);
        return false;
    }

    let mut answers = veld_core::values::VarOverrides::new();
    let mut to_store: Vec<(String, veld_core::config::ConfigValue)> = Vec::new();
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
            return false;
        };
        let cv = veld_core::config::ConfigValue {
            source: veld_core::config::SecretSource::Literal(value),
            secret: var.secret,
        };
        // Ask before writing. A "no" still starts the run — the answer is just
        // not this machine's answer, which is exactly the distinction the store
        // is supposed to preserve.
        if prompt_yes_no(&format!("  Save `{}` for this machine?", var.name)) {
            to_store.push((var.name.clone(), cv.clone()));
        } else {
            eprintln!("  Using it for this run only.");
        }
        answers.insert(var.name.clone(), cv);
    }

    if !to_store.is_empty() {
        // Counted, so the summary describes what happened rather than what was
        // attempted: it used to print "Saved to …" even when every write failed,
        // and print nothing at all when the database would not open — losing
        // answers the human had explicitly confirmed saving, silently.
        let mut saved = 0usize;
        match super::open_db(false) {
            Some(db) => {
                let project_id = orchestrator.project_id().clone();
                for (name, value) in &to_store {
                    match db.set_var_override(
                        &project_id,
                        veld_core::db::OverrideScope::Project,
                        project_root,
                        name,
                        value,
                    ) {
                        // Not fatal: the run has its answers in memory. Losing
                        // the *storage* costs a re-prompt next time, not this
                        // start.
                        Err(e) => {
                            eprintln!("Warning: could not save `{name}` for this machine: {e}");
                        }
                        Ok(()) => saved += 1,
                    }
                }
                if saved > 0 {
                    eprintln!(
                        "Saved {saved} value(s) to {project_id} — shared by every worktree of \
                         this project. Change with `veld config set`, clear with \
                         `veld config unset`."
                    );
                }
            }
            None => eprintln!(
                "Warning: veld's database could not be opened, so nothing was saved for this \
                 machine. This run has your answers; the next one will ask again."
            ),
        }
    }
    eprintln!();
    orchestrator.set_var_answers(answers);
    true
}

/// The refusal shown when there is no way to ask.
///
/// Names every var and prints the exact command, because the alternative — the
/// process quietly choosing for the machine — is the thing this whole path is
/// built to prevent. Goes to stderr so a `--oneshot` stdout capture stays clean.
fn report_unanswered_vars(missing: &[veld_core::values::UnansweredVar]) {
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
    if !plain.is_empty() {
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

/// Handle the case where no selections or preset were given.
///
/// Returns the selections and, when the answer came from a preset, that
/// preset's config name — a bare `veld start` that lands on `default_preset` or
/// on the interactive picker *did* start from a preset, and the run's record has
/// to say so or the two paths would look like explicit-token starts.
fn handle_no_selections(config: &VeldConfig) -> Option<(Vec<NodeSelection>, Option<String>)> {
    let presets = veld_core::presets::resolve(config);
    let default = veld_core::presets::default_preset(config);

    // A declared default is the answer to "just start it" whether or not anyone
    // is watching — so it applies without a TTY too. That is the case a coding
    // agent hits: `veld start` in a non-interactive shell used to be an error
    // payload listing everything and choosing nothing.
    if !is_tty() {
        if let Some(default) = default {
            // stderr, not `print_info`: without a TTY this run streams JSON
            // events on stdout, and a chrome line in that stream corrupts the
            // capture of the very agent this path exists to serve. Names the
            // config key, which is what `--preset` takes, not just the label.
            let label = default
                .label
                .as_deref()
                .map(|l| format!(" ({})", output::one_line(l)))
                .unwrap_or_default();
            eprintln!(
                "Starting default preset `{}`{label}.",
                output::one_line(&default.name)
            );
            let sels = expand_and_resolve(&default.name, config)?;
            return Some((sels, Some(default.name.clone())));
        }
        let node_names: Vec<String> = config.nodes.keys().cloned().collect();
        let payload = serde_json::json!({
            "error": "No selections provided",
            "nodes": node_names,
            "presets": presets,
            "hint": "Pass `--preset <name-or-key>`, explicit `node:variant` selections, \
                     or set `default_preset` in veld.json so a bare `veld start` has an \
                     answer.",
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        return None;
    }

    if presets.is_empty() {
        return interactive_node_variant_picker(config).map(|sels| (sels, None));
    }

    let selected = interactive_preset_selector(config, &presets, default.as_ref())?;
    let sels = expand_and_resolve(&selected, config)?;
    Some((sels, Some(selected)))
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

/// Find all `start_server` nodes whose URL template resolves to a
/// non-localhost domain. Returns a list of `(node:variant, hostname)` pairs.
fn find_non_localhost_domains(
    selections: &[veld_core::graph::NodeSelection],
    config: &VeldConfig,
) -> Vec<(String, String)> {
    use veld_core::config::StepType;
    use veld_core::url;

    // Build dummy values to evaluate templates — the apex domain is the static
    // part of the template, so placeholder values are sufficient.
    let dummy_values =
        url::build_url_template_values("svc", "var", "run", "proj", "branch", "wt", "user", "host");

    let mut offenders = Vec::new();

    for sel in selections {
        let node_cfg = match config.nodes.get(&sel.node) {
            Some(n) => n,
            None => continue,
        };
        let variant_cfg = match node_cfg.variants.get(&sel.variant) {
            Some(v) => v,
            None => continue,
        };
        let Some(resolved) = config.resolved(&sel.node, &sel.variant) else {
            continue;
        };

        if resolved.step_type != StepType::StartServer {
            continue;
        }

        let effective_template = url::resolve_url_template(
            &config.url_template,
            node_cfg.url_template.as_deref(),
            variant_cfg.url_template.as_deref(),
        );

        // Err means an unrecognized variable — that template will also fail at
        // runtime, so we skip it here rather than producing a confusing error.
        if let Ok(hostname) = url::evaluate_url_template(effective_template, &dummy_values) {
            if !url::is_localhost_domain(&hostname) {
                offenders.push((format!("{}:{}", sel.node, sel.variant), hostname));
            }
        }
    }

    offenders
}
