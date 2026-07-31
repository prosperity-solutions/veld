//! `veld stats` — the detailed resource view.
//!
//! `veld status` answers "is it up", with one CPU and one memory number per
//! node. This command answers "what is it *doing* with the machine": the memory
//! breakdown by page class, the split across a node's subprocesses, and history
//! over a window. It reads the samples the daemon's stats sampler already stores
//! (same source as `veld status` and the management UI) rather than probing
//! processes itself, so it works the same whether or not the daemon is currently
//! up — a dead daemon shows as stale samples, which is information rather than
//! an error.

use std::str::FromStr;

use veld_core::config;
use veld_core::stats::{MemoryMetric, ProcessSample, ProcessStats, StatsBucket, StatsWindow};

use crate::output;

/// How many buckets to aggregate a history window into for the terminal.
/// A sparkline wider than this stops fitting an 80-column terminal.
const HISTORY_POINTS: u32 = 60;

/// `veld stats` arguments, flattened into the CLI so the flag definitions live
/// next to the code that reads them.
#[derive(Debug, clap::Args)]
pub struct StatsArgs {
    /// Name of the run to inspect.
    #[arg(long)]
    pub name: Option<String>,

    /// Limit to one node, as `node` or `node:variant`.
    #[arg(long)]
    pub node: Option<String>,

    /// Break each node down by subprocess.
    #[arg(long)]
    pub processes: bool,

    /// Show a history sparkline over --window. Needed to see a trend: a single
    /// reading shows how big a figure is, not whether it is growing.
    #[arg(long)]
    pub history: bool,

    /// Graph CPU instead of memory in --history.
    #[arg(long)]
    pub cpu: bool,

    /// History window: a duration (`30s`, `15m`, `2h`) or plain seconds.
    #[arg(long, default_value = "15m")]
    pub window: String,

    /// Which memory figure to show. `footprint` (default) is the only one that
    /// sums correctly over a process tree; `private_dirty` is the heap, so a
    /// rising `private_dirty --history` is what a leak looks like; `resident` is
    /// RSS. Also: private_clean, shared_dirty, shared_clean, swap, wired,
    /// virtual.
    #[arg(long, default_value = "footprint")]
    pub memory: String,

    /// Show a column for every memory metric this platform reports.
    #[arg(long)]
    pub all_metrics: bool,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// What `--history` plots. Memory and CPU are different units, so they are
/// different graphs rather than two lines sharing one axis.
#[derive(Debug, Clone, Copy)]
enum Dimension {
    Memory(MemoryMetric),
    Cpu,
}

impl Dimension {
    fn label(&self) -> String {
        match self {
            Dimension::Memory(m) => m.as_str().to_owned(),
            Dimension::Cpu => "cpu".to_owned(),
        }
    }

    /// This dimension's value for one bucket, `None` where the platform doesn't
    /// report it (memory page classes only — CPU is always available).
    fn read(&self, b: &StatsBucket) -> Option<f64> {
        match self {
            Dimension::Memory(m) => m.read(b.memory_bytes, &b.memory).map(|v| v as f64),
            Dimension::Cpu => Some(b.cpu_percent as f64),
        }
    }

