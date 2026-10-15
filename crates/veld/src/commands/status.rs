use veld_core::config;
use veld_core::state::{NodeStatus, RunStatus};
use veld_engine::api::StatusReport;

use crate::engine_client::{ClientError, EngineClient};
use crate::output;

/// Print a failed daemon call the way every command does and pick the exit
/// code: 75 while an update holds the lock, 1 otherwise.
pub(super) fn client_failed(e: &ClientError, json: bool) -> i32 {
    output::print_error(&e.to_string(), json);
    if e.is_update_in_progress() { 75 } else { 1 }
}

/// `veld orchestrator status [--name <n>] [--outputs] [--json]`
///
/// The engine (in the daemon) resolves the environment, checks liveness and
/// reads the stats; this renders its report.
pub async fn run(name: Option<String>, show_outputs: bool, json: bool) -> i32 {
    let Some((config_path, cfg)) = super::parse_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let client = match EngineClient::connect().await {
        Ok(c) => c,
        Err(e) => return client_failed(&e, json),
    };
    let mut query = vec![
        ("project_root", project_root.to_string_lossy().into_owned()),
        ("outputs", show_outputs.to_string()),
    ];
    if let Some(n) = name {
        query.push(("name", n));
    }
    let report: StatusReport = match client.get("/api/engine/status", &query).await {
        Ok(r) => r,
        Err(e) => return client_failed(&e, json),
    };
    let StatusReport {
        name: run_name,
        note,
        run: run_state,
        effective: effective_statuses,
        stats: node_stats,
        outputs,
    } = report;
    let run_name = run_name.as_str();
    if let (Some(note), false) = (&note, json) {
        output::print_info(note);
    }

    if json {
        // Build a modified run state with effective statuses for JSON output.
        let mut run_for_json = run_state.clone();
        for (key, effective) in &effective_statuses {
            if let Some(ns) = run_for_json.nodes.get_mut(key.as_str()) {
                ns.status = effective.clone();
            }
        }
        // If any node is dead, mark the run as degraded (failed).
        if effective_statuses
            .values()
            .any(|s| matches!(s, NodeStatus::Failed))
            && run_for_json.status == RunStatus::Running
        {
            run_for_json.status = RunStatus::Failed;
        }
        // Attach live resource stats alongside the run without polluting the
        // persisted `NodeState` type: flatten the run and add a `stats` map.
        #[derive(serde::Serialize)]
        struct StatusJson<'a> {
            #[serde(flatten)]
            run: &'a veld_core::state::RunState,
            /// Whether the run occupies the live slot. Stale URLs on a
            /// non-live run must not read as reachable.
            live: bool,
            /// Deprecated alias of `ended_at` — kept so status-parsing
            /// scripts survive the v3 rename.
            #[serde(skip_serializing_if = "Option::is_none")]
            stopped_at: Option<chrono::DateTime<chrono::Utc>>,
            #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
            stats: &'a std::collections::HashMap<String, veld_core::stats::ProcessStats>,
        }
        let payload = StatusJson {
            live: run_for_json.is_live(),
            stopped_at: run_for_json.ended_at,
            run: &run_for_json,
            stats: &node_stats,
        };
        match serde_json::to_string_pretty(&payload) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                output::print_error(&format!("JSON serialization failed: {e}"), json);
                return 1;
            }
        }
    } else {
        // Check if any nodes are dead.
        let has_dead = effective_statuses.values().any(|s| {
            matches!(s, NodeStatus::Failed)
                && run_state
                    .nodes
                    .values()
                    .any(|ns| ns.status == NodeStatus::Healthy || ns.status == NodeStatus::Starting)
        });

        let run_status_display = if has_dead && run_state.status == RunStatus::Running {
            output::red("degraded")
        } else {
            format_run_status(&run_state.status)
        };

        println!(
            "{} {}",
            output::bold("Environment:"),
            output::cyan(run_name),
        );
        println!(
            "{} {}",
            output::bold("Run:"),
            output::dim(&run_state.short_id()),
        );
        println!("{} {}", output::bold("State:"), run_status_display,);
        if let Some(label) = super::start_origin_label(
            run_state
                .graph_snapshot
                .as_ref()
                .and_then(|s| s.started_from.as_ref()),
            &cfg,
        ) {
            println!("{} {}", output::bold("Started from:"), label);
        }
        let live = run_state.is_live();
        if !live {
            // Last run's outcome — the "why did it die" line.
            println!("{} {}", output::bold("Outcome:"), run_state.outcome_label(),);
            if let Some(ended) = run_state.ended_at {
                println!(
                    "{} {}",
                    output::bold("Ended:"),
                    ended
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S"),
                );
            }
        }
        println!();

        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut node_keys: Vec<&String> = run_state.nodes.keys().collect();
        node_keys.sort();
        for key in &node_keys {
            let ns = &run_state.nodes[*key];
            let effective = effective_statuses.get(key.as_str()).unwrap_or(&ns.status);
            let status_str = if *effective != ns.status {
                // The process died — show "dead" instead of the stored status.
                format!("{} {}", output::cross(), output::red("dead"))
            } else {
                format_node_status(&ns.status)
            };
            let (cpu_str, mem_str) = match node_stats.get(key.as_str()) {
                // Footprint, not RSS: summing RSS over a process tree counts every
                // page shared inside the tree once per process. `veld orchestrator stats` breaks
                // the same number down further.
                Some(s) => (
                    output::fmt_cpu(s.cpu_percent),
                    output::fmt_bytes(s.memory.footprint),
                ),
                None => (output::dim("-"), output::dim("-")),
            };
            let mut row = vec![
                ns.node_name.clone(),
                ns.variant.clone(),
                status_str,
                cpu_str,
                mem_str,
            ];
            if !live {
                rows.push(row);
                continue;
            }
            // Every routed http port gets its own row. Not newline-joined into
            // one cell: `print_table` measures cells as single lines, so an
            // embedded newline breaks the column alignment for the whole table.
            // A node with one URL (or none) still produces exactly one row.
            let routed = ns.routed_urls();
            // Raw (`tcp`) ports get rows too, marked. They have a hostname veld
            // minted and nothing else prints, and showing them unmarked beside
            // the URLs would invite someone to open one.
            let raw = ns.raw_addresses();
            let mut extra: Vec<String> = routed
                .iter()
                .skip(1)
                .map(|(port, url)| match port {
                    Some(port) => format!("{url}  ({port})"),
                    None => (*url).to_owned(),
                })
                .collect();
            extra.extend(
                raw.iter()
                    .map(|(port, address)| format!("{address}  ({port}, tcp)")),
            );

            match routed.first() {
                Some((first_port, first_url)) => {
                    debug_assert!(first_port.is_none(), "routed_urls puts the primary first");
                    row.push((*first_url).to_owned());
                }
                // No URL at all: a portless node, or one with only raw ports —
                // then the first raw address takes the cell rather than leaving
                // it blank beside rows that do have one.
                None if !extra.is_empty() => row.push(extra.remove(0)),
                None => row.push(String::new()),
            }
            rows.push(row);
            for line in extra {
                // Leading cells blank: the node's identity and stats belong to
                // the node, and repeating them would read as several nodes.
                rows.push(vec![
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    line,
                ]);
            }
        }

        // Routes are torn down with the run — a non-live run's URLs are dead,
        // so the column is dropped rather than shown as if reachable.
        if live {
            output::print_table(&["NODE", "VARIANT", "STATUS", "CPU", "MEM", "URL"], &rows);
        } else {
            output::print_table(&["NODE", "VARIANT", "STATUS", "CPU", "MEM"], &rows);
        }

        // Show liveness/recovery details for nodes that have them.
        let has_liveness_info = run_state.nodes.values().any(|ns| {
            ns.recovery_count > 0
                || ns.consecutive_failures > 0
                || ns.last_liveness_error.is_some()
                || ns.status == NodeStatus::Unhealthy
        });
        if has_liveness_info {
            println!();
            println!("{}", output::bold("Liveness:"));
            for key in &node_keys {
                let ns = &run_state.nodes[*key];
                if ns.recovery_count == 0
                    && ns.consecutive_failures == 0
                    && ns.last_liveness_error.is_none()
                    && ns.status != NodeStatus::Unhealthy
                {
                    continue;
                }
                println!();
                println!(
                    "  {}",
                    output::cyan(&format!("{}:{}", ns.node_name, ns.variant))
                );
                if ns.consecutive_failures > 0 {
                    println!(
                        "    {} consecutive failures: {}",
                        output::yellow("!"),
                        ns.consecutive_failures
                    );
                }
                if ns.recovery_count > 0 {
                    println!("    {} recoveries: {}", output::dim("↻"), ns.recovery_count);
                }
                if let Some(ref err) = ns.last_liveness_error {
                    println!("    {} last error: {}", output::dim("→"), err);
                }
            }
        }

        // Show outputs per node when --outputs is passed (the engine masks
        // sensitive values).
        if let Some(outputs) = outputs {
            println!();
            println!("{}", output::bold("Outputs:"));
            for node in &outputs {
                println!();
                println!(
                    "  {}",
                    output::cyan(&format!("{}:{}", node.node, node.variant))
                );
                for (okey, val) in &node.values {
                    println!("    {} = {}", output::dim(okey), val);
                }
            }
            if outputs.is_empty() {
                println!("  {}", output::dim("No outputs recorded."));
            }
        }
    }

    0
}

