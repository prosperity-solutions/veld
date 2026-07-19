use veld_core::config;
use veld_core::db::{Db, LogFilter, LogRow, LogStream, stream_is_per_node};
use veld_core::logging;

use crate::output;

/// Log source filter for `--source` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFilter {
    All,
    Server,
    Client,
    Internal,
}

impl SourceFilter {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "server" => Some(Self::Server),
            "client" => Some(Self::Client),
            "internal" | "veld" => Some(Self::Internal),
            _ => None,
        }
    }

    /// Which stored streams this filter includes.
    ///
    /// NOTE: this is a hand-maintained mapping onto `veld_core::db::LogStream`
    /// — when a new stream variant is added there, list it here (under `All`
    /// at minimum) or `veld logs` will never show it.
    fn streams(&self) -> Vec<&'static str> {
        match self {
            // "all" intentionally includes the setup/debug streams too — they
            // were separate files before and are part of the run's story.
            Self::All => vec![
                LogStream::Server.as_str(),
                LogStream::Client.as_str(),
                LogStream::Setup.as_str(),
                LogStream::Debug.as_str(),
                LogStream::Internal.as_str(),
            ],
            Self::Server => vec![LogStream::Server.as_str(), LogStream::Setup.as_str()],
            Self::Client => vec![LogStream::Client.as_str()],
            Self::Internal => vec![LogStream::Internal.as_str()],
        }
    }
}

pub struct LogsOptions {
    pub name: Option<String>,
    pub node: Option<String>,
    pub lines: usize,
    pub since: Option<String>,
    pub follow: bool,
    pub json: bool,
    pub source: SourceFilter,
    pub search: Option<String>,
    pub context_lines: usize,
}

