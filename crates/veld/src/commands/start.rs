use veld_core::config::VeldConfig;
use veld_core::graph::{self, NodeSelection};
use veld_core::orchestrator::Orchestrator;
use veld_core::url::slugify;

use crate::output::{self, is_tty};

/// `veld start [node:variant...] [--preset <n>] [--name <n>] [--debug]`
pub async fn run(
    selections: Vec<String>,
    preset: Option<String>,
    name: Option<String>,
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
        None => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let dir_name = cwd
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default");
            slugify(dir_name)
        }
    };
    let run_name = run_name.as_str();

    // Build the orchestrator.
    let mut orchestrator = Orchestrator::new(config_path, config);
    orchestrator.set_debug(_debug);

    println!(
        "{} Starting environment '{}'...",
        output::bold("veld"),
        run_name,
    );
    println!();

    match orchestrator.start(&parsed_selections, run_name).await {
        Ok(run_state) => {
            // Print node results.
            let mut node_keys: Vec<&String> = run_state.nodes.keys().collect();
            node_keys.sort();
            let total = node_keys.len();

            for (i, key) in node_keys.iter().enumerate() {
                let ns = &run_state.nodes[*key];
                let label = format!("{}:{}", ns.node_name, ns.variant);
                let padded = output::pad_right(&label, 30);
                let status_icon = match ns.status {
                    veld_core::state::NodeStatus::Healthy
                    | veld_core::state::NodeStatus::Skipped => output::checkmark(),
                    veld_core::state::NodeStatus::Failed => output::cross(),
                    _ => output::dim("~"),
                };
                let detail = match &ns.url {
                    Some(url) => url.clone(),
                    None => format!("{:?}", ns.status).to_lowercase(),
                };
                eprintln!(
                    "{} {status_icon} {}",
                    output::step(i + 1, total, &padded),
                    output::dim(&detail),
                );
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

            0
        }
        Err(e) => {
            output::print_error(&format!("Startup failed: {e}"), false);
            // Best-effort teardown.
            let _ = orchestrator.stop(run_name).await;
            1
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
