use veld_core::helper::HelperClient;
use veld_core::state::{NodeStatus, RunStatus};

use crate::output;

/// Maximum age for log lines before pruning (hours). Matches the daemon GC.
const MAX_LOG_AGE_HOURS: i64 = 168; // 7 days

/// `veld gc` -- garbage-collect stale state, logs, orphaned processes, and routes.
pub async fn run() -> i32 {
    output::print_info("Running garbage collection...");

    let helper = HelperClient::default_client();

    let Some(db) = super::open_db(false) else {
        return 1;
    };
    let registry = match db.registry() {
        Ok(r) => r,
        Err(e) => {
            output::print_error(&format!("Failed to load registry: {e}"), false);
            return 1;
        }
    };

    let mut projects_removed = 0usize;
    let mut orphans_cleaned = 0usize;
    let mut routes_cleaned = 0usize;

    for reg_entry in registry.projects.values() {
        let project_root = reg_entry.project_root.clone();

        // Remove projects whose root no longer exists.
        if !project_root.exists() {
            if db.remove_project(&project_root).is_ok() {
                projects_removed += 1;
            }
            continue;
        }

        let project_state = match db.load_project_state(&project_root) {
            Ok(ps) => ps,
            Err(_) => continue,
        };

        for (run_name, run) in &project_state.runs {
            let is_orphan = match run.status {
                RunStatus::Running | RunStatus::Starting => {
                    // Check if all processes are dead.
                    let has_pids = run.nodes.values().any(|n| n.pid.is_some());
                    let all_dead = run
                        .nodes
                        .values()
                        .filter_map(|n| n.pid)
                        .all(|pid| unsafe { libc::kill(pid as libc::pid_t, 0) != 0 });
                    has_pids && all_dead
                }
                _ => false,
            };

            if is_orphan {
                let mut run = run.clone();
                // Clean up Caddy routes and DNS entries.
                for ns in run.nodes.values() {
                    let route_id = format!("veld-{}-{}-{}", run_name, ns.node_name, ns.variant);
                    if helper.remove_route(&route_id).await.is_ok() {
                        routes_cleaned += 1;
                    }
                    if let Some(ref url_str) = ns.url {
                        let hostname = url_str.strip_prefix("https://").unwrap_or(url_str);
                        let _ = helper.remove_host(hostname).await;
                    }
                }

                // Same guarded one-step finalize as the daemon: record final
                // node states while the run is still live, then label it
                // crashed (no-ops if an ender moved it to `stopping` first).
                let mut dead_node: Option<String> = None;
                for (key, node) in run.nodes.iter_mut() {
                    if node.pid.take().is_some() && dead_node.is_none() {
                        dead_node = Some(key.clone());
                    }
                    node.status = NodeStatus::Stopped;
                }
                let _ = db.save_run(&project_root, &reg_entry.project_name, &run);
                let detail = veld_core::state::EndDetail {
                    failed_node: dead_node,
                    ..Default::default()
                };
                let _ = db.finalize_crashed(&run.run_id, Some(&detail));
                orphans_cleaned += 1;
            }
        }
    }

    // Prune old log lines and orphaned feedback data (older than 7 days),
    // then reclaim the freed pages.
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(MAX_LOG_AGE_HOURS);
    let logs_pruned = db.prune_logs_older_than(cutoff).unwrap_or(0);
    let feedback_cleaned = db.prune_orphaned_feedback(cutoff).unwrap_or(0);
    let _ = db.vacuum();

    // Prune leftover pre-SQLite log files from each project's .veld/logs/
    // (same age policy as the daemon GC, so a daemon-less setup cleans up too).
    for reg_entry in registry.projects.values() {
        let logs_dir = reg_entry.project_root.join(".veld").join("logs");
        let Ok(entries) = std::fs::read_dir(&logs_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| {
                    t.elapsed().unwrap_or_default()
                        > std::time::Duration::from_secs(MAX_LOG_AGE_HOURS as u64 * 3600)
                })
                .unwrap_or(false);
            if stale {
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    // Print summary.
    let mut parts = Vec::new();
    if projects_removed > 0 {
        parts.push(format!("{projects_removed} stale project(s) removed"));
    }
    if orphans_cleaned > 0 {
        parts.push(format!("{orphans_cleaned} orphan run(s) cleaned"));
    }
    if routes_cleaned > 0 {
        parts.push(format!("{routes_cleaned} route(s) removed"));
    }
    if logs_pruned > 0 {
        parts.push(format!("{logs_pruned} old log line(s) pruned"));
    }
    if feedback_cleaned > 0 {
        parts.push(format!("{feedback_cleaned} stale feedback run(s) removed"));
    }

    if parts.is_empty() {
        output::print_success("Nothing to clean up.");
    } else {
        output::print_success(&parts.join(", "));
    }

    0
}
