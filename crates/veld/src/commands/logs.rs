use veld_core::config;
use veld_core::logging;
use veld_core::state::ProjectState;

use crate::output;

/// `veld logs [--name <n>] [--node <n>] [--lines <n>] [--since <d>] [--json]`
pub async fn run(
    name: Option<String>,
    node: Option<String>,
    lines: usize,
    since: Option<String>,
    json: bool,
) -> i32 {
    if !super::require_setup(json).await {
        return 1;
    }

    let Some((config_path, _cfg)) = super::load_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let project_state = match ProjectState::load(&project_root) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), json);
            return 1;
        }
    };

    let run_name = name.as_deref().unwrap_or("default");

    let run_state = match project_state.get_run(run_name) {
        Some(r) => r,
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), json);
            return 1;
        }
    };

    // Determine which nodes to show logs for.
    let mut targets: Vec<(&str, &str)> = Vec::new();
    for ns in run_state.nodes.values() {
        if let Some(ref filter_node) = node {
            if ns.node_name != *filter_node {
                continue;
            }
        }
        targets.push((&ns.node_name, &ns.variant));
    }
    targets.sort();

    if targets.is_empty() {
        output::print_info("No matching nodes found.");
        return 0;
    }

    let since_duration = since.as_deref().and_then(parse_duration);

    let mut all_output: Vec<serde_json::Value> = Vec::new();

    for (node_name, variant) in &targets {
        let log_path = logging::log_file(&project_root, run_name, node_name, variant);
        if !log_path.exists() {
            continue;
        }

        let log_lines = if let Some(secs) = since_duration {
            let dur = chrono::Duration::seconds(secs as i64);
            match logging::lines_since(&log_path, dur).await {
                Ok(l) => l,
                Err(e) => {
                    output::print_error(
                        &format!("Failed to read log for {node_name}:{variant}: {e}"),
                        json,
                    );
                    continue;
                }
            }
        } else {
            match logging::tail_lines(&log_path, lines).await {
                Ok(l) => l,
                Err(e) => {
                    output::print_error(
                        &format!("Failed to read log for {node_name}:{variant}: {e}"),
                        json,
                    );
                    continue;
                }
            }
        };

        if json {
            for line in &log_lines {
                all_output.push(logging::line_to_json(line, run_name, node_name, variant));
            }
        } else {
            for line in &log_lines {
                let label = output::cyan(&format!("{node_name}:{variant}"));
                println!("{label} {line}");
            }
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&all_output).unwrap());
    }

    0
}

/// Parse a human-friendly duration string like "5m", "1h", "30s" into seconds.
fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, suffix) = s.split_at(s.len().saturating_sub(1));
    let multiplier: u64 = match suffix {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return s.parse().ok(),
    };
    num.parse::<u64>().ok().map(|n| n * multiplier)
}
