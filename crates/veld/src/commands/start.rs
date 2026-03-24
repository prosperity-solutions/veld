use std::collections::HashMap;
use std::path::PathBuf;

use veld_core::config::VeldConfig;
use veld_core::graph::{self, NodeSelection};
use veld_core::logging;
use veld_core::orchestrator::Orchestrator;
use veld_core::progress::ProgressEvent;
use veld_core::url::generate_run_name;

use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::mpsc;

use crate::output::{self, is_tty};

/// `veld start [node:variant...] [--preset <n>] [--name <n>] [-a] [--debug]`
pub async fn run(
    selections: Vec<String>,
    preset: Option<String>,
    name: Option<String>,
    attach: bool,
    _debug: bool,
) -> i32 {
    let Some((config_path, config)) = super::load_config(false) else {
        return 1;
    };

    // Determine what to start.
    let parsed_selections = if let Some(ref preset_name) = preset {
        match graph::expand_preset(preset_name, &config) {
            Ok(sels) => match graph::resolve_selections(&sels, &config) {
                Ok(resolved) => resolved,
                Err(e) => {
                    output::print_error(&format!("Invalid preset: {e}"), false);
                    return 1;
                }
            },
            Err(e) => {
                output::print_error(&format!("Unknown preset: {e}"), false);
                return 1;
            }
        }
    } else if selections.is_empty() {
        match handle_no_selections(&config) {
            Some(sels) => sels,
            None => return 1,
        }
    } else {
        let raw: Result<Vec<NodeSelection>, _> = selections
            .iter()
            .map(|s| graph::parse_selection(s))
            .collect();
        match raw {
            Ok(parsed) => match graph::resolve_selections(&parsed, &config) {
                Ok(resolved) => resolved,
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

    let project_root = veld_core::config::project_root(&config_path);
    let run_name = match name {
        Some(ref n) => n.clone(),
        None => generate_run_name(&project_root),
    };
    let run_name_str = run_name.as_str();

    // Build the orchestrator.
    let foreground = attach && is_tty();
    let mut orchestrator = Orchestrator::new(config_path.clone(), config);
    orchestrator.set_debug(_debug);
    orchestrator.set_foreground(foreground);

    // Set up live progress channel.
    let (progress_tx, progress_rx) = mpsc::unbounded_channel::<ProgressEvent>();
    orchestrator.set_progress_sender(progress_tx);
    let tty = is_tty();
    let progress_handle = tokio::spawn(render_progress(progress_rx, tty));

    eprintln!(
        "{} Starting environment {}...",
        output::bold("veld"),
        output::bold(&format!("'{run_name_str}'")),
    );
    eprintln!();

    // Run start with Ctrl+C interception so we can clean up on interrupt.
    let start_result = tokio::select! {
        result = orchestrator.start(&parsed_selections, run_name_str) => result,
        _ = tokio::signal::ctrl_c() => {
            orchestrator.close_progress_sender();
            let _ = progress_handle.await;
            eprintln!();
            output::print_info("Interrupted — stopping partially started environment...");
            match orchestrator.stop(run_name_str).await {
                Ok(_) => output::print_success(&format!("Environment '{}' cleaned up.", run_name_str)),
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
                follow_logs_until_interrupt(&targets, &project_root, run_name_str).await;

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
                    let log_path =
                        logging::log_file(&project_root, run_name_str, node, variant);
                    if let Ok(raw_lines) = logging::tail_lines(&log_path, 40).await {
                        let merged = logging::merge_continuation_lines(raw_lines);
                        let start = merged.len().saturating_sub(20);
                        let tail = &merged[start..];
                        if !tail.is_empty() {
                            eprintln!();
                            eprintln!(
                                "  {}",
                                output::dim(&format!("Last log lines from {node}:{variant}:"))
                            );
                            eprintln!();
                            for line in tail {
                                let content = line
                                    .find("] ")
                                    .map(|i| &line[i + 2..])
                                    .unwrap_or(line);
                                eprintln!("    {}", output::dim(content));
                            }
                            eprintln!();
                            eprintln!(
                                "  {}",
                                output::dim(&format!("Full log: {}", log_path.display()))
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
async fn render_progress(mut rx: mpsc::UnboundedReceiver<ProgressEvent>, tty: bool) {
    let mut ctx = TtyProgressCtx::new();

    while let Some(event) = rx.recv().await {
        if tty {
            render_progress_tty(&event, &mut ctx);
        } else {
            // NDJSON for non-TTY / agent mode.
            if let Ok(json) = serde_json::to_string(&event) {
                println!("{json}");
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
        ProgressEvent::HealthCheckPhase {
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
        ProgressEvent::HealthCheckAttempt {
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
        ProgressEvent::HealthCheckPassed {
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
                output::dim("skipped (verify passed)"),
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
                // Strip timestamp prefix for readability.
                let content = line.find("] ").map(|i| &line[i + 2..]).unwrap_or(line);
                let _ = ctx
                    .multi
                    .println(format!("  {label} {}", output::dim(content)));
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
    }
}

/// Tail all log files, printing timestamped lines with node labels, until Ctrl+C.
async fn follow_logs_until_interrupt(
    targets: &[(String, String)],
    project_root: &std::path::Path,
    run_name: &str,
) {
    let mut positions: HashMap<PathBuf, u64> = HashMap::new();

    // Initialize positions to current file sizes (skip historical output).
    for (node_name, variant) in targets {
        let log_path = logging::log_file(project_root, run_name, node_name, variant);
        if let Ok(metadata) = tokio::fs::metadata(&log_path).await {
            positions.insert(log_path, metadata.len());
        }
    }

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                for (node_name, variant) in targets {
                    let log_path = logging::log_file(project_root, run_name, node_name, variant);
                    let pos = positions.get(&log_path).copied().unwrap_or(0);

                    let metadata = match tokio::fs::metadata(&log_path).await {
                        Ok(m) => m,
                        Err(_) => continue,
                    };

                    let file_len = metadata.len();

                    // Handle file truncation/rotation.
                    if file_len < pos {
                        positions.insert(log_path.clone(), 0);
                        continue;
                    }
                    if file_len == pos {
                        continue;
                    }

                    let mut file = match tokio::fs::File::open(&log_path).await {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    file.seek(std::io::SeekFrom::Start(pos)).await.ok();

                    let mut reader = BufReader::new(file);
                    let mut new_pos = pos;
                    let mut line = String::new();

                    loop {
                        line.clear();
                        match reader.read_line(&mut line).await {
                            Ok(0) => break,
                            Ok(n) => {
                                new_pos += n as u64;
                                let trimmed = line.trim_end();
                                if !trimmed.is_empty() {
                                    let label = output::cyan(
                                        &format!("{node_name}:{variant}"),
                                    );
                                    println!("{label} {trimmed}");
                                }
                            }
                            Err(_) => break,
                        }
                    }

                    positions.insert(log_path, new_pos);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                return;
            }
        }
    }
}

/// Handle the case where no selections or preset were given.
fn handle_no_selections(config: &VeldConfig) -> Option<Vec<NodeSelection>> {
    let mut preset_names: Vec<String> = config
        .presets
        .as_ref()
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();
    preset_names.sort();

    if is_tty() && !preset_names.is_empty() {
        match interactive_preset_selector(&preset_names) {
            Some(selected) => match graph::expand_preset(&selected, config) {
                Ok(sels) => match graph::resolve_selections(&sels, config) {
                    Ok(resolved) => Some(resolved),
                    Err(e) => {
                        output::print_error(&format!("{e}"), false);
                        None
                    }
                },
                Err(e) => {
                    output::print_error(&format!("{e}"), false);
                    None
                }
            },
            None => {
                output::print_info("Cancelled.");
                None
            }
        }
    } else if is_tty() {
        interactive_node_variant_picker(config)
    } else {
        let node_names: Vec<String> = config.nodes.keys().cloned().collect();
        let payload = serde_json::json!({
            "error": "No selections provided",
            "nodes": node_names,
            "presets": preset_names,
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        None
    }
}

/// Simple interactive preset selector for TTY mode.
fn interactive_preset_selector(presets: &[String]) -> Option<String> {
    use std::io::{self, BufRead, Write};

    println!("{}", output::bold("Available presets:"));
    println!();
    for (i, p) in presets.iter().enumerate() {
        println!("  {} {}", output::dim(&format!("[{}]", i + 1)), p);
    }
    println!();
    print!("Select a preset (1-{}): ", presets.len());
    io::stdout().flush().ok()?;

    let stdin = io::stdin();
    let line = stdin.lock().lines().next()?.ok()?;
    let idx: usize = line.trim().parse().ok()?;
    if idx == 0 || idx > presets.len() {
        return None;
    }
    Some(presets[idx - 1].clone())
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

        if variant_cfg.step_type != StepType::StartServer {
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
