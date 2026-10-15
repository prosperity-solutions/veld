//! Read-only queries over a project's runs: what `veld orchestrator status` and `veld orchestrator logs`
//! show. Everything here is synchronous (SQLite, `kill(pid, 0)`); the daemon runs
//! it through its blocking pool. Rendering — colours, tables, time zones — stays
//! with the face.

use std::collections::HashMap;
use std::path::Path;

use veld_core::db::{Db, LogFilter, LogRow, LogStream, stream_is_per_node};
use veld_core::process;
use veld_core::state::{GlobalRegistry, NodeStatus, RunState, RunStatus};

use crate::api::{
    LogLine, LogsPage, LogsQuery, NodeOutputs, RunningRun, SourceFilter, StatusReport,
};
use crate::preflight::resolve_run_name;

fn open_db() -> Result<Db, String> {
    Db::open().map_err(|e| format!("Failed to open veld database: {e}"))
}

// ---------------------------------------------------------------------------
// running runs
// ---------------------------------------------------------------------------

/// Every environment, across all projects, whose latest run is `running`.
///
/// Reads the registry's stored status without probing PIDs: the daemon's health
/// scan is what flips a dead run to `crashed`, and the overview is a glance, not
/// a liveness check (`status` does that per run). `starting` and `stopping` are
/// left out on purpose: they are transitions, not something that is up.
pub fn running_runs() -> Result<Vec<RunningRun>, String> {
    let registry = open_db()?
        .registry()
        .map_err(|e| format!("Failed to load registry: {e}"))?;
    Ok(running_in(&registry))
}

