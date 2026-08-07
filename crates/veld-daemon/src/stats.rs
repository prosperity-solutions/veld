//! The daemon's resource-stats sampler.
//!
//! Records CPU/memory for every node of every live run, once per tick, from the
//! PID each node persisted when it spawned. The sampling itself — the
//! cross-platform `sysinfo` probing, the process-tree walk and the per-platform
//! memory detail — lives in [`veld_stats::StatsCollector`], shared with the CLI
//! so both producers of a sample mean the same thing by it.
//!
//! **This sampler sees exactly the nodes with a persisted
//! [`veld_core::state::NodeState::pid`], which is only ever the `start_server`
//! path.** A run's `command` steps (builds, installs, codegen) are sampled by
//! the CLI that spawns them, via `veld_stats::CommandStatsRecorder`, because
//! their PIDs exist only inside that process. The two producers are therefore
//! disjoint by node kind: no process is counted twice, and no node key is
//! written by both. Persisting a `command` step's PID to make it visible here
//! would break that *and* make a finished build look like a dead node to
//! `veld stop` and the orphan reaper.

use std::time::Duration;

use tracing::{debug, warn};
use veld_core::db::Db;
use veld_core::stats::is_sampled;
use veld_stats::StatsCollector;

/// Interval between resource-stats samples (seconds). Kept at/under
/// `veld_core::stats::STALE_AFTER_SECS` so a healthy sampler always refreshes a
/// node's stats before its last sample ages out.
const SAMPLE_INTERVAL_SECS: u64 = 5;

/// Periodically sample CPU/memory for every live run's node process trees
/// and append them to the `node_stats` table. Runs as its own daemon task,
/// separate from the health monitor, so slow liveness probes there never delay
/// sampling (which would make live stats read as stale).
pub async fn run_stats_sampler() {
    let mut interval = tokio::time::interval(Duration::from_secs(SAMPLE_INTERVAL_SECS));
    // Match the monitor/GC loops: after a macOS sleep, take one sample on wake
    // rather than firing the whole backlog of missed ticks.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Persistent across ticks: sysinfo derives CPU usage from the delta between
    // two refreshes of the same process, so the collector must outlive a tick.
    let mut collector = StatsCollector::new();

    loop {
        interval.tick().await;
        if let Err(e) = sample_once(&mut collector).await {
            warn!("stats sampling error: {e}");
        }
    }
}

/// One sampling pass: refresh the process table once, then record a sample per
/// live node of every live run. Observational only — per-run write failures
/// are logged and skipped, never propagated.
async fn sample_once(collector: &mut StatsCollector) -> anyhow::Result<()> {
    // Open per pass so the sampler self-heals across CLI upgrades that migrate
    // the schema (mirrors the health monitor and GC loops).
    let db = Db::open()?;
    let registry = db.registry()?;

    // Skip the (machine-wide) process-table refresh entirely when nothing is
    // live — no point scanning every process to observe zero nodes.
    let any_live = registry
        .projects
        .values()
        .any(|e| e.runs.values().any(|r| is_sampled(r.status)));
    if !any_live {
        return Ok(());
    }
    collector.refresh();

    let sampled_at = chrono::Utc::now();
    for entry in registry.projects.values() {
        for (run_name, run_info) in &entry.runs {
            if !is_sampled(run_info.status) {
                continue;
            }
            let run_state = match db.get_run(&entry.project_root, run_name) {
                Ok(Some(rs)) => rs,
                _ => continue,
            };
            let mut samples = Vec::new();
            let mut trees = Vec::new();
            for (key, node_state) in &run_state.nodes {
                if let Some(pid) = node_state.pid {
                    if veld_core::process::is_alive(pid) {
                        if let Some(tree) = collector.sample_tree(pid, sampled_at) {
                            samples.push((key.clone(), tree.total));
                            trees.push((key.clone(), tree.processes));
                        }
                    }
                }
            }
            if let Err(e) = db.record_node_stats(&entry.project_root, run_name, &samples, &trees) {
                debug!("could not record node stats for run '{run_name}': {e}");
            }
        }
    }

    Ok(())
}
