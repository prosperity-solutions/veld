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
    if !super::require_setup(false).await {
        return 1;
    }

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

    let run_name = match name {
        Some(ref n) => n.clone(),
        None => generate_run_name(),
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

    match orchestrator.start(&parsed_selections, run_name_str).await {
        Ok(run_state) => {
            // Drop the progress sender so the renderer can finish.
            orchestrator.close_progress_sender();
            let _ = progress_handle.await;

            // Print outputs for nodes that have non-trivial outputs.
            let mut node_keys: Vec<&String> = run_state.nodes.keys().collect();
            node_keys.sort();
            let skip_keys = ["port", "url", "exit_code"];
            for key in &node_keys {
                let ns = &run_state.nodes[*key];
                let mut output_keys: Vec<&String> = ns
                    .outputs
                    .keys()
                    .filter(|k| !skip_keys.contains(&k.as_str()))
                    .collect();
                output_keys.sort();
                if !output_keys.is_empty() {
                    let label = format!("{}:{}", ns.node_name, ns.variant);
                    for okey in output_keys {
                        let val = if ns.sensitive_keys.contains(okey) {
                            "***".to_owned()
                        } else {
                            ns.outputs[okey].clone()
                        };
                        eprintln!(
                            "  {} {} {}={}",
                            output::dim(&label),
                            output::dim("↳"),
                            output::dim(okey),
                            output::dim(&val),
                        );
                    }
                }
            }

            // Print URLs on success.
            println!();
            let urls: Vec<(&str, &str)> = run_state
                .nodes
                .values()
                .filter_map(|ns| ns.url.as_ref().map(|u| (ns.node_name.as_str(), u.as_str())))
                .collect();

            if urls.is_empty() {
                output::print_success("Environment started (no URLs exposed).");
            } else {
                output::print_success("Environment started. URLs:");
                println!();
                for (node, url) in &urls {
                    println!("  {} {}", output::cyan(node), url);
                }
            }

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
            output::print_error(&format!("Startup failed: {e}"), false);
            // Best-effort teardown.
            let _stop_result = orchestrator.stop(run_name_str).await;
            1
        }
    }
}

/// Render live progress events from the orchestrator.
///
/// TTY mode: Uses `\r` carriage returns for in-place updates, finalizing with `\n`.
/// Non-TTY/JSON mode: Emits NDJSON for agent consumption.
async fn render_progress(mut rx: mpsc::UnboundedReceiver<ProgressEvent>, tty: bool) {
    while let Some(event) = rx.recv().await {
        if tty {
            render_progress_tty(&event);
        } else {
            // NDJSON for non-TTY / agent mode.
            if let Ok(json) = serde_json::to_string(&event) {
                println!("{json}");
            }
        }
    }
}

/// Render a single progress event for TTY output.
fn render_progress_tty(event: &ProgressEvent) {
    match event {
        ProgressEvent::PlanResolved {
            total_nodes,
            stages,
        } => {
            eprintln!(
                "  {} {total_nodes} node(s) in {stages} stage(s)",
                output::dim("plan:"),
            );
            eprintln!();
        }
        ProgressEvent::NodeStarting {
            node,
            variant,
            index,
            total,
        } => {
            let label = format!("{node}:{variant}");
            eprint!(
                "\x1b[2K\r{}",
                output::step(*index, *total, &output::pad_right(&label, 30)),
            );
            eprint!(" {}", output::dim("starting..."));
        }
        ProgressEvent::PortAllocated {
            node: _,
            variant: _,
            port,
        } => {
            eprint!(" {}", output::dim(&format!("port {port}")));
        }
        ProgressEvent::HealthCheckPhase {
            node: _,
            variant: _,
            phase,
            description,
        } => {
            eprint!(
                " {}",
                output::dim(&format!("[phase {phase}: {description}]"))
            );
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
            let label = format!("{node}:{variant}");
            let detail = match url {
                Some(u) => u.clone(),
                None => "healthy".to_owned(),
            };
            let elapsed = format!("{elapsed_ms}ms");
            eprintln!(
                "\x1b[2K\r  {} {} {}",
                output::checkmark(),
                output::pad_right(&label, 30),
                output::dim(&format!("{detail} ({elapsed})")),
            );
        }
        ProgressEvent::NodeSkipped { node, variant } => {
            let label = format!("{node}:{variant}");
            eprintln!(
                "\x1b[2K\r  {} {} {}",
                output::dim("~"),
                output::pad_right(&label, 30),
                output::dim("skipped (verify passed)"),
            );
        }
        ProgressEvent::NodeFailed {
            node,
            variant,
            error,
        } => {
            let label = format!("{node}:{variant}");
            eprintln!(
                "\x1b[2K\r  {} {} {}",
                output::cross(),
                output::pad_right(&label, 30),
                output::red(error),
            );
        }
        ProgressEvent::CommandRunning {
            node: _,
            variant: _,
        } => {
            eprint!(" {}", output::dim("running..."));
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
    let preset_names: Vec<String> = config
        .presets
        .as_ref()
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();

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