    /// Format a value in this dimension's own unit.
    fn format(&self, v: f64) -> String {
        match self {
            Dimension::Memory(_) => output::fmt_bytes(v as u64),
            Dimension::Cpu => output::fmt_cpu(v as f32),
        }
    }
}

/// One node's data, assembled once and then rendered by whichever formatter.
struct NodeStatsRow {
    key: String,
    node: String,
    variant: String,
    stats: ProcessStats,
    fresh: bool,
    processes: Vec<ProcessSample>,
    history: Vec<StatsBucket>,
}

pub async fn run(args: StatsArgs) -> i32 {
    let json = args.json;
    let metric = match MemoryMetric::from_str(&args.memory) {
        Ok(m) => m,
        Err(e) => {
            output::print_error(&e, json);
            return 1;
        }
    };
    let window_secs = match parse_window(&args.window) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&e, json);
            return 1;
        }
    };

    let Some((config_path, _cfg)) = super::parse_config(json) else {
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
    let Some(run_name) = super::resolve_run_name(args.name, &project_state, true, json) else {
        return 1;
    };
    if project_state.get_run(&run_name).is_none() {
        output::print_error(&format!("Run '{run_name}' not found."), json);
        return 1;
    }

    let latest = match db.latest_node_stats(&project_root, &run_name) {
        Ok(l) => l,
        Err(e) => {
            output::print_error(&format!("Failed to load stats: {e}"), json);
            return 1;
        }
    };

    let now = chrono::Utc::now();
    let window = StatsWindow::for_points(
        now - chrono::Duration::seconds(window_secs),
        now,
        HISTORY_POINTS,
    );

    let mut rows: Vec<NodeStatsRow> = Vec::new();
    for (key, stats) in latest {
        if !matches_node_filter(&key, args.node.as_deref()) {
            continue;
        }
        let (node, variant) = split_key(&key);
        let processes = if args.processes {
            db.latest_process_tree(&project_root, &run_name, &key)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let history = if args.history {
            db.node_stats_buckets(&project_root, &run_name, &key, window)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        rows.push(NodeStatsRow {
            fresh: stats.is_fresh(now),
            key,
            node,
            variant,
            stats,
            processes,
            history,
        });
    }
    // Deterministic order: `latest_node_stats` returns a HashMap.
    rows.sort_by(|a, b| a.key.cmp(&b.key));

    if json {
        print_json(
            &rows,
            &run_name,
            metric,
            window,
            args.history,
            args.processes,
        );
        return 0;
    }

    if rows.is_empty() {
        if args.node.is_some() {
            output::print_info(&format!(
                "No samples for that node in run '{run_name}'. `veld stats` lists every sampled node when --node is omitted."
            ));
        } else {
            output::print_info(&format!(
                "No resource samples for run '{run_name}' yet. The daemon samples every 5s while a run is up — check `veld doctor` if this persists."
            ));
        }
        return 0;
    }

    if args.cpu && !args.history {
        // The flag only selects which dimension --history graphs, so on its own
        // it does nothing. Say so rather than printing a memory table that looks
        // like it honoured the request.
        output::print_info("--cpu selects the dimension for --history; add --history to graph it.");
    }

    print_human(&rows, metric, args.all_metrics, window);
    if args.processes {
        for row in &rows {
            print_process_tree(row, metric);
        }
    }
    if args.history {
        let dim = if args.cpu {
            Dimension::Cpu
        } else {
            Dimension::Memory(metric)
        };
        print_history(
            &rows,
            dim,
            window,
            args.processes,
            &db,
            &project_root,
            &run_name,
        );
    }
    if rows.iter().any(|r| !r.fresh) {
        output::print_info(
            "Values marked stale are older than the sampler's interval — the node exited or the daemon stopped sampling (`veld doctor`).",
        );
    }
    0
}

/// Parse a `--window` value: a duration suffixed `s`/`m`/`h`, or plain seconds.
fn parse_window(raw: &str) -> Result<i64, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("--window cannot be empty".to_owned());
    }
    let (digits, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        // No suffix → seconds, so `--window 900` works.
        _ => (s, 1),
    };
    let n: i64 = digits
        .parse()
        .map_err(|_| format!("invalid --window '{raw}' (expected e.g. 30s, 15m, 2h)"))?;
    if n <= 0 {
        return Err(format!("--window must be positive, got '{raw}'"));
    }
    Ok(n * mult)
}

/// Whether a node key matches a `--node` filter. The filter may name the node
/// alone (`web`, matching every variant) or a full key (`web:local`).
fn matches_node_filter(key: &str, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => key == f || split_key(key).0 == f,
    }
}

/// Split a `"node:variant"` key. A key with no colon is a node with an empty
/// variant, which is how the state stores a variant-less node.
fn split_key(key: &str) -> (String, String) {
    match key.split_once(':') {
        Some((n, v)) => (n.to_owned(), v.to_owned()),
        None => (key.to_owned(), String::new()),
    }
}

