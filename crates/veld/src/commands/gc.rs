use veld_core::helper::HelperClient;
use veld_core::state::{GlobalRegistry, NodeStatus, ProjectState, RunStatus};

use crate::output;

/// `veld gc` -- garbage-collect stale state, logs, orphaned processes, and routes.
pub async fn run() -> i32 {
    output::print_info("Running garbage collection...");

    let helper = HelperClient::default_client();

    let mut registry = match GlobalRegistry::load() {
        Ok(r) => r,
        Err(e) => {
            output::print_error(&format!("Failed to load registry: {e}"), false);
            return 1;
        }
    };

    let mut registry_changed = false;
    let mut orphans_cleaned = 0usize;
    let mut routes_cleaned = 0usize;

    // Remove entries whose project root no longer exists.
    let before = registry.projects.len();
    registry
        .projects
        .retain(|_, entry| entry.project_root.exists());
    let projects_removed = before - registry.projects.len();
    if projects_removed > 0 {
        registry_changed = true;
    }

    // Process each project: find orphaned runs and clean them up.
    for reg_entry in registry.projects.values_mut() {
        let project_root = reg_entry.project_root.clone();
        let mut project_state = match ProjectState::load(&project_root) {
            Ok(ps) => ps,
            Err(_) => continue,
        };
        let mut project_changed = false;

        let run_names: Vec<String> = project_state.runs.keys().cloned().collect();

        for run_name in &run_names {
            let is_orphan = {
                let run = match project_state.get_run(run_name) {
                    Some(r) => r,
                    None => continue,
                };
                match run.status {
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
                }
            };

            if is_orphan {
                if let Some(run) = project_state.get_run_mut(run_name) {
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

                    run.status = RunStatus::Stopped;
                    run.stopped_at = Some(chrono::Utc::now());
                    for node in run.nodes.values_mut() {
                        node.status = NodeStatus::Stopped;
                    }
                    project_changed = true;
                    orphans_cleaned += 1;
                }

                if let Some(info) = reg_entry.runs.get_mut(run_name) {
                    info.status = RunStatus::Stopped;
                    info.urls.clear();
                    registry_changed = true;
                }
            }
        }

        if project_changed {
            let _ = project_state.save(&project_root);
        }
    }

    if registry_changed {
        let _ = registry.save();
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

    if parts.is_empty() {
        output::print_success("Nothing to clean up.");
    } else {
        output::print_success(&parts.join(", "));
    }

    0
}
