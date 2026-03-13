use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::Notify;
use tracing::{info, warn};
use veld_core::feedback::{FeedbackComment, FeedbackStore};
use veld_core::state::GlobalRegistry;

#[path = "feedback_assets.rs"]
mod feedback_assets;

/// Port the feedback HTTP server listens on.
pub const FEEDBACK_PORT: u16 = 19899;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    /// Notifier for long-poll: signalled whenever a batch is submitted.
    batch_notify: Notify,
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// Start the feedback HTTP server on `127.0.0.1:FEEDBACK_PORT`.
pub async fn run_feedback_server() {
    let state = Arc::new(AppState {
        batch_notify: Notify::new(),
    });

    let app = Router::new()
        // Service Worker installer page.
        .route("/", get(installer_page))
        // Service Worker script.
        .route("/sw.js", get(service_worker))
        // Overlay script.
        .route("/feedback/script.js", get(overlay_script))
        // Feedback CRUD API.
        .route("/feedback/api/comments", get(list_comments))
        .route("/feedback/api/comments", post(create_comment))
        .route("/feedback/api/comments/{id}", put(update_comment))
        .route("/feedback/api/comments/{id}", delete(delete_comment))
        // Submit / batches.
        .route("/feedback/api/submit", post(submit_batch))
        .route("/feedback/api/batches", get(list_batches))
        .route("/feedback/api/batches/wait", get(wait_for_batch))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], FEEDBACK_PORT));
    info!("feedback server listening on {addr}");

    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            if let Err(e) = axum::serve(listener, app).await {
                warn!("feedback server error: {e}");
            }
        }
        Err(e) => {
            warn!("failed to bind feedback server on {addr}: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RunQuery {
    run: Option<String>,
    project: Option<String>,
}

#[derive(Deserialize)]
struct WaitQuery {
    run: Option<String>,
    project: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 {
    300
}

// ---------------------------------------------------------------------------
// Resolve project + run from query params or headers
// ---------------------------------------------------------------------------

fn resolve_store(
    run: Option<&str>,
    project: Option<&str>,
    headers: &axum::http::HeaderMap,
) -> Result<FeedbackStore, StatusCode> {
    let run_name = run
        .or_else(|| headers.get("x-veld-run").and_then(|v| v.to_str().ok()))
        .ok_or(StatusCode::BAD_REQUEST)?;

    // Try explicit project path first, then header, then search registry.
    if let Some(project_path) =
        project.or_else(|| headers.get("x-veld-project").and_then(|v| v.to_str().ok()))
    {
        return Ok(FeedbackStore::new(
            std::path::Path::new(project_path),
            run_name,
        ));
    }

    // Fallback: search the global registry for a project with this run.
    let registry = GlobalRegistry::load().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for entry in registry.projects.values() {
        if entry.runs.contains_key(run_name) {
            return Ok(FeedbackStore::new(&entry.project_root, run_name));
        }
    }

    Err(StatusCode::NOT_FOUND)
}

// ---------------------------------------------------------------------------
// Asset handlers
// ---------------------------------------------------------------------------

async fn installer_page() -> Html<&'static str> {
    Html(feedback_assets::INSTALLER_HTML)
}

async fn service_worker() -> Response {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        feedback_assets::SERVICE_WORKER_JS,
    )
        .into_response()
}

async fn overlay_script() -> Response {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        feedback_assets::OVERLAY_JS,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Comment CRUD
// ---------------------------------------------------------------------------

async fn list_comments(
    headers: axum::http::HeaderMap,
    Query(q): Query<RunQuery>,
) -> Result<Json<Vec<FeedbackComment>>, StatusCode> {
    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;
    let comments = store
        .get_comments()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(comments))
}

async fn create_comment(
    headers: axum::http::HeaderMap,
    Json(mut comment): Json<FeedbackComment>,
) -> Result<(StatusCode, Json<FeedbackComment>), StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    // Ensure ID and timestamps.
    if comment.id.is_empty() {
        comment.id = uuid::Uuid::new_v4().to_string();
    }
    let now = chrono::Utc::now();
    comment.created_at = now;
    comment.updated_at = now;

    store
        .save_comment(&comment)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(comment)))
}

async fn update_comment(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(mut comment): Json<FeedbackComment>,
) -> Result<Json<FeedbackComment>, StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    comment.id = id;
    comment.updated_at = chrono::Utc::now();

    if store
        .update_comment(&comment)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        Ok(Json(comment))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn delete_comment(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    if store
        .delete_comment(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

// ---------------------------------------------------------------------------
// Batch submission
// ---------------------------------------------------------------------------

async fn submit_batch(
    headers: axum::http::HeaderMap,
    state: State<Arc<AppState>>,
) -> Result<Json<veld_core::feedback::FeedbackBatch>, StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    let batch = store
        .submit_batch()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Notify any long-poll waiters.
    state.batch_notify.notify_waiters();

    Ok(Json(batch))
}

async fn list_batches(
    headers: axum::http::HeaderMap,
    Query(q): Query<RunQuery>,
) -> Result<Json<Vec<veld_core::feedback::FeedbackBatch>>, StatusCode> {
    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;
    let batches = store
        .get_batches()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(batches))
}

async fn wait_for_batch(
    headers: axum::http::HeaderMap,
    Query(q): Query<WaitQuery>,
    state: State<Arc<AppState>>,
) -> Result<Json<Vec<veld_core::feedback::FeedbackBatch>>, StatusCode> {
    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;

    let timeout = tokio::time::Duration::from_secs(q.timeout_secs.min(600));

    // Wait for a notification or timeout.
    let _ = tokio::time::timeout(timeout, state.batch_notify.notified()).await;

    // Return all batches (the caller can filter by timestamp).
    let batches = store
        .get_batches()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(batches))
}
