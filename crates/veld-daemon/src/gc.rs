use std::sync::Arc;

use tracing::{debug, info, warn};
use uuid::Uuid;
use veld_core::db::Db;
use veld_core::helper::HelperClient;
use veld_core::state::{RunState, RunStatus};

use crate::share::manager::ShareManager;

/// Interval between garbage-collection runs (seconds).
const GC_INTERVAL_SECS: u64 = 600; // 10 minutes

/// Maximum age for stopped/failed entries before pruning (hours).
const MAX_ENTRY_AGE_HOURS: i64 = 72;

/// Maximum age for log files before pruning (hours).
const MAX_LOG_AGE_HOURS: i64 = 168; // 7 days

/// Maximum age for process-stats samples before pruning (hours). Short: the
/// samples are high-frequency (one row per node every 5s) and only feed the
/// live UI/CLI views, which never look back more than a few minutes.
const MAX_STATS_AGE_HOURS: i64 = 6;

/// Run the garbage-collection scheduler. This function loops forever and
/// performs GC on the configured interval.
pub async fn run_gc_scheduler(share_manager: Arc<ShareManager>) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(GC_INTERVAL_SECS));
    // Don't fire a backlog of missed ticks after a macOS sleep — one GC pass on
    // wake is enough.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        info!("running scheduled garbage collection");

        match run_gc().await {
            Ok(summary) => {
                info!(
                    "gc complete: {} stale removed, {} orphans killed, {} logs pruned, {} stats pruned, {} routes cleaned",
                    summary.stale_removed,
                    summary.orphans_killed,
                    summary.logs_pruned,
                    summary.stats_pruned,
                    summary.routes_cleaned
                );
                // Stop any shares whose run just died so they don't outlive the
                // environment they expose (crash path — CLI `veld stop` already
                // unshares directly).
                for run_id in summary.orphaned_runs {
                    share_manager.unshare_run(run_id).await;
                }
            }
            Err(e) => {
                warn!("gc error: {e}");
            }
        }
    }
}

/// Summary of a single GC pass.
#[derive(Debug, Default)]
pub struct GcSummary {
    pub stale_removed: usize,
    pub orphans_killed: usize,
    pub logs_pruned: usize,
    pub stats_pruned: usize,
    pub routes_cleaned: usize,
    /// Run ids whose processes were found dead this pass — their P2P shares
    /// should be stopped.
    pub orphaned_runs: Vec<Uuid>,
}