/// [`running_runs`] over an already-loaded registry. Sorted, because the registry
/// is a `HashMap` and an overview that reshuffles between two calls reads as churn.
fn running_in(registry: &GlobalRegistry) -> Vec<RunningRun> {
    let mut out: Vec<RunningRun> = registry
        .projects
        .values()
        .flat_map(|entry| {
            entry
                .runs
                .values()
                .filter(|run| run.status == RunStatus::Running)
                .map(|run| RunningRun {
                    project_name: entry.project_name.clone(),
                    project_root: entry.project_root.clone(),
                    name: run.name.clone(),
                    run_id: run.run_id.to_string(),
                    status: serde_json::to_value(run.status)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                })
        })
        .collect();
    out.sort_by(|a, b| {
        (&a.project_name, &a.project_root, &a.name).cmp(&(
            &b.project_name,
            &b.project_root,
            &b.name,
        ))
    });
    out
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// The state of one environment's latest run, with liveness checked and the
/// latest resource sample per node. `name: None` infers the environment the way
/// `veld orchestrator status` always did (a sole live one, else a sole one at all).
pub fn status(
    project_root: &Path,
    name: Option<String>,
    outputs: bool,
) -> Result<StatusReport, String> {
    let db = open_db()?;
    let project_state = db
        .load_project_state(project_root)
        .map_err(|e| format!("Failed to load state: {e}"))?;

    let (run_name, note) = resolve_run_name(name, &project_state, true)?;
    let run = project_state
        .get_run(&run_name)
        .ok_or_else(|| format!("Run '{run_name}' not found."))?
        .clone();

    let effective = compute_effective_statuses(&run);

    // Latest per-node resource stats (recorded by the daemon's stats sampler),
    // keyed by node key ("node:variant"). Stale samples (a dead node, or a
    // daemon that has stopped writing) are dropped so we never present a frozen
    // reading as live.
    let mut stats = db
        .latest_node_stats(project_root, &run_name)
        .unwrap_or_default();
    let now = chrono::Utc::now();
    stats.retain(|_, s| s.is_fresh(now));

    let outputs = outputs.then(|| node_outputs(&run));
    Ok(StatusReport {
        name: run_name,
        note,
        run,
        effective,
        stats,
        outputs,
    })
}

/// Check PID liveness for each node and return effective statuses.
/// If a node is supposedly running but the process is dead, mark it as Failed.
pub fn compute_effective_statuses(run_state: &RunState) -> HashMap<String, NodeStatus> {
    let mut result = HashMap::new();
    for (key, ns) in &run_state.nodes {
        let effective = match ns.status {
            NodeStatus::Healthy | NodeStatus::Starting | NodeStatus::HealthChecking => {
                match ns.pid {
                    Some(pid) if !process::is_alive(pid) => NodeStatus::Failed,
                    _ => ns.status.clone(),
                }
            }
            _ => ns.status.clone(),
        };
        result.insert(key.clone(), effective);
    }
    result
}

/// Outputs per node (nodes with none are left out), nodes and keys sorted,
/// sensitive values masked.
fn node_outputs(run: &RunState) -> Vec<NodeOutputs> {
    let mut keys: Vec<&String> = run.nodes.keys().collect();
    keys.sort();
    let mut out = Vec::new();
    for key in keys {
        let ns = &run.nodes[key];
        if ns.outputs.is_empty() {
            continue;
        }
        let mut okeys: Vec<&String> = ns.outputs.keys().collect();
        okeys.sort();
        let values = okeys
            .into_iter()
            .map(|k| {
                let v = if ns.sensitive_keys.contains(k) {
                    "***".to_owned()
                } else {
                    ns.outputs[k].clone()
                };
                (k.clone(), v)
            })
            .collect();
        out.push(NodeOutputs {
            node: ns.node_name.clone(),
            variant: ns.variant.clone(),
            values,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

impl SourceFilter {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "server" => Some(Self::Server),
            "setup" | "teardown" => Some(Self::Setup),
            "internal" | "veld" => Some(Self::Internal),
            _ => None,
        }
    }

    /// Which stored streams this filter includes.
    ///
    /// NOTE: this is a hand-maintained mapping onto `veld_core::db::LogStream`
    /// — when a new stream variant is added there, list it here (under `All`
    /// at minimum) or `veld orchestrator logs` will never show it.
    fn streams(&self) -> Vec<&'static str> {
        match self {
            // "all" intentionally includes the setup/debug streams too — they
            // were separate files before and are part of the run's story.
            Self::All => vec![
                LogStream::Server.as_str(),
                LogStream::Setup.as_str(),
                LogStream::Debug.as_str(),
                LogStream::Internal.as_str(),
            ],
            // Node output only. `Setup` used to be listed here as well, back
            // when it had no writers at all and the inclusion was inert; now
            // that project steps write to it, keeping it would mean
            // `--source server` printed project setup output too.
            Self::Server => vec![LogStream::Server.as_str()],
            // Project `setup`/`teardown` steps only — node output, including a
            // `command` node's, is on the `server` stream.
            Self::Setup => vec![LogStream::Setup.as_str()],
            Self::Internal => vec![LogStream::Internal.as_str()],
        }
    }
}

/// The stored streams `--source` selects, minus the run-level **internal**
/// stream when a specific `--node` filter is active.
///
/// Internal rows (liveness probes, recoveries) carry `node = NULL` — they are
/// daemon noise, not the node the user asked for, and under `--node` they
/// drowned the filtered node's output while debugging a crash. Setup/debug
/// stay under `--source all`: setup is a node's real step output and debug is
/// an opt-in trace, so neither is hidden by a node pick. Both the follow poll
/// set and the snapshot filter list use this so the two never disagree about
/// whether internal leaks in.
fn selected_streams(source: SourceFilter, node: Option<&str>) -> Vec<&'static str> {
    source
        .streams()
        .into_iter()
        .filter(|s| !(node.is_some() && *s == LogStream::Internal.as_str()))
        .collect()
}

/// Parse a human-friendly duration string like "5m", "1h", "30s" into seconds.
pub fn parse_duration(s: &str) -> Option<u64> {
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

fn line_of(row: LogRow) -> LogLine {
    LogLine {
        id: row.id,
        node: row.node,
        variant: row.variant,
        stream: row.stream,
        ts: row.ts,
        line: row.line,
        is_match: true,
        gap_before: false,
    }
}

/// Read a run's log rows: a historical snapshot, or (with `after_id`) the rows a
/// follower has not seen yet.
pub fn logs(q: LogsQuery) -> Result<LogsPage, String> {
    let db = open_db()?;
    let project_root = q.project_root.as_path();
    let node = q.node.as_deref();
    let following = q.after_id.is_some();

    // Resolve which run instance to read. Default: the environment's latest
    // run (a restart no longer mixes last week's lines into today's tail).
    // `--all-runs` restores the old interleaved scope and is the only way to see
    // legacy unscoped rows.
    //
    // `--run <prefix>` identifies the instance by itself, project-wide — the
    // environment name is then DERIVED from the resolved run, never resolved
    // separately (a separately-resolved name from another environment would
    // scope the query to zero rows, and a multi-live-env project would error
    // on ambiguity before the prefix was even considered).
    let mut note: Option<String> = None;
    let run_state: RunState = if let Some(ref prefix) = q.run {
        match db.get_run_by_id_prefix(project_root, prefix) {
            Ok(Some(r)) => r,
            // A followed run pruned under the follower has ended.
            Ok(None) if following => return Ok(gone_page(q.after_id.unwrap_or(0))),
            Ok(None) => {
                return Err(format!(
                    "No run matches id prefix '{prefix}' (see `veld orchestrator runs list`)."
                ));
            }
            Err(e) => return Err(format!("{e}")),
        }
    } else {
        let project_state = db
            .load_project_state(project_root)
            .map_err(|e| format!("Failed to load state: {e}"))?;
        let (run_name, n) = resolve_run_name(q.name.clone(), &project_state, true)?;
        note = n;
        if q.previous {
            match db.list_runs(project_root, Some(&run_name)) {
                Ok(history) if history.len() >= 2 => history[1].clone(),
                Ok(_) => {
                    return Err(format!(
                        "Environment '{run_name}' has no previous run recorded."
                    ));
                }
                Err(e) => return Err(format!("Failed to load run history: {e}")),
            }
        } else {
            project_state
                .get_run(&run_name)
                .ok_or_else(|| format!("Run '{run_name}' not found."))?
                .clone()
        }
    };
    let run_name = run_state.name.as_str();
    let run_scope: Option<String> = if q.all_runs {
        None
    } else {
        Some(run_state.run_id.to_string())
    };
    let search_lower = q.search.as_deref().map(|s| s.to_lowercase());
    let matches = |line: &str| {
        search_lower
            .as_deref()
            .is_none_or(|n| line.to_lowercase().contains(n))
    };

    let mut page = LogsPage {
        run: run_state.name.clone(),
        run_id: run_state.run_id.to_string(),
        short_id: run_state.short_id(),
        live: run_state.is_live(),
        outcome: run_state.outcome_label(),
        rows: Vec::new(),
        last_id: 0,
        time_zone: db.logs_time_zone().as_str().to_owned(),
        note,
    };

    if let Some(after_id) = q.after_id {
        // Follow mode: per-node streams honor `--node`, run-level streams
        // (internal/debug/setup) never do — internal/debug rows have
        // `node = NULL`, so a node filter would silently drop them live even
        // though the snapshot shows them. The stream sets are disjoint, so no
        // row is polled twice.
        let (per_node, run_level): (Vec<&'static str>, Vec<&'static str>) =
            selected_streams(q.source, node)
                .into_iter()
                .partition(|s| stream_is_per_node(s));
        let mut filters: Vec<LogFilter> = Vec::new();
        if !per_node.is_empty() {
            filters.push(LogFilter {
                node: q.node.clone(),
                variant: None,
                streams: Some(per_node),
                run_id: run_scope.clone(),
            });
        }
        if !run_level.is_empty() {
            filters.push(LogFilter {
                node: None,
                variant: None,
                streams: Some(run_level),
                run_id: run_scope.clone(),
            });
        }
        let mut rows: Vec<LogRow> = Vec::new();
        for filter in &filters {
            // A transient read error skips this tick's rows for that filter.
            if let Ok(mut r) = db.logs_after_id(project_root, run_name, filter, after_id) {
                rows.append(&mut r);
            }
        }
        rows.sort_by_key(|r| r.id);
        page.last_id = rows.iter().map(|r| r.id).max().unwrap_or(after_id);
        page.rows = rows
            .into_iter()
            .filter(|r| matches(&r.line))
            .map(line_of)
            .collect();
        return Ok(page);
    }

    // Build the per-source list, mirroring the old one-file-per-source layout:
    // `--lines N` means N lines per source (per node+stream), not N total —
    // otherwise one chatty node pushes every other node out of the window.
    let mut source_filters: Vec<LogFilter> = Vec::new();
    for stream in selected_streams(q.source, node) {
        if stream_is_per_node(stream) {
            // Per-node streams: one source per (node, variant).
            let mut targets: Vec<(&str, &str)> = run_state
                .nodes
                .values()
                .filter(|ns| node.is_none_or(|f| ns.node_name == f))
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
            // the node: None filter matches them too). `selected_streams` has
            // already dropped internal under a node filter, mirroring follow
            // mode.
            source_filters.push(LogFilter {
                node: None,
                variant: None,
                streams: Some(vec![stream]),
                run_id: run_scope.clone(),
            });
        }
    }

    // Historical snapshot: read each source, then interleave by timestamp. The
    // follow watermark is taken BEFORE the reads: a line written to an
    // already-read source while later sources are still being read would
    // otherwise fall between snapshot and follow and be lost.
    page.last_id = db.max_log_id().unwrap_or(0);
    let since_secs = q.since.as_deref().and_then(parse_duration);
    let mut rows: Vec<LogRow> = Vec::new();
    for sf in &source_filters {
        let result = if let Some(secs) = since_secs {
            let cutoff = chrono::Utc::now() - chrono::Duration::seconds(secs as i64);
            db.logs_since(project_root, run_name, sf, cutoff)
        } else {
            db.tail_logs(project_root, run_name, sf, q.lines)
        };
        match result {
            Ok(mut r) => rows.append(&mut r),
            Err(e) => return Err(format!("Failed to read logs: {e}")),
        }
    }
    rows.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));

    // When searching, keep matches + surrounding context lines.
    let visible: Vec<bool> = if search_lower.is_some() {
        let mut visible = vec![false; rows.len()];
        for (idx, r) in rows.iter().enumerate() {
            if matches(&r.line) {
                let start = idx.saturating_sub(q.context);
                let end = (idx + q.context + 1).min(rows.len());
                for v in &mut visible[start..end] {
                    *v = true;
                }
            }
        }
        visible
    } else {
        vec![true; rows.len()]
    };
    let mut prev_idx: Option<usize> = None;
    for (idx, row) in rows.into_iter().enumerate() {
        if !visible[idx] {
            continue;
        }
        let gap = prev_idx.is_some_and(|prev| idx > prev + 1);
        prev_idx = Some(idx);
        let is_match = matches(&row.line);
        let mut line = line_of(row);
        line.is_match = is_match;
        line.gap_before = gap;
        page.rows.push(line);
    }
    Ok(page)
}

