use std::collections::HashMap;
use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};

use veld_core::config;
use veld_core::logging;
use veld_core::state::ProjectState;

use crate::output;

/// `veld logs [--name <n>] [--node <n>] [--lines <n>] [--since <d>] [-f] [--json]`
pub async fn run(
    name: Option<String>,
    node: Option<String>,
    lines: usize,
    since: Option<String>,
    follow: bool,
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

    let run_name = match super::resolve_run_name(name, &project_state, true, json) {
        Some(n) => n,
        None => return 1,
    };
    let run_name = run_name.as_str();

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

    // Track file positions after historical read so follow mode can continue
    // exactly where the snapshot left off (no gap, no duplicates).
    let mut positions: HashMap<PathBuf, u64> = HashMap::new();
    let mut all_output: Vec<serde_json::Value> = Vec::new();

    for (node_name, variant) in &targets {
        let log_path = logging::log_file(&project_root, run_name, node_name, variant);
        if !log_path.exists() {
            positions.insert(log_path, 0);
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
                let entry = logging::line_to_json(line, run_name, node_name, variant);
                if follow {
                    // In follow+json mode, emit as NDJSON immediately.
                    println!("{}", serde_json::to_string(&entry).unwrap());
                } else {
                    all_output.push(entry);
                }
            }
        } else {
            for line in &log_lines {
                let label = output::cyan(&format!("{node_name}:{variant}"));
                println!("{label} {line}");
            }
        }

        // Record current file size so follow mode starts right after.
        if let Ok(metadata) = tokio::fs::metadata(&log_path).await {
            positions.insert(log_path, metadata.len());
        }
    }

    if json && !follow {
        println!("{}", serde_json::to_string_pretty(&all_output).unwrap());
    }

    // Follow mode: tail all log files continuously.
    if follow {
        if let Err(e) = follow_logs(&targets, &project_root, run_name, json, positions).await {
            output::print_error(&format!("Follow error: {e}"), json);
            return 1;
        }
    }

    0
}

/// Tail log files continuously, printing new lines as they appear.
async fn follow_logs(
    targets: &[(&str, &str)],
    project_root: &std::path::Path,
    run_name: &str,
    json: bool,
    mut positions: HashMap<PathBuf, u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                for (node_name, variant) in targets {
                    let log_path = logging::log_file(project_root, run_name, node_name, variant);
                    let pos = positions.get(&log_path).copied().unwrap_or(0);

                    // Check if file exists and has new content.
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

                    // Read new content from the last position.
                    let mut file = match tokio::fs::File::open(&log_path).await {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    file.seek(std::io::SeekFrom::Start(pos)).await?;

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
                                    if json {
                                        let entry = logging::line_to_json(
                                            trimmed, run_name, node_name, variant,
                                        );
                                        println!("{}", serde_json::to_string(&entry).unwrap());
                                    } else {
                                        let label = output::cyan(
                                            &format!("{node_name}:{variant}"),
                                        );
                                        println!("{label} {trimmed}");
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }

                    positions.insert(log_path, new_pos);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                return Ok(());
            }
        }
    }
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
