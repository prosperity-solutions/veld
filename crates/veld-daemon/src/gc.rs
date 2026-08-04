use std::sync::Arc;

use tracing::{debug, info, warn};
use uuid::Uuid;
use veld_core::db::Db;
use veld_core::helper::HelperClient;
use veld_core::state::{RunState, RunStatus};
// Stats retention lives in `veld_core::stats` because three components have to
// agree on it: this GC, the history API's window clamp, and the UI's window
// presets. See `NODE_STATS_RETENTION_SECS` for why it is not a local constant.
use veld_core::stats::{NODE_STATS_RETENTION_SECS, PROCESS_STATS_RETENTION_SECS};

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
/// ender, so only age separates them. Generous on purpose. Conscious accept:
/// a legitimate teardown that runs longer than this gets finalized early,
/// releasing the live slot mid-teardown — at 10 minutes that's a hung hook,
/// not a working stop.
const STOPPING_GRACE_SECS: i64 = 600;

/// Maximum age for log lines and ended runs before pruning (hours).
const MAX_LOG_AGE_HOURS: i64 = 168; // 7 days

/// How long after a run ends its unconfirmed PIDs are still re-killed by the
/// straggler sweep. Past this, PID recycling makes re-killing more dangerous
/// than the leak — the PID is cleared with a warning instead.
const STRAGGLER_SWEEP_MAX_AGE_SECS: i64 = 3600;

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
                    "gc complete: {} stale removed, {} orphans killed, {} logs pruned, {} stats pruned ({} per-process), {} routes cleaned, {} worktrees evicted",
                    summary.stale_removed,
                    summary.orphans_killed,
                    summary.logs_pruned,
                    summary.stats_pruned,
                    summary.process_stats_pruned,
                    summary.routes_cleaned,
                    // Logged because it is the only destructive number in this
                    // summary: if a checkout disappeared overnight, this line is
                    // where the answer has to be.
                    summary.worktrees_evicted
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
    /// Per-process rows pruned — counted separately from `stats_pruned` because
    /// the two tables are pruned on different horizons.
    pub process_stats_pruned: usize,
    pub routes_cleaned: usize,
    /// Worktrees marked for removal by auto-eviction this pass. Zero unless the
    /// user has set `worktree.evictAfterDays`, which defaults to off.
    pub worktrees_evicted: usize,
    /// Run ids whose processes were found dead this pass — their P2P shares
    /// should be stopped.
    pub orphaned_runs: Vec<Uuid>,
}