/// The answer for a followed run that no longer exists.
fn gone_page(after_id: i64) -> LogsPage {
    LogsPage {
        run: String::new(),
        run_id: String::new(),
        short_id: String::new(),
        live: false,
        outcome: String::new(),
        rows: Vec::new(),
        last_id: after_id,
        time_zone: veld_core::db::LogTimeZone::default().as_str().to_owned(),
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_runs_keeps_only_running() {
        use std::collections::HashMap;
        use veld_core::state::{RegistryEntry, RegistryRunInfo};

        let run = |name: &str, status| RegistryRunInfo {
            run_id: uuid::Uuid::new_v4(),
            name: name.to_owned(),
            status,
            urls: HashMap::new(),
            hostnames: Vec::new(),
        };
        let mut registry = GlobalRegistry::default();
        registry.projects.insert(
            "/p".to_owned(),
            RegistryEntry {
                project_root: "/p".into(),
                project_name: "p".to_owned(),
                runs: [
                    ("dev".to_owned(), run("dev", RunStatus::Running)),
                    ("old".to_owned(), run("old", RunStatus::Stopped)),
                    ("bad".to_owned(), run("bad", RunStatus::Crashed)),
                    ("new".to_owned(), run("new", RunStatus::Starting)),
                    ("end".to_owned(), run("end", RunStatus::Stopping)),
                ]
                .into(),
            },
        );
        let got = running_in(&registry);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "dev");
        assert_eq!(got[0].project_name, "p");
        assert_eq!(got[0].status, "running");
    }

    fn names(streams: Vec<&'static str>) -> Vec<String> {
        streams.into_iter().map(|s| s.to_owned()).collect()
    }

    #[test]
    fn all_sources_without_node_filter_keep_internal() {
        let streams = selected_streams(SourceFilter::All, None);
        assert!(streams.contains(&LogStream::Internal.as_str()));
        assert!(streams.contains(&LogStream::Setup.as_str()));
        assert!(streams.contains(&LogStream::Server.as_str()));
    }

    #[test]
    fn a_node_filter_drops_internal_but_keeps_setup_and_debug() {
        let streams = names(selected_streams(SourceFilter::All, Some("web")));
        assert!(!streams.contains(&LogStream::Internal.as_str().to_owned()));
        // Real project-step output and opt-in trace survive a node pick.
        assert!(streams.contains(&LogStream::Setup.as_str().to_owned()));
        assert!(streams.contains(&LogStream::Debug.as_str().to_owned()));
        assert!(streams.contains(&LogStream::Server.as_str().to_owned()));
    }

    #[test]
    fn explicit_internal_source_is_empty_under_a_node_filter() {
        // `--node X --source internal` has nothing to show: internal is hidden
        // the moment a node is picked, consistently with `--source all`.
        assert_eq!(
            names(selected_streams(SourceFilter::Internal, Some("web"))),
            Vec::<String>::new()
        );
        assert_eq!(
            names(selected_streams(SourceFilter::Internal, None)),
            vec![LogStream::Internal.as_str().to_owned()]
        );
    }

    #[test]
    fn server_source_never_includes_internal() {
        assert!(
            selected_streams(SourceFilter::Server, None)
                .iter()
                .all(|s| *s != LogStream::Internal.as_str())
        );
    }

    #[test]
    fn durations_parse_with_a_unit_or_as_bare_seconds() {
        assert_eq!(parse_duration("30s"), Some(30));
        assert_eq!(parse_duration("5m"), Some(300));
        assert_eq!(parse_duration("1h"), Some(3600));
        assert_eq!(parse_duration("2d"), Some(172_800));
        assert_eq!(parse_duration("45"), Some(45));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("soon"), None);
    }

    #[test]
    fn source_spellings_match_the_flag_and_the_wire() {
        assert_eq!(SourceFilter::from_str("veld"), Some(SourceFilter::Internal));
        assert_eq!(
            SourceFilter::from_str("teardown"),
            Some(SourceFilter::Setup)
        );
        assert_eq!(SourceFilter::from_str("nope"), None);
        let s: SourceFilter = serde_json::from_str("\"veld\"").unwrap();
        assert_eq!(s, SourceFilter::Internal);
        assert_eq!(
            serde_json::to_string(&SourceFilter::Setup).unwrap(),
            "\"setup\""
        );
    }
}
