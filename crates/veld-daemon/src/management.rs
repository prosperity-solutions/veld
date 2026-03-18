//! Browser-based management dashboard served at `_veld.localhost`.
//!
//! Provides a read-only overview of all Veld environments on the machine,
//! with clickable service URLs and live status badges.

use std::collections::HashMap;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing::warn;
use veld_core::state::{GlobalRegistry, RunStatus};

const DASHBOARD_HTML: &str = include_str!("../assets/management-ui.html");

/// Build an axum [`Router`] for the management UI (mounted into the daemon's
/// HTTP server).
pub fn routes() -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/api/environments", get(list_environments))
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
