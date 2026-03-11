use crate::broadcaster::Broadcaster;
use tracing::{debug, info, warn};
use veld_core::state::{GlobalRegistry, ProjectState, RunStatus};

/// Interval between health-check scans (seconds).
const SCAN_INTERVAL_SECS: u64 = 5;

/// Periodically scan all runs from the global registry and check process health.
/// When a status change is detected, update the registry and broadcast the event.
pub async fn run_health_monitor(broadcaster: Broadcaster) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(SCAN_INTERVAL_SECS));

    loop {
        interval.tick().await;
        debug!("running health-check scan");

        match scan_and_update(&broadcaster).await {
            Ok(changes) => {
                if changes > 0 {
                    info!("health scan detected {changes} status change(s)");
                }
            }
            Err(e) => {
                warn!("health scan error: {e}");
            }
        }
    }
}

/// Scan the global registry, check each running process, and return the number
/// of status changes applied.
async fn scan_and_update(broadcaster: &Broadcaster) -> anyhow::Result<usize> {
    let registry = GlobalRegistry::load()?;

    let mut changes = 0;

    for reg_entry in registry.projects.values() {
        let project_root = &reg_entry.project_root;

        for (run_name, run_info) in &reg_entry.runs {
            if run_info.status != RunStatus::Running {
                continue;
            }

            // Load the actual project state to get full RunState with node PIDs.
            let project_state = match ProjectState::load(project_root) {
                Ok(ps) => ps,
                Err(e) => {
                    debug!(
                        "could not load project state for {}: {e}",
                        project_root.display()
                    );
                    continue;
                }
            };

            let run_state = match project_state.get_run(run_name) {
                Some(rs) => rs,
                None => continue,
            };

            // Check if any node with a PID has died.
            let mut any_dead = false;
            for node_state in run_state.nodes.values() {
                if let Some(pid) = node_state.pid {
                    if !is_process_alive(pid) {
                        any_dead = true;
                        info!(
                            "process {pid} (node {}:{}) is no longer alive",
                            node_state.node_name, node_state.variant
                        );
                    }
                }
            }

            if any_dead {
                // Update the project state: mark the run as stopped.
                let mut project_state = match ProjectState::load(project_root) {
                    Ok(ps) => ps,
                    Err(_) => continue,
                };

                if let Some(run) = project_state.get_run_mut(run_name) {
                    run.status = RunStatus::Stopped;
                    run.stopped_at = Some(chrono::Utc::now());

                    // Mark dead nodes as stopped.
                    for node in run.nodes.values_mut() {
                        if let Some(pid) = node.pid {
                            if !is_process_alive(pid) {
                                node.status = veld_core::state::NodeStatus::Stopped;
                            }
                        }
                    }
                }

                let _ = project_state.save(project_root);

                // Update the global registry.
                let mut registry = GlobalRegistry::load().unwrap_or_default();
                if let Some(entry) = registry
                    .projects
                    .get_mut(&project_root.to_string_lossy().into_owned())
                {
                    if let Some(info) = entry.runs.get_mut(run_name) {
                        info.status = RunStatus::Stopped;
                    }
                }
                let _ = registry.save();

                // Broadcast the change.
                let event = serde_json::json!({
                    "event": "status_change",
                    "run": run_name,
                    "project": project_root.to_string_lossy(),
                    "old_status": "running",
                    "new_status": "stopped",
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                broadcaster.broadcast(&event).await;

                changes += 1;
            }
        }
    }

    Ok(changes)
}

/// Check whether a given PID is alive by sending signal 0.
fn is_process_alive(pid: u32) -> bool {
    // On Unix, sending signal 0 checks for process existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
