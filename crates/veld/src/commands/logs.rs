use std::collections::HashMap;
use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};

use veld_core::config;
use veld_core::logging;
use veld_core::state::ProjectState;

use crate::output;

/// Log source filter for `--source` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFilter {
    All,
    Server,
    Client,
}

impl SourceFilter {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "server" => Some(Self::Server),
            "client" => Some(Self::Client),
            _ => None,
        }
    }
}

/// `veld logs [--name <n>] [--node <n>] [--lines <n>] [--since <d>] [-f] [--json] [--source <s>] [--search <term>] [--context <n>]`
pub async fn run(
    name: Option<String>,
    node: Option<String>,
    lines: usize,
    since: Option<String>,
    follow: bool,
    json: bool,
    source: SourceFilter,
    search: Option<String>,
    context_lines: usize,
) -> i32 {
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

    // Build list of (path, node, variant, source_label) for each log file to read.
    let mut log_sources: Vec<(PathBuf, &str, &str, &str)> = Vec::new();
    for (node_name, variant) in &targets {
        if source != SourceFilter::Client {
            log_sources.push((
                logging::log_file(&project_root, run_name, node_name, variant),
                node_name,
                variant,
                "server",
            ));
        }
        if source != SourceFilter::Server {
            log_sources.push((
                logging::client_log_file(&project_root, run_name, node_name, variant),
                node_name,
                variant,
                "client",
            ));
        }
    }

    // Collect all lines with metadata for interleaved sorting.
    struct LogEntry {
        line: String,
        node: String,
        variant: String,
        source: String,
    }
    let mut all_entries: Vec<LogEntry> = Vec::new();

    for (log_path, node_name, variant, src) in &log_sources {
        if !log_path.exists() {
            positions.insert(log_path.clone(), 0);
            continue;
        }

        let log_lines = if let Some(secs) = since_duration {
            let dur = chrono::Duration::seconds(secs as i64);
            match logging::lines_since(log_path, dur).await {
                Ok(l) => logging::merge_continuation_lines(l),
                Err(e) => {
                    output::print_error(
                        &format!("Failed to read log for {node_name}:{variant} ({src}): {e}"),
                        json,
                    );
                    continue;
                }
            }
        } else {
            match logging::tail_lines(log_path, lines).await {
                Ok(l) => logging::merge_continuation_lines(l),
                Err(e) => {
                    output::print_error(
                        &format!("Failed to read log for {node_name}:{variant} ({src}): {e}"),
                        json,
                    );
                    continue;
                }
            }
        };

        for line in log_lines {
            all_entries.push(LogEntry {
                line,
                node: node_name.to_string(),
                variant: variant.to_string(),
                source: src.to_string(),
            });
        }

        // Record current file size so follow mode starts right after.
        if let Ok(metadata) = tokio::fs::metadata(log_path).await {
            positions.insert(log_path.clone(), metadata.len());
        }
    }

    // Sort all entries by parsed timestamp for correct interleaving.
    // Server logs use +00:00 suffix, client logs use Z suffix with different
    // fractional-second precision — lexicographic sort would be wrong.
    all_entries.sort_by(|a, b| {
        let ts_a = logging::extract_timestamp(&a.line)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        let ts_b = logging::extract_timestamp(&b.line)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        match (ts_a, ts_b) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });

    let search_lower = search.as_deref().map(|s| s.to_lowercase());

    // When searching, filter entries to matches + surrounding context lines.
    let visible_indices: Vec<usize> = if let Some(ref needle) = search_lower {
        let match_indices: Vec<usize> = all_entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.line.to_lowercase().contains(needle.as_str()))
            .map(|(i, _)| i)
            .collect();
        let mut visible = vec![false; all_entries.len()];
        for &idx in &match_indices {
            let start = idx.saturating_sub(context_lines);
            let end = (idx + context_lines + 1).min(all_entries.len());
            for i in start..end {
                visible[i] = true;
            }
        }
        (0..all_entries.len()).filter(|i| visible[*i]).collect()
    } else {
        (0..all_entries.len()).collect()
    };

    let mut prev_idx: Option<usize> = None;
    for &idx in &visible_indices {
        // Print separator when there's a gap between visible lines.
        if let Some(prev) = prev_idx {
            if idx > prev + 1 && !json {
                println!("{}", output::dim("..."));
            }
        }
        prev_idx = Some(idx);

        let entry = &all_entries[idx];
        if json {
            let j = logging::line_to_json(
                &entry.line,
                run_name,
                &entry.node,
                &entry.variant,
                &entry.source,
            );
            if follow {
                println!("{}", serde_json::to_string(&j).unwrap());
            } else {
                all_output.push(j);
            }
        } else {
            let label = if entry.source == "client" {
                output::cyan(&format!("{}:{}:client", entry.node, entry.variant))
            } else {
                output::cyan(&format!("{}:{}", entry.node, entry.variant))
            };
            let is_match = search_lower
                .as_ref()
                .map_or(true, |n| entry.line.to_lowercase().contains(n.as_str()));
            if is_match {
                println!("{label} {}", entry.line);
            } else {
                println!("{label} {}", output::dim(&entry.line));
            }
        }
    }

    if json && !follow {
        println!("{}", serde_json::to_string_pretty(&all_output).unwrap());
    }

    // Follow mode: tail all log files continuously.
    if follow {
        if let Err(e) = follow_logs(
            &targets,
            &project_root,
            run_name,
            json,
            source,
            positions,
            &search_lower,
        )
        .await
        {
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
    source: SourceFilter,
    mut positions: HashMap<PathBuf, u64>,
    search: &Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));

    // Build list of (path, node, variant, source_label) to follow.
    let mut follow_sources: Vec<(PathBuf, String, String, String)> = Vec::new();
    for (node_name, variant) in targets {
        if source != SourceFilter::Client {
            follow_sources.push((
                logging::log_file(project_root, run_name, node_name, variant),
                node_name.to_string(),
                variant.to_string(),
                "server".to_string(),
            ));
        }
        if source != SourceFilter::Server {
            follow_sources.push((
                logging::client_log_file(project_root, run_name, node_name, variant),
                node_name.to_string(),
                variant.to_string(),
                "client".to_string(),
            ));
        }
    }

    loop {
        tokio::select! {
            _ = interval.tick() => {
                for (log_path, node_name, variant, src) in &follow_sources {
                    let pos = positions.get(log_path).copied().unwrap_or(0);

                    let metadata = match tokio::fs::metadata(log_path).await {
                        Ok(m) => m,
                        Err(_) => continue,
                    };

                    let file_len = metadata.len();

                    if file_len < pos {
                        positions.insert(log_path.clone(), 0);
                        continue;
                    }

                    if file_len == pos {
                        continue;
                    }

                    let mut file = match tokio::fs::File::open(log_path).await {
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
                                    // Skip lines that don't match search filter.
                                    if let Some(needle) = search {
                                        if !trimmed.to_lowercase().contains(needle.as_str()) {
                                            continue;
                                        }
                                    }
                                    if json {
                                        let entry = logging::line_to_json(
                                            trimmed, run_name, node_name, variant, src,
                                        );
                                        println!("{}", serde_json::to_string(&entry).unwrap());
                                    } else {
                                        let label = if src == "client" {
                                            output::cyan(
                                                &format!("{node_name}:{variant}:client"),
                                            )
                                        } else {
                                            output::cyan(
                                                &format!("{node_name}:{variant}"),
                                            )
                                        };
                                        println!("{label} {trimmed}");
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }

                    positions.insert(log_path.clone(), new_pos);
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
