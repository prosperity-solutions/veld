use veld_core::config;
use veld_core::db::{Db, LogFilter, LogRow, LogStream};
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

    if project_state.get_run(run_name).is_none() {
        output::print_error(&format!("Run '{run_name}' not found."), json);
        return 1;
    }

    let filter = LogFilter {
        node: node.clone(),
        variant: None,
        streams: Some(source.streams()),
    };

    // Historical snapshot.
    let since_duration = since.as_deref().and_then(parse_duration);
    let rows = if let Some(secs) = since_duration {
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(secs as i64);
        db.logs_since(&project_root, run_name, &filter, cutoff)
    } else {
        db.tail_logs(&project_root, run_name, &filter, lines)
    };
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            output::print_error(&format!("Failed to read logs: {e}"), json);
            return 1;
        }
    };

    // Snapshot point for follow mode: continue exactly after the last row we
    // showed (no gap, no duplicates). When nothing matched yet, start at the
    // current global maximum instead of dumping the entire history.
    let last_id = rows
        .last()
        .map(|r| r.id)
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
            &filter,
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
async fn follow_logs(
    db: &Db,
    project_root: &std::path::Path,
    run_name: &str,
    filter: &LogFilter,
    mut last_id: i64,
    json: bool,
    search: &Option<String>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let rows = match db.logs_after_id(project_root, run_name, filter, last_id) {
                    Ok(rows) => rows,
                    Err(_) => continue,
                };
                for row in rows {
                    last_id = row.id;
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
