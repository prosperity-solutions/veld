//! Browser-based management dashboard served at `veld.localhost`.
//!
//! Provides a read-only overview of all Veld environments on the machine,
//! with clickable service URLs and live status badges.

use std::collections::HashMap;

use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::warn;
use veld_core::logging;
use veld_core::state::{GlobalRegistry, NodeStatus, ProjectState, RunStatus};

const DASHBOARD_HTML: &str = include_str!("../assets/management-ui.html");

/// Build an axum [`Router`] for the management UI (mounted into the daemon's
/// HTTP server).
pub fn routes() -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/api/environments", get(list_environments))
        .route("/api/logs/{run}", get(get_logs))
        .route("/api/open-terminal", post(open_terminal))
        .route("/api/environments/{run}/stop", post(stop_environment))
        .route("/api/environments/{run}/restart", post(restart_environment))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn dashboard() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        DASHBOARD_HTML,
    )
        .into_response()
}

#[derive(Serialize)]
struct EnvironmentList {
    projects: Vec<ProjectInfo>,
}

#[derive(Serialize)]
struct ProjectInfo {
    name: String,
    project_root: String,
    runs: Vec<RunInfo>,
}

#[derive(Serialize)]
struct RunInfo {
    name: String,
    status: RunStatus,
    urls: HashMap<String, String>,
    nodes: Vec<NodeInfo>,
}

#[derive(Serialize)]
struct NodeInfo {
    name: String,
    variant: String,
    status: NodeStatus,
    url: Option<String>,
    pid: Option<u32>,
}

async fn list_environments() -> Result<Json<EnvironmentList>, StatusCode> {
    let registry = GlobalRegistry::load().map_err(|e| {
        warn!("failed to load global registry: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut projects: Vec<ProjectInfo> = registry
        .projects
        .values()
        .map(|entry| {
            // Load full project state for node-level detail.
            let project_state = ProjectState::load(&entry.project_root).ok();

            let mut runs: Vec<RunInfo> = entry
                .runs
                .values()
                .map(|r| {
                    let mut nodes: Vec<NodeInfo> = project_state
                        .as_ref()
                        .and_then(|ps| ps.get_run(&r.name))
                        .map(|rs| {
                            rs.nodes
                                .values()
                                .map(|ns| NodeInfo {
                                    name: ns.node_name.clone(),
                                    variant: ns.variant.clone(),
                                    status: ns.status.clone(),
                                    url: ns.url.clone(),
                                    pid: ns.pid,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    nodes.sort_by(|a, b| a.name.cmp(&b.name));

                    RunInfo {
                        name: r.name.clone(),
                        status: r.status.clone(),
                        urls: r.urls.clone(),
                        nodes,
                    }
                })
                .collect();
            runs.sort_by(|a, b| a.name.cmp(&b.name));

            ProjectInfo {
                name: entry.project_name.clone(),
                project_root: entry.project_root.display().to_string(),
                runs,
            }
        })
        .collect();

    projects.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(EnvironmentList { projects }))
}

// ---------------------------------------------------------------------------
// Log API
// ---------------------------------------------------------------------------

fn default_lines() -> usize {
    200
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default = "default_lines")]
    lines: usize,
    node: Option<String>,
}

#[derive(Serialize)]
struct LogResponse {
    nodes: Vec<NodeLogs>,
}

#[derive(Serialize)]
struct NodeLogs {
    node: String,
    variant: String,
    lines: Vec<String>,
}

/// Look up a run name in the global registry, returning the project root.
fn find_project_for_run(registry: &GlobalRegistry, run_name: &str) -> Option<std::path::PathBuf> {
    registry
        .projects
        .values()
        .find(|entry| entry.runs.contains_key(run_name))
        .map(|entry| entry.project_root.clone())
}

async fn get_logs(
    Path(run_name): Path<String>,
    Query(q): Query<LogQuery>,
) -> Result<Json<LogResponse>, StatusCode> {
    // Allow only safe characters in run names (alphanumeric, hyphens, underscores, dots).
    if !run_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let registry = GlobalRegistry::load().map_err(|e| {
        warn!("failed to load registry for logs: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let project_root = find_project_for_run(&registry, &run_name).ok_or(StatusCode::NOT_FOUND)?;

    let project_state = ProjectState::load(&project_root).map_err(|e| {
        warn!("failed to load project state for logs: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let run_state = project_state
        .get_run(&run_name)
        .ok_or(StatusCode::NOT_FOUND)?;

    let lines_limit = q.lines.clamp(1, 5000);
    let mut nodes = Vec::new();

    for ns in run_state.nodes.values() {
        if let Some(ref filter) = q.node {
            if ns.node_name != *filter {
                continue;
            }
        }

        let log_path = logging::log_file(&project_root, &run_name, &ns.node_name, &ns.variant);
        let lines = if log_path.exists() {
            logging::tail_lines(&log_path, lines_limit)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        nodes.push(NodeLogs {
            node: ns.node_name.clone(),
            variant: ns.variant.clone(),
            lines,
        });
    }

    nodes.sort_by(|a, b| a.node.cmp(&b.node));
    Ok(Json(LogResponse { nodes }))
}

// ---------------------------------------------------------------------------
// Open terminal
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OpenTerminalBody {
    path: String,
}

async fn open_terminal(Json(body): Json<OpenTerminalBody>) -> StatusCode {
    let path = std::path::Path::new(&body.path);
    if !path.is_dir() {
        return StatusCode::BAD_REQUEST;
    }

    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-a")
            .arg("Terminal")
            .arg(&body.path)
            .spawn()
    } else {
        // Try common Linux terminal emulators.
        std::process::Command::new("xdg-open")
            .arg(&body.path)
            .spawn()
    };

    match result {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(e) => {
            warn!("failed to open terminal at {}: {e}", body.path);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

// ---------------------------------------------------------------------------
// Stop / Restart
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct EnvActionBody {
    project_root: String,
}

async fn stop_environment(
    Path(run_name): Path<String>,
    Json(body): Json<EnvActionBody>,
) -> StatusCode {
    run_veld_command(&body.project_root, &["stop", "--name", &run_name]).await
}

async fn restart_environment(
    Path(run_name): Path<String>,
    Json(body): Json<EnvActionBody>,
) -> StatusCode {
    run_veld_command(&body.project_root, &["restart", "--name", &run_name]).await
}

/// Spawn `veld <args>` in the given project directory as a detached process.
async fn run_veld_command(project_root: &str, args: &[&str]) -> StatusCode {
    let path = std::path::Path::new(project_root);
    if !path.is_dir() {
        return StatusCode::BAD_REQUEST;
    }

    // The daemon binary is at ~/.local/lib/veld/veld-daemon.
    // The CLI binary is at ~/.local/bin/veld.
    // Go up two levels from lib/veld/ to ~/.local/, then into bin/veld.
    let veld_bin = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()?
                .parent()?
                .parent()
                .map(|d| d.join("bin").join("veld"))
        })
        .filter(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("veld"));

    match std::process::Command::new(&veld_bin)
        .args(args)
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => StatusCode::ACCEPTED,
        Err(e) => {
            warn!("failed to run veld {:?}: {e}", args);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