fn format_run_status(status: &RunStatus) -> String {
    match status {
        RunStatus::Running => output::green("running"),
        RunStatus::Starting => output::yellow("starting"),
        RunStatus::Stopping => output::yellow("stopping"),
        RunStatus::Stopped => output::dim("stopped"),
        RunStatus::Failed => output::red("failed"),
        RunStatus::Crashed => output::red("crashed"),
    }
}

fn format_node_status(status: &NodeStatus) -> String {
    match status {
        NodeStatus::Healthy => format!("{} {}", output::checkmark(), output::green("healthy")),
        NodeStatus::Starting => format!("{} {}", output::yellow("~"), output::yellow("starting")),
        NodeStatus::HealthChecking => {
            format!(
                "{} {}",
                output::yellow("~"),
                output::yellow("health-checking")
            )
        }
        NodeStatus::Pending => format!("{} {}", output::dim("-"), output::dim("pending")),
        NodeStatus::Stopped => format!("{} {}", output::dim("-"), output::dim("stopped")),
        NodeStatus::Failed => format!("{} {}", output::cross(), output::red("failed")),
        NodeStatus::Skipped => format!("{} {}", output::dim("-"), output::dim("skipped")),
        NodeStatus::Unhealthy => format!("{} {}", output::cross(), output::yellow("unhealthy")),
    }
}