/// `veld logs [--name <n>] [--node <n>] [--lines <n>] [--since <d>] [-f] [--json] [--source <s>] [--search <term>] [--context <n>]`
pub async fn run(opts: LogsOptions) -> i32 {
    let LogsOptions {
        name,
        node,
        lines,
        since,
        follow,
        json,
        source,
        search,
        context_lines,
    } = opts;
    let Some((config_path, _cfg)) = super::load_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let Some(db) = super::open_db(json) else {
        return 1;
    };
    let project_state = match db.load_project_state(&project_root) {
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

    let Some(run_state) = project_state.get_run(run_name) else {
        output::print_error(&format!("Run '{run_name}' not found."), json);
        return 1;
    };

    // Follow mode polls these: per-node streams honor `--node`, run-level
    // streams (internal/debug/setup) never do — internal/debug rows have
    // `node = NULL`, so a node filter would silently drop them live even
    // though the snapshot shows them. The stream sets are disjoint, so no
    // row is polled twice.
    let (per_node_streams, run_level_streams): (Vec<&'static str>, Vec<&'static str>) = source
        .streams()
        .into_iter()
        .partition(|s| stream_is_per_node(s));
    let mut follow_filters: Vec<LogFilter> = Vec::new();
    if !per_node_streams.is_empty() {
        follow_filters.push(LogFilter {
            node: node.clone(),
            variant: None,
            streams: Some(per_node_streams),
        });
    }
    if !run_level_streams.is_empty() {
        follow_filters.push(LogFilter {
            node: None,
            variant: None,
            streams: Some(run_level_streams),
        });
    }

    // Build the per-source list, mirroring the old one-file-per-source layout:
    // `--lines N` means N lines per source (per node+stream), not N total —
    // otherwise one chatty node pushes every other node out of the window.
    let mut source_filters: Vec<LogFilter> = Vec::new();
    for stream in source.streams() {
        if stream_is_per_node(stream) {
            // Per-node streams: one source per (node, variant).
            let mut targets: Vec<(&str, &str)> = run_state
                .nodes
                .values()
                .filter(|ns| node.as_deref().is_none_or(|f| ns.node_name == f))
                .map(|ns| (ns.node_name.as_str(), ns.variant.as_str()))
                .collect();
            targets.sort();
            for (node_name, variant) in targets {
                source_filters.push(LogFilter {
                    node: Some(node_name.to_owned()),
                    variant: Some(variant.to_owned()),
                    streams: Some(vec![stream]),
                });
            }
        } else {
            // Run-level streams (internal/debug; setup rows carry a node but
            // the node: None filter matches them too).
            source_filters.push(LogFilter {
                node: None,
                variant: None,
                streams: Some(vec![stream]),
            });
        }
    }

    // Historical snapshot: read each source, then interleave by timestamp.
    let since_duration = since.as_deref().and_then(parse_duration);
    let mut rows: Vec<LogRow> = Vec::new();
    for sf in &source_filters {
        let result = if let Some(secs) = since_duration {
            let cutoff = chrono::Utc::now() - chrono::Duration::seconds(secs as i64);
            db.logs_since(&project_root, run_name, sf, cutoff)
        } else {
            db.tail_logs(&project_root, run_name, sf, lines)
        };
        match result {
            Ok(mut r) => rows.append(&mut r),
            Err(e) => {
                output::print_error(&format!("Failed to read logs: {e}"), json);
                return 1;
            }
        }
    }
    rows.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));

    // Snapshot point for follow mode: continue after the highest row id we
    // showed (no duplicates). When nothing matched yet, start at the current
    // global maximum instead of dumping the entire history.
    let last_id = rows
        .iter()
        .map(|r| r.id)
        .max()
        .unwrap_or_else(|| db.max_log_id().unwrap_or(0));

    let search_lower = search.as_deref().map(|s| s.to_lowercase());

    // When searching, filter rows to matches + surrounding context lines.
    let visible_indices: Vec<usize> = if let Some(ref needle) = search_lower {
        let match_indices: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.line.to_lowercase().contains(needle.as_str()))
            .map(|(i, _)| i)
            .collect();
        let mut visible = vec![false; rows.len()];
        for &idx in &match_indices {
            let start = idx.saturating_sub(context_lines);
            let end = (idx + context_lines + 1).min(rows.len());
            for v in &mut visible[start..end] {
                *v = true;
            }
        }
        (0..rows.len()).filter(|i| visible[*i]).collect()
    } else {
        (0..rows.len()).collect()
    };

    let mut all_output: Vec<serde_json::Value> = Vec::new();
    let mut prev_idx: Option<usize> = None;
    for &idx in &visible_indices {
        // Print separator when there's a gap between visible lines.
        if let Some(prev) = prev_idx {
            if idx > prev + 1 && !json {
                println!("{}", output::dim("..."));
            }
        }
        prev_idx = Some(idx);

        let row = &rows[idx];
        if json {
            let j = logging::row_to_json(row, run_name);
            if follow {
                println!("{}", serde_json::to_string(&j).unwrap());
            } else {
                all_output.push(j);
            }
        } else {
            let is_match = search_lower
                .as_ref()
                .is_none_or(|n| row.line.to_lowercase().contains(n.as_str()));
            let text = format_row(row);
            if is_match {
                println!("{text}");
            } else {
                println!("{}", output::dim(&text));
            }
        }
    }

    if json && !follow {
        println!("{}", serde_json::to_string_pretty(&all_output).unwrap());
    }

    // Follow mode: poll for new rows continuously.
    if follow {
        follow_logs(
            &db,
            &project_root,
            run_name,
            &follow_filters,
            last_id,
            json,
            &search_lower,
        )
        .await;
    }

    0
}

/// Human-readable label + line for one log row.
fn format_row(row: &LogRow) -> String {
    let label = match (&row.node, row.stream.as_str()) {
        (Some(node), "client") => output::cyan(&format!(
            "{node}:{}:client",
            row.variant.as_deref().unwrap_or("?")
        )),
        (Some(node), _) => {
            output::cyan(&format!("{node}:{}", row.variant.as_deref().unwrap_or("?")))
        }
        (None, stream) => output::cyan(&format!("_veld:{stream}")),
    };
    format!("{label} [{}] {}", row.ts, row.line)
}

/// Poll the database for new rows, printing them as they appear, until Ctrl+C.
/// The filters' stream sets are disjoint, so no row matches twice; rows from
/// all filters are merged in id order per tick.
async fn follow_logs(
    db: &Db,
    project_root: &std::path::Path,
    run_name: &str,
    filters: &[LogFilter],
    mut last_id: i64,
    json: bool,
    search: &Option<String>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let mut rows: Vec<LogRow> = Vec::new();
                for filter in filters {
                    match db.logs_after_id(project_root, run_name, filter, last_id) {
                        Ok(mut r) => rows.append(&mut r),
                        Err(_) => continue,
                    }
                }
                rows.sort_by_key(|r| r.id);
                for row in rows {
                    last_id = last_id.max(row.id);
                    if let Some(needle) = search {
                        if !row.line.to_lowercase().contains(needle.as_str()) {
                            continue;
                        }
                    }
                    if json {
                        let entry = logging::row_to_json(&row, run_name);
                        println!("{}", serde_json::to_string(&entry).unwrap());
                    } else {
                        println!("{}", format_row(&row));
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                return;
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
