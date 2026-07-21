use std::sync::Arc;

use tracing::{debug, info, warn};
use uuid::Uuid;
use veld_core::db::Db;
use veld_core::helper::HelperClient;
use veld_core::state::{RunState, RunStatus};

use crate::share::manager::ShareManager;

/// Interval between garbage-collection runs (seconds).
const GC_INTERVAL_SECS: u64 = 600; // 10 minutes

/// Ended runs kept per environment (run history cap). Runs beyond the cap —
/// and ended runs older than `MAX_LOG_AGE_HOURS` — are pruned with their logs.
const RUN_HISTORY_KEEP: usize = 10;

/// Grace period before the stale-`stopping` reaper touches a `stopping` run.
/// Dead PIDs under `stopping` is the NORMAL state of a healthy `veld stop`
/// (PIDs are killed first, then on_stop hooks and teardown steps run for
/// seconds to minutes) — indistinguishable in DB state from a SIGKILLed
/// ender, so only age separates them. Generous on purpose.
const STOPPING_GRACE_SECS: i64 = 600;

/// Maximum age for log lines and ended runs before pruning (hours).
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
            if !matches!(run_state.status, RunStatus::Running | RunStatus::Starting) {
                // `stopping` belongs to the grace-gated reaper below; terminal
                // runs are history (retention handles them).
                continue;
            }

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
                // Crash detection: same one-step guarded finalize as the
                // monitor — whichever fires first wins, both say `crashed`.
                info!(
                    "finalizing orphan run '{}' as crashed (dead PIDs: {:?})",
                    run_name, dead_pids
                );

                let mut run = run_state.clone();
                summary.orphaned_runs.push(run.run_id);
                let mut dead_node: Option<String> = None;
                for (key, node) in run.nodes.iter_mut() {
                    if node.pid.take().is_some() {
                        if dead_node.is_none() {
                            dead_node = Some(key.clone());
                        }
                        node.status = veld_core::state::NodeStatus::Stopped;
                    }
                }

                // Clean up Caddy routes and DNS entries.
                summary.routes_cleaned += cleanup_routes_and_dns(&run, run_name, &helper).await;

                // Final node states while live, then the guarded finalize (a
                // no-op if a deliberate ender moved it to `stopping` first).
                let _ = db.save_run(&project_root, &reg_entry.project_name, &run);
                let detail = veld_core::state::EndDetail {
                    failed_node: dead_node,
                    ..Default::default()
                };
                let _ = db.finalize_crashed(&run.run_id, Some(&detail));
                summary.orphans_killed += 1;
            }
        }
    }

    // Phase 1b: stale-`stopping` reaper, grace-gated on BOTH branches (dead
    // PIDs under `stopping` is what a healthy slow teardown looks like).
    // Past the grace period the ender is dead or hung: re-kill anything
    // alive, then finalize with the intent `begin_ending` stored.
    let stopping_cutoff = chrono::Utc::now() - chrono::Duration::seconds(STOPPING_GRACE_SECS);
    if let Ok(stale) = db.stale_stopping_runs(stopping_cutoff) {
        for (_project_root, _project_name, run) in stale {
            let run_name = run.name.clone();
            info!("finalizing stale 'stopping' run '{run_name}' (ender gone)");
            for (key, node) in &run.nodes {
                if let Some(pid) = node.pid {
                    if is_process_alive(pid) {
                        kill_process(pid);
                    }
                    // Confirm before clearing — an unkilled PID stays recorded
                    // so the straggler sweep keeps covering it.
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    if !is_process_alive(pid) {
                        let _ = db.clear_node_pid(&run.run_id, key);
                    }
                }
            }
            summary.routes_cleaned += cleanup_routes_and_dns(&run, &run_name, &helper).await;
            if db.finalize_run(&run.run_id).unwrap_or(false) {
                summary.orphaned_runs.push(run.run_id);
                summary.stale_removed += 1;
            }
        }
    }

    // Phase 1c: terminal-run straggler sweep. A PID recorded under a terminal
    // run means a finalize could not confirm its kill — re-kill until it dies,
    // then clear it. Leak-freedom never depends on the end label.
    if let Ok(stragglers) = db.terminal_runs_with_pids() {
        for run in stragglers {
            for (key, node) in &run.nodes {
                if let Some(pid) = node.pid {
                    if is_process_alive(pid) {
                        info!(
                            "re-killing straggler PID {pid} under terminal run '{}'",
                            run.name
                        );
                        kill_process(pid);
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    }
                    if !is_process_alive(pid) {
                        let _ = db.clear_node_pid(&run.run_id, key);
                    }
                }
            }
        }
    }

    // Phase 1d: run-history retention — keep the newest RUN_HISTORY_KEEP ended
    // runs per environment, and nothing older than the log age cap. Deleting a
    // run cascades nodes/node_stats by FK and removes its log lines by run_id.
    let history_cutoff = chrono::Utc::now() - chrono::Duration::hours(MAX_LOG_AGE_HOURS);
    if let Ok(prunable) = db.prunable_run_ids(RUN_HISTORY_KEEP, history_cutoff) {
        for run_id in prunable {
            if db.delete_ended_run(&run_id).unwrap_or(false) {
                summary.stale_removed += 1;
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