/// The metrics these samples actually carry, totals first. Derived from the data
/// so a macOS run doesn't advertise page classes it cannot report.
fn available_metrics(rows: &[NodeStatsRow]) -> Vec<MemoryMetric> {
    MemoryMetric::ALL
        .iter()
        .copied()
        .filter(|m| rows.iter().any(|r| r.stats.memory_metric(*m).is_some()))
        .collect()
}

/// A metric's value, or a dim `-` when this platform doesn't report it. Never a
/// zero — an absent metric that renders as `0 B` reads as "this node uses no
/// private memory", which is a different and false claim.
fn metric_cell(value: Option<u64>) -> String {
    match value {
        Some(v) => output::fmt_bytes(v),
        None => output::dim("-"),
    }
}

fn print_human(
    rows: &[NodeStatsRow],
    metric: MemoryMetric,
    all_metrics: bool,
    window: StatsWindow,
) {
    let _ = window;
    let metrics: Vec<MemoryMetric> = if all_metrics {
        available_metrics(rows)
    } else {
        vec![metric]
    };

    let mut headers: Vec<String> = vec!["NODE".into(), "VARIANT".into(), "CPU".into()];
    headers.extend(metrics.iter().map(|m| m.label().to_owned()));
    headers.push("PROCS".into());
    headers.push("CPU TIME".into());

    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let mut cells = vec![
                r.node.clone(),
                r.variant.clone(),
                if r.fresh {
                    output::fmt_cpu(r.stats.cpu_percent)
                } else {
                    output::dim(&format!("{} stale", output::fmt_cpu(r.stats.cpu_percent)))
                },
            ];
            cells.extend(
                metrics
                    .iter()
                    .map(|m| metric_cell(r.stats.memory_metric(*m))),
            );
            cells.push(r.stats.process_count.to_string());
            cells.push(output::fmt_cpu_time(r.stats.cpu_seconds));
            cells
        })
        .collect();

    let header_refs: Vec<&str> = headers.iter().map(|h| h.as_str()).collect();
    output::print_table(&header_refs, &table);

    if !all_metrics && !available_metrics(rows).iter().any(|m| m.is_page_class()) {
        // Saying so beats leaving the reader to wonder whether their node really
        // has no private memory.
        output::print_info(
            "This platform reports totals only (no private/shared page split); pass --all-metrics to see what is available.",
        );
    }
}