/// The two stats-pruning cutoffs, as `(node_aggregates, per_process)`.
///
/// Extracted and returned as a pair so the *wiring* is testable: both cutoffs are
/// `DateTime<Utc>`, so passing them to the wrong prune call compiles happily and
/// would silently prune node totals on the 2h horizon and per-process rows on the
/// 24h one — the exact inverse of the documented split, with nothing failing.
/// `now` is a parameter rather than read inside so a test can pin the arithmetic.
fn retention_cutoffs(
    now: chrono::DateTime<chrono::Utc>,
) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    (
        now - chrono::Duration::seconds(NODE_STATS_RETENTION_SECS),
        now - chrono::Duration::seconds(PROCESS_STATS_RETENTION_SECS),
    )
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
                    // Escalating kill (SIGTERM → wait → SIGKILL): a run stuck
                    // in `stopping` past the grace period must actually end,
                    // or leak-freedom would depend on the process honoring
                    // SIGTERM. Recycled-PID exposure is bounded here — a run
                    // sits in `stopping` for minutes, not days.
                    if is_process_alive(pid) {
                        let _ = veld_core::process::kill_process(pid).await;
                    }
                    // Confirm before clearing — an unkilled PID stays recorded
                    // so the straggler sweep keeps covering it.
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
    //
    // Bounded window: PIDs are only swept while the run ended less than
    // STRAGGLER_SWEEP_MAX_AGE_SECS ago. Terminal rows now persist for days,
    // and the OS recycles PIDs — an old recorded PID is more likely an
    // unrelated process than our straggler, and SIGTERMing it every pass
    // would be worse than the leak. Past the window the PID is cleared with
    // a warning instead of killed.
    if let Ok(stragglers) = db.terminal_runs_with_pids() {
        let now = chrono::Utc::now();
        for run in stragglers {
            let within_window = run
                .ended_at
                .is_some_and(|t| (now - t).num_seconds() < STRAGGLER_SWEEP_MAX_AGE_SECS);
            for (key, node) in &run.nodes {
                let Some(pid) = node.pid else { continue };
                if !within_window {
                    warn!(
                        "giving up on unconfirmed PID {pid} under terminal run '{}' \
                         (ended too long ago to safely re-kill; PID may be recycled)",
                        run.name
                    );
                    let _ = db.clear_node_pid(&run.run_id, key);
                    continue;
                }
                if is_process_alive(pid) {
                    info!(
                        "re-killing straggler PID {pid} under terminal run '{}'",
                        run.name
                    );
                    // Escalates SIGTERM → SIGKILL; a SIGTERM-ignorer must not
                    // survive every pass until the window closes and leak.
                    let _ = veld_core::process::kill_process(pid).await;
                }
                if !is_process_alive(pid) {
                    let _ = db.clear_node_pid(&run.run_id, key);
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
    let (stats_cutoff, proc_cutoff) = retention_cutoffs(chrono::Utc::now());
    summary.stats_pruned = db.prune_node_stats_older_than(stats_cutoff).unwrap_or(0);
    summary.process_stats_pruned = db
        .prune_node_process_stats_older_than(proc_cutoff)
        .unwrap_or(0);
    let _ = db.vacuum();

    // Phase 2b: Auto-evict idle worktrees, if the user turned it on.
    //
    // Marks only. The actual `git worktree remove` goes through the same
    // background worker and the same **un-forced** removal as a hand-clicked
    // delete, so git still refuses a dirty or locked checkout and the worktree
    // comes back out of trash with the reason attached. That is the safety
    // property that makes a timer allowed to touch a developer's checkout at all:
    // it can never override git, only ask.
    summary.worktrees_evicted = crate::feedback_server::worktree_trash::mark_evictions(&db).await;

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
        // The pre-#170 id needs no URL, so it is removed unconditionally — a
        // node whose `url` checkpoint never landed still had its route added
        // (`add_route` runs before the post-spawn save), and this is the only
        // key that can reach such an entry. Covers a route stored by an older
        // helper still running after a `veld update` (see `legacy_run_route_id`).
        let legacy = veld_core::url::legacy_run_route_id(run_name, &ns.node_name, &ns.variant);
        // Counted like any other removal: in the `veld update` window this exists
        // for, it IS the removal, and reporting "0 routes removed" while deleting
        // them would misread as "nothing to do".
        if helper.remove_route(&legacy).await.is_ok() {
            debug!("removed legacy Caddy route: {legacy}");
            cleaned += 1;
        }

        // The hostname-keyed id, on the other hand, is derivable only from a
        // recorded URL. An entry added just before a kill, with no URL persisted,
        // is therefore unreachable here — it is overwritten by the next start of
        // the same environment, since the id is a pure function of the hostname.
        let Some(ref url_str) = ns.url else { continue };
        // `veld_core::url` owns both the hostname extraction and the id format,
        // so this cannot drift from the orchestrator's construction side (#170).
        // Note the port is stripped here too — the previous version removed the
        // DNS host as `host:18443` whenever the helper wasn't on 443.
        let hostname = veld_core::url::hostname_of_url(url_str);
        let route_id = veld_core::url::run_route_id(hostname);
        if helper.remove_route(&route_id).await.is_ok() {
            debug!("removed Caddy route: {route_id}");
            cleaned += 1;
        }

        // Remove DNS host entry.
        if helper.remove_host(hostname).await.is_ok() {
            debug!("removed DNS entry: {hostname}");
        }
    }
    cleaned
}

fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

// Process kills go through `veld_core::process::kill_process`, which
// escalates SIGTERM → bounded wait → SIGKILL for the whole process group —
// the daemon reapers are exactly the paths that must not depend on a target
// honoring SIGTERM.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_cutoffs_map_each_horizon_to_its_own_table() {
        let now = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH + chrono::Duration::days(10);
        let (stats, proc_) = retention_cutoffs(now);

        // Aggregates keep the longer history...
        assert_eq!(
            (now - stats).num_seconds(),
            NODE_STATS_RETENTION_SECS,
            "node aggregates must use NODE_STATS_RETENTION_SECS"
        );
        // ...per-process rows the shorter one. Swapping the pair at the call site
        // compiles, so this is the only thing standing between a typo and an
        // inverted retention policy.
        assert_eq!(
            (now - proc_).num_seconds(),
            PROCESS_STATS_RETENTION_SECS,
            "per-process rows must use PROCESS_STATS_RETENTION_SECS"
        );
        assert!(
            proc_ > stats,
            "the per-process cutoff is more recent, i.e. prunes more aggressively"
        );
    }
}