/// Perform a single garbage-collection pass.
pub async fn run_gc() -> anyhow::Result<GcSummary> {
    let mut summary = GcSummary::default();
    let helper = HelperClient::default_client();

    // Open per pass so the daemon self-heals across CLI upgrades that migrate
    // the schema.
    let db = Db::open()?;
    let registry = db.registry()?;

    // Phase 1: Process each project's runs -- remove stale entries and kill orphans.
    for reg_entry in registry.projects.values() {
        let project_root = reg_entry.project_root.clone();

        let project_state = match db.load_project_state(&project_root) {
            Ok(ps) => ps,
            Err(e) => {
                debug!(
                    "could not load project state for {}: {e}",
                    project_root.display()
                );
                continue;
            }
        };

        for (run_name, run_state) in &project_state.runs {
            match run_state.status {
                RunStatus::Running | RunStatus::Starting => {
                    // Check if processes are actually alive.
                    let mut any_alive = false;
                    let mut dead_pids = Vec::new();

                    for node_state in run_state.nodes.values() {
                        if let Some(pid) = node_state.pid {
                            if is_process_alive(pid) {
                                any_alive = true;
                            } else {
                                dead_pids.push(pid);
                            }
                        }
                    }

                    if !any_alive && !dead_pids.is_empty() {
                        // All processes dead -- mark as stopped (orphan cleanup).
                        info!(
                            "killing orphan run '{}' with dead PIDs: {:?}",
                            run_name, dead_pids
                        );

                        let mut run = run_state.clone();
                        run.status = RunStatus::Stopped;
                        run.stopped_at = Some(chrono::Utc::now());
                        summary.orphaned_runs.push(run.run_id);
                        for node in run.nodes.values_mut() {
                            if let Some(pid) = node.pid {
                                if !is_process_alive(pid) {
                                    node.status = veld_core::state::NodeStatus::Stopped;
                                } else {
                                    // Still alive -- kill it.
                                    kill_process(pid);
                                    node.status = veld_core::state::NodeStatus::Stopped;
                                }
                            }
                        }

                        // Clean up Caddy routes and DNS entries.
                        summary.routes_cleaned +=
                            cleanup_routes_and_dns(&run, run_name, &helper).await;

                        let _ = db.save_run(&project_root, &reg_entry.project_name, &run);
                        summary.orphans_killed += 1;
                    }
                }
                RunStatus::Stopped | RunStatus::Failed => {
                    // Check age -- remove if older than threshold.
                    if let Some(stopped_at) = run_state.stopped_at {
                        let age = chrono::Utc::now().signed_duration_since(stopped_at);
                        if age.num_hours() > MAX_ENTRY_AGE_HOURS {
                            debug!(
                                "removing stale run '{}' from project {}",
                                run_name,
                                project_root.display()
                            );
                            // Best-effort route/DNS cleanup before removing state.
                            summary.routes_cleaned +=
                                cleanup_routes_and_dns(run_state, run_name, &helper).await;

                            let _ = db.remove_run(&project_root, run_name);
                            summary.stale_removed += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Phase 2: Prune old log lines and orphaned feedback data, then reclaim
    // the freed pages (screenshot BLOBs and log rows add up).
    let log_cutoff = chrono::Utc::now() - chrono::Duration::hours(MAX_LOG_AGE_HOURS);
    summary.logs_pruned = db.prune_logs_older_than(log_cutoff).unwrap_or(0);
    let _ = db.prune_orphaned_feedback(log_cutoff);
    let stats_cutoff = chrono::Utc::now() - chrono::Duration::hours(MAX_STATS_AGE_HOURS);
    summary.stats_pruned = db.prune_node_stats_older_than(stats_cutoff).unwrap_or(0);
    let _ = db.vacuum();

    // Phase 3: Prune leftover pre-SQLite log files from each project's
    // .veld/logs/ directory (written by old veld versions and by legacy
    // `_timestamp` pipelines that survive the upgrade). Same age policy.
    for reg_entry in registry.projects.values() {
        let logs_dir = reg_entry.project_root.join(".veld").join("logs");
        if logs_dir.exists() {
            let mut entries = match tokio::fs::read_dir(&logs_dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Some(entry) = entries.next_entry().await.unwrap_or(None) {
                let path = entry.path();
                if let Ok(meta) = tokio::fs::metadata(&path).await {
                    if let Ok(modified) = meta.modified() {
                        let age = std::time::SystemTime::now()
                            .duration_since(modified)
                            .unwrap_or_default();
                        let age_hours = age.as_secs() as i64 / 3600;

                        if age_hours > MAX_LOG_AGE_HOURS {
                            debug!("pruning old log file: {}", path.display());
                            if meta.is_dir() {
                                let _ = tokio::fs::remove_dir_all(&path).await;
                            } else {
                                let _ = tokio::fs::remove_file(&path).await;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(summary)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Remove Caddy routes and DNS entries for all nodes in a run.
/// Returns the number of routes/hosts cleaned up.
async fn cleanup_routes_and_dns(run: &RunState, run_name: &str, helper: &HelperClient) -> usize {
    let mut cleaned = 0;
    for ns in run.nodes.values() {
        // Remove Caddy route (ID follows the convention from orchestrator).
        let route_id = format!("veld-{}-{}-{}", run_name, ns.node_name, ns.variant);
        if helper.remove_route(&route_id).await.is_ok() {
            debug!("removed Caddy route: {route_id}");
            cleaned += 1;
        }

        // Remove DNS host entry.
        if let Some(ref url_str) = ns.url {
            let hostname = url_str.strip_prefix("https://").unwrap_or(url_str);
            if helper.remove_host(hostname).await.is_ok() {
                debug!("removed DNS entry: {hostname}");
            }
        }
    }
    cleaned
}

fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn kill_process(pid: u32) {
    // Guard against dangerous PIDs (same as veld-core::process::kill_process).
    if pid <= 1 || pid > i32::MAX as u32 {
        return;
    }
    unsafe {
        // Send to the process group first (negative PID) to kill the entire
        // pipeline (server + _timestamp wrapper). Fall back to the individual
        // PID if the group kill fails (process may not be a group leader).
        let pgid = -(pid as libc::pid_t);
        if libc::kill(pgid, libc::SIGTERM) != 0 {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}
