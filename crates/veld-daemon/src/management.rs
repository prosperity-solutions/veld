//! Browser-based management dashboard served at `_veld.localhost`.
//!
//! Provides a read-only overview of all Veld environments on the machine,
//! with clickable service URLs and live status badges.

use std::collections::HashMap;

use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::warn;
use veld_core::logging;
use veld_core::state::{GlobalRegistry, ProjectState, RunStatus};

const DASHBOARD_HTML: &str = include_str!("../assets/management-ui.html");

/// Build an axum [`Router`] for the management UI (mounted into the daemon's
/// HTTP server).
pub fn routes() -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/api/environments", get(list_environments))
        .route("/api/logs/{run}", get(get_logs))
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
            let mut runs: Vec<RunInfo> = entry
                .runs
                .values()
                .map(|r| RunInfo {
                    name: r.name.clone(),
                    status: r.status.clone(),
                    urls: r.urls.clone(),
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
