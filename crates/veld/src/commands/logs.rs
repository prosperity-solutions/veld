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
    /// Address a specific past run by id prefix.
    pub run: Option<String>,
    /// The run before the latest one.
    pub previous: bool,
    /// Every run under the name interleaved (pre-v3 behavior, includes
    /// legacy unscoped rows).
    pub all_runs: bool,
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
        run,
        previous,
        all_runs,
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

    // Resolve which run instance to read. Default: the environment's latest
    // run (fixes the old generation-interleaving: a restart no longer mixes
    // last week's lines into today's tail). `--all-runs` restores the old
    // interleaved scope and is the only way to see legacy unscoped rows.
    //
    // `--run <prefix>` identifies the instance by itself, project-wide — the
    // environment name is then DERIVED from the resolved run, never resolved
    // separately (a separately-resolved name from another environment would
    // scope the query to zero rows, and a multi-live-env project would error
    // on ambiguity before the prefix was even considered).
    let target_run: veld_core::state::RunState = if let Some(ref prefix) = run {
        match db.get_run_by_id_prefix(&project_root, prefix) {
            Ok(Some(r)) => r,
            Ok(None) => {
                output::print_error(
                    &format!("No run matches id prefix '{prefix}' (see `veld runs`)."),
                    json,
                );
                return 1;
            }
            Err(e) => {
                output::print_error(&format!("{e}"), json);
                return 1;
            }
        }
    } else {
        let run_name = match super::resolve_run_name(name, &project_state, true, json) {
            Some(n) => n,
            None => return 1,
        };
        if previous {
            match db.list_runs(&project_root, Some(&run_name)) {
                Ok(history) if history.len() >= 2 => history[1].clone(),
                Ok(_) => {
                    output::print_error(
                        &format!("Environment '{run_name}' has no previous run recorded."),
                        json,
                    );
                    return 1;
                }
                Err(e) => {
                    output::print_error(&format!("Failed to load run history: {e}"), json);
                    return 1;
                }
            }
        } else {
            match project_state.get_run(&run_name) {
                Some(r) => r.clone(),
                None => {
                    output::print_error(&format!("Run '{run_name}' not found."), json);
                    return 1;
                }
            }
        }
    };
    let run_state = &target_run;
    let run_name = run_state.name.as_str();
    let run_scope: Option<String> = if all_runs {
        None
    } else {
        Some(run_state.run_id.to_string())
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
            run_id: run_scope.clone(),
        });
    }
    if !run_level_streams.is_empty() {
        follow_filters.push(LogFilter {
            node: None,
            variant: None,
            streams: Some(run_level_streams),
            run_id: run_scope.clone(),
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
                    run_id: run_scope.clone(),
                });
            }
        } else {
            // Run-level streams (internal/debug; setup rows carry a node but
            // the node: None filter matches them too).
            source_filters.push(LogFilter {
                node: None,
                variant: None,
                streams: Some(vec![stream]),
                run_id: run_scope.clone(),
            });
        }
    }

    // Historical snapshot: read each source, then interleave by timestamp.
    // The follow watermark is taken BEFORE the reads: a line written to an
    // already-read source while later sources are still being read would
    // otherwise fall between snapshot and follow and be lost. Rows the
    // snapshot shows beyond this watermark are remembered so follow skips
    // them (no duplicates either).
    let follow_from = db.max_log_id().unwrap_or(0);
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

    // Ids the snapshot already showed past the follow watermark — follow
    // skips exactly these.
    let already_shown: std::collections::HashSet<i64> = rows
        .iter()
        .map(|r| r.id)
        .filter(|id| *id > follow_from)
        .collect();

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

    // Follow mode: poll for new rows continuously. Following an ended run
    // (already ended, or ending mid-follow) prints history and exits 0 —
    // polling forever on a run that can't produce lines would hang an agent
    // waiting on a crashed environment. The note goes to stderr so stdout
    // stays pure log payload. `--all-runs` has no single run to watch and
    // keeps the old poll-forever behavior.
    if follow {
        if !all_runs && !run_state.is_live() {
            eprintln!(
                "run {} has ended ({}) — nothing to follow",
                run_state.short_id(),
                run_state.outcome_label()
            );
            return 0;
        }
        follow_logs(
            &db,
            &project_root,
            run_name,
            if all_runs {
                None
            } else {
                Some(run_state.run_id)
            },
            &follow_filters,
            follow_from,
            already_shown,
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

/// Poll the database for new rows, printing them as they appear, until
/// Ctrl+C — or until the followed run reaches a terminal status (one final
/// drain tick, then exit; a run that ended produces no further lines and an
/// agent must not block on it forever). `watch_run` is `None` under
/// `--all-runs`, which has no single run to watch.
/// The filters' stream sets are disjoint, so no row matches twice; rows from
/// all filters are merged in id order per tick. `already_shown` holds ids past
/// the watermark that the historical snapshot already printed.
#[allow(clippy::too_many_arguments)]
async fn follow_logs(
    db: &Db,
    project_root: &std::path::Path,
    run_name: &str,
    watch_run: Option<veld_core::uuid::Uuid>,
    filters: &[LogFilter],
    mut last_id: i64,
    mut already_shown: std::collections::HashSet<i64>,
    json: bool,
    search: &Option<String>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
    // Check the run's status only every ~2s (every 10th tick) — the status
    // probe is cheap but needless at 5Hz.
    let mut tick: u64 = 0;
    let mut final_drain = false;
    let mut drain_ticks: u64 = 0;

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
                let quiet_tick = rows.is_empty();
                for row in rows {
                    last_id = last_id.max(row.id);
                    if already_shown.remove(&row.id) {
                        continue; // the snapshot already printed this line
                    }
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

                if final_drain {
                    // Drain until a quiet tick (detached `_log` writers can
                    // trail the finalize by more than one 200ms tick — the
                    // trailing lines are often the crash output itself),
                    // bounded so a still-chattering wrapper can't hold the
                    // exit forever.
                    drain_ticks += 1;
                    if quiet_tick || drain_ticks >= 25 {
                        return;
                    }
                    continue;
                }
                tick += 1;
                if tick % 10 == 0 {
                    if let Some(run_id) = watch_run {
                        let ended = match db.run_status_by_id(&run_id) {
                            Ok(Some(s)) => !s.is_live(),
                            Ok(None) => true, // pruned while following
                            Err(_) => false,  // transient DB error — keep going
                        };
                        if ended {
                            eprintln!("run has ended — stopping follow");
                            final_drain = true;
                        }
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
