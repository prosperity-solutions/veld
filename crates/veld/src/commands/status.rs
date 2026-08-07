use veld_core::config;
use veld_core::process;
use veld_core::state::{NodeStatus, RunStatus};

use crate::output;

/// `veld status [--name <n>] [--outputs] [--json]`
pub async fn run(name: Option<String>, show_outputs: bool, json: bool) -> i32 {
    let Some((config_path, cfg)) = super::parse_config(json) else {
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

    let run_state = match project_state.get_run(run_name) {
        Some(r) => r.clone(),
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), json);
            return 1;
        }
    };

    // Check PID liveness for each node and compute effective statuses.
    let effective_statuses = compute_effective_statuses(&run_state);

    // Latest per-node resource stats (recorded by the daemon's stats sampler);
    // keyed by node key ("node:variant"). Empty when the daemon hasn't sampled
    // yet or isn't running. Stale samples (a dead node, or a daemon that has
    // stopped writing) are dropped so we never present a frozen reading as live.
    let mut node_stats = db
        .latest_node_stats(&project_root, run_name)
        .unwrap_or_default();
    let stats_now = chrono::Utc::now();
    node_stats.retain(|_, s| s.is_fresh(stats_now));

    if json {
        // Build a modified run state with effective statuses for JSON output.
        let mut run_for_json = run_state.clone();
        for (key, effective) in &effective_statuses {
            if let Some(ns) = run_for_json.nodes.get_mut(*key) {
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
                // page shared inside the tree once per process. `veld stats` breaks
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
            let Some(((first_port, first_url), rest)) = routed.split_first() else {
                row.push(String::new());
                rows.push(row);
                continue;
            };
            debug_assert!(first_port.is_none(), "routed_urls puts the primary first");
            row.push((*first_url).to_owned());
            rows.push(row);
            for (port, url) in rest {
                // Leading cells blank: the node's identity and stats belong to
                // the node, and repeating them would read as several nodes.
                rows.push(vec![
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    match port {
                        Some(port) => format!("{url}  ({port})"),
                        None => (*url).to_owned(),
                    },
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

        // Show outputs per node when --outputs is passed.
        if show_outputs {
            println!();
            println!("{}", output::bold("Outputs:"));
            let mut any = false;
            for key in &node_keys {
                let ns = &run_state.nodes[*key];
                if ns.outputs.is_empty() {
                    continue;
                }
                any = true;
                println!();
                println!(
                    "  {}",
                    output::cyan(&format!("{}:{}", ns.node_name, ns.variant))
                );
                let mut okeys: Vec<&String> = ns.outputs.keys().collect();
                okeys.sort();
                for okey in okeys {
                    let val = if ns.sensitive_keys.contains(okey) {
                        "***".to_owned()
                    } else {
                        ns.outputs[okey].clone()
                    };
                    println!("    {} = {}", output::dim(okey), val);
                }
            }
            if !any {
                println!("  {}", output::dim("No outputs recorded."));
            }
        }
    }

    0
}

/// Check PID liveness for each node and return effective statuses.
/// If a node is supposedly running but the process is dead, mark it as Failed.
fn compute_effective_statuses(
    run_state: &veld_core::state::RunState,
) -> std::collections::HashMap<&str, NodeStatus> {
    let mut result = std::collections::HashMap::new();
    for (key, ns) in &run_state.nodes {
        let effective = match ns.status {
            NodeStatus::Healthy | NodeStatus::Starting | NodeStatus::HealthChecking => {
                if let Some(pid) = ns.pid {
                    if process::is_alive(pid) {
                        ns.status.clone()
                    } else {
                        NodeStatus::Failed
                    }
                } else {
                    ns.status.clone()
                }
            }
            _ => ns.status.clone(),
        };
        result.insert(key.as_str(), effective);
    }
    result
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