fn print_process_tree(row: &NodeStatsRow, metric: MemoryMetric) {
    println!();
    println!(
        "{} {}",
        output::bold(&row.key),
        output::dim(&format!(
            "— {} processes, {} cpu, {} {}",
            row.stats.process_count,
            output::fmt_cpu(row.stats.cpu_percent),
            metric_cell(row.stats.memory_metric(metric)),
            metric.as_str(),
        ))
    );
    if row.processes.is_empty() {
        output::print_info(
            "No per-process samples for this node — they are pruned sooner than the aggregates (2h).",
        );
        return;
    }
    let headers = [
        "PID",
        "NAME",
        "CPU",
        metric.label(),
        "RSS",
        "CPU TIME",
        "COMMAND",
    ];
    let table: Vec<Vec<String>> = row
        .processes
        .iter()
        .map(|p| {
            vec![
                p.pid.to_string(),
                // Indent by tree depth. The parent may be missing from the list
                // (the sampler caps how many processes it records), so depth is
                // the only reliable shape signal.
                format!("{}{}", "  ".repeat(p.depth as usize), p.name),
                output::fmt_cpu(p.cpu_percent),
                metric_cell(p.memory_metric(metric)),
                output::fmt_bytes(p.memory_bytes),
                output::fmt_cpu_time(p.cpu_seconds),
                p.cmd.clone().unwrap_or_else(|| output::dim("-")),
            ]
        })
        .collect();
    output::print_table(&headers, &table);
    if row.processes.len() < row.stats.process_count as usize {
        output::print_info(&format!(
            "{} of {} processes shown — the sampler records the heaviest per node.",
            row.processes.len(),
            row.stats.process_count
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn print_history(
    rows: &[NodeStatsRow],
    dim: Dimension,
    window: StatsWindow,
    per_process: bool,
    db: &veld_core::db::Db,
    project_root: &std::path::Path,
    run_name: &str,
) {
    println!();
    println!(
        "{} {}",
        output::bold(&format!("History — {}", dim.label())),
        output::dim(&format!(
            "({}s window, {}s buckets)",
            (window.end - window.start).num_seconds(),
            window.bucket_secs
        ))
    );
    for row in rows {
        print_series_line(&row.key, &row.history, dim);
        if !per_process {
            continue;
        }
        let series = db
            .process_stats_buckets(project_root, run_name, &row.key, window)
            .unwrap_or_default();
        for s in &series {
            print_series_line(&format!("  {} ({})", s.name, s.pid), &s.buckets, dim);
        }
    }
}

/// One label + sparkline + range line. A window with no samples says so rather
/// than drawing an empty axis.
fn print_series_line(label: &str, buckets: &[StatsBucket], dim: Dimension) {
    if buckets.is_empty() {
        println!("  {label:<28} {}", output::dim("no samples in window"));
        return;
    }
    // Buckets are omitted where nothing was sampled, so lay them out on the
    // bucket ordinal: a gap must render as a gap, not as the series closing up.
    let first = buckets[0].bucket_start;
    let slots = buckets
        .last()
        .map(|b| ((b.bucket_start - first).num_seconds() / window_step(buckets)) as usize + 1)
        .unwrap_or(0)
        .min(HISTORY_POINTS as usize);
    let mut values: Vec<Option<f64>> = vec![None; slots];
    for b in buckets {
        let idx = ((b.bucket_start - first).num_seconds() / window_step(buckets)) as usize;
        if idx < slots {
            values[idx] = dim.read(b);
        }
    }
    let present: Vec<f64> = values.iter().flatten().copied().collect();
    if present.is_empty() {
        println!(
            "  {label:<28} {}",
            output::dim("metric not reported on this platform")
        );
        return;
    }
    let min = present.iter().copied().fold(f64::MAX, f64::min);
    let max = present.iter().copied().fold(0.0f64, f64::max);
    println!(
        "  {label:<28} {}  {}",
        output::sparkline(&values),
        output::dim(&format!("{} – {}", dim.format(min), dim.format(max)))
    );
}

/// The spacing between consecutive buckets, in seconds. Derived from the data
/// rather than passed in so this helper works for the per-process series too,
/// which share the node window's bucket width.
fn window_step(buckets: &[StatsBucket]) -> i64 {
    buckets
        .windows(2)
        .map(|w| (w[1].bucket_start - w[0].bucket_start).num_seconds())
        .filter(|s| *s > 0)
        .min()
        // A single bucket has no spacing; any positive step lays it out at 0.
        .unwrap_or(1)
}

fn print_json(
    rows: &[NodeStatsRow],
    run_name: &str,
    metric: MemoryMetric,
    window: StatsWindow,
    include_history: bool,
    include_processes: bool,
) {
    let nodes: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let mut v = serde_json::json!({
                "key": r.key,
                "node": r.node,
                "variant": r.variant,
                "live": r.fresh,
                "cpu_percent": r.stats.cpu_percent,
                "cpu_seconds": r.stats.cpu_seconds,
                "process_count": r.stats.process_count,
                // `resident` is named explicitly here rather than reusing
                // ProcessStats's `memory_bytes`, so an agent reading this
                // payload doesn't have to know which of the two is RSS.
                "resident": r.stats.memory_bytes,
                "memory": r.stats.memory,
                "sampled_at": r.stats.sampled_at.timestamp_millis(),
            });
            if include_processes {
                // Always an array once `--processes` was passed, even when empty:
                // the agent-facing contract in skills/veld/SKILL.md says the key
                // is there, and a node whose per-process rows aged out of the
                // shorter retention window would otherwise make
                // `node.processes.length` throw rather than read 0.
                v["processes"] = serde_json::to_value(&r.processes).unwrap_or_default();
            }
            if include_history {
                v["history"] = serde_json::to_value(&r.history).unwrap_or_default();
            }
            v
        })
        .collect();

    let mut out = serde_json::json!({
        "run": run_name,
        "memory_metric": metric.as_str(),
        "available_metrics": available_metrics(rows)
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>(),
        "nodes": nodes,
    });
    if include_history {
        out["window"] = serde_json::json!({
            "start": window.start.timestamp_millis(),
            "end": window.end.timestamp_millis(),
            "bucket_secs": window.bucket_secs,
        });
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_accepts_durations_and_bare_seconds() {
        assert_eq!(parse_window("30s").unwrap(), 30);
        assert_eq!(parse_window("15m").unwrap(), 900);
        assert_eq!(parse_window("2h").unwrap(), 7200);
        assert_eq!(parse_window("900").unwrap(), 900);
        assert_eq!(parse_window(" 5m ").unwrap(), 300);
    }

    #[test]
    fn window_rejects_nonsense() {
        for bad in ["", "abc", "-5m", "0", "5d", "m"] {
            assert!(parse_window(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn window_rejects_multibyte_suffixes_without_panicking() {
        // `parse_window` slices `&s[..s.len() - 1]` to strip the unit. That is a
        // BYTE index, so it would panic mid-codepoint if a multi-byte character
        // could reach it. It cannot: the slicing arms only match ASCII 's'/'m'/
        // 'h', and everything else falls through to the no-suffix branch, which
        // does not slice. Pinned because the natural "tidy-up" — matching on a
        // `char` set that includes a non-ASCII unit like 'µ' — reintroduces it.
        for bad in ["15µ", "µ", "１５m", "5h🙂", "15 m", "s", "-", "1.5h"] {
            assert!(parse_window(bad).is_err(), "{bad:?} should be rejected");
        }
        // A multi-byte char before an ASCII suffix parses its digits and fails on
        // them, rather than panicking on the slice.
        assert!(parse_window("15µs").is_err());
    }

    #[test]
    fn node_filter_matches_node_or_full_key() {
        assert!(matches_node_filter("web:local", None));
        assert!(matches_node_filter("web:local", Some("web")));
        assert!(matches_node_filter("web:local", Some("web:local")));
        assert!(!matches_node_filter("web:local", Some("web:prod")));
        assert!(!matches_node_filter("web:local", Some("api")));
    }

    fn test_bucket(footprint: u64, cpu: f32) -> StatsBucket {
        StatsBucket {
            bucket_start: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
            samples: 1,
            cpu_percent: cpu,
            cpu_peak: cpu,
            process_count: 1.0,
            memory_bytes: footprint * 2,
            memory: veld_core::stats::MemoryBreakdown {
                footprint,
                virtual_bytes: footprint * 10,
                private_dirty: Some(footprint / 2),
                ..Default::default()
            },
            footprint_peak: footprint,
        }
    }

    #[test]
    fn dimension_reads_and_formats_in_its_own_unit() {
        let b = test_bucket(2048, 37.4);
        let mem = Dimension::Memory(MemoryMetric::Footprint);
        assert_eq!(mem.read(&b), Some(2048.0));
        assert_eq!(mem.format(2048.0), "2 KB");
        assert_eq!(mem.label(), "footprint");

        let cpu = Dimension::Cpu;
        // CPU is stored as f32 and widened to f64 for the plot, so compare with
        // slack rather than for equality.
        assert!((cpu.read(&b).unwrap() - 37.4).abs() < 1e-4);
        assert_eq!(cpu.format(37.4), "37%");
        assert_eq!(cpu.label(), "cpu");
    }

    #[test]
    fn cpu_is_always_readable_but_a_page_class_may_not_be() {
        // The reason `read` returns Option at all: a metric the platform cannot
        // measure must render as a gap, not as zero. CPU has no such case.
        let mut b = test_bucket(1024, 5.0);
        b.memory.private_dirty = None;
        assert_eq!(Dimension::Memory(MemoryMetric::PrivateDirty).read(&b), None);
        assert_eq!(Dimension::Cpu.read(&b), Some(5.0));
    }

    #[test]
    fn split_key_handles_a_missing_variant() {
        assert_eq!(split_key("web:local"), ("web".into(), "local".into()));
        assert_eq!(split_key("web"), ("web".into(), String::new()));
    }
}
