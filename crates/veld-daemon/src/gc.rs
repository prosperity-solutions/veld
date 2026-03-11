use tracing::{debug, info, warn};
use veld_core::state::{GlobalRegistry, ProjectState, RunStatus};

/// Interval between garbage-collection runs (seconds).
const GC_INTERVAL_SECS: u64 = 600; // 10 minutes

/// Maximum age for stopped/failed entries before pruning (hours).
const MAX_ENTRY_AGE_HOURS: i64 = 72;

/// Maximum age for log files before pruning (hours).
const MAX_LOG_AGE_HOURS: i64 = 168; // 7 days

/// Run the garbage-collection scheduler. This function loops forever and
/// performs GC on the configured interval.
pub async fn run_gc_scheduler() {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(GC_INTERVAL_SECS));

    loop {
        interval.tick().await;
        info!("running scheduled garbage collection");

        match run_gc().await {
            Ok(summary) => {
                info!(
                    "gc complete: {} stale entries removed, {} orphans killed, {} log files pruned",
                    summary.stale_removed, summary.orphans_killed, summary.logs_pruned
                );
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
}

/// Perform a single garbage-collection pass.
pub async fn run_gc() -> anyhow::Result<GcSummary> {
    let mut summary = GcSummary::default();

    let mut registry = GlobalRegistry::load()?;
    let mut registry_changed = false;

    // Phase 1: Process each project's runs -- remove stale entries and kill orphans.
    for (_project_path, reg_entry) in registry.projects.iter_mut() {
        let project_root = reg_entry.project_root.clone();

        let mut project_state = match ProjectState::load(&project_root) {
            Ok(ps) => ps,
            Err(e) => {
                debug!(
                    "could not load project state for {}: {e}",
                    project_root.display()
                );
                continue;
            }
        };
        let mut project_changed = false;

        // Collect run names to avoid borrow issues.
        let run_names: Vec<String> = reg_entry.runs.keys().cloned().collect();

        for run_name in &run_names {
            let run_info = &reg_entry.runs[run_name];

            match run_info.status {
                RunStatus::Running => {
                    // Check if processes are actually alive.
                    if let Some(run_state) = project_state.get_run(run_name) {
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

                            if let Some(run) = project_state.get_run_mut(run_name) {
                                run.status = RunStatus::Stopped;
                                run.stopped_at = Some(chrono::Utc::now());
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
                                project_changed = true;
                            }

                            if let Some(info) = reg_entry.runs.get_mut(run_name) {
                                info.status = RunStatus::Stopped;
                                info.urls.clear();
                                registry_changed = true;
                            }

                            summary.orphans_killed += 1;
                        }
                    }
                }
                RunStatus::Stopped | RunStatus::Failed => {
                    // Check age -- remove if older than threshold.
                    if let Some(run_state) = project_state.get_run(run_name) {
                        if let Some(stopped_at) = run_state.stopped_at {
                            let age = chrono::Utc::now().signed_duration_since(stopped_at);
                            if age.num_hours() > MAX_ENTRY_AGE_HOURS {
                                debug!(
                                    "removing stale run '{}' from project {}",
                                    run_name,
                                    project_root.display()
                                );
                                project_state.runs.remove(run_name);
                                project_changed = true;
                                // Will remove from registry below.
                                summary.stale_removed += 1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Remove stale runs from registry entry.
        reg_entry
            .runs
            .retain(|name, _| project_state.runs.contains_key(name));
        if reg_entry.runs.len() != run_names.len() {
            registry_changed = true;
        }

        if project_changed {
            let _ = project_state.save(&project_root);
        }
    }

    if registry_changed {
        let _ = registry.save();
    }

    // Phase 2: Prune old log files from each project's .veld/logs/ directory.
    let registry = GlobalRegistry::load().unwrap_or_default();
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
                            debug!("pruning old log: {}", path.display());
                            let _ = tokio::fs::remove_file(&path).await;
                            summary.logs_pruned += 1;
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

fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn kill_process(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}
