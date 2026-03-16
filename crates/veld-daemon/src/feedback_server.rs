use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
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
        // Overlay assets (injected by Caddy's replace-response handler).
        .route("/feedback/script.js", get(overlay_script))
        .route("/feedback/style.css", get(overlay_css))
        .route("/feedback/logo.svg", get(logo_svg))
        // Feedback CRUD API.
        .route("/feedback/api/comments", get(list_comments))
        .route("/feedback/api/comments", post(create_comment))
        .route("/feedback/api/comments/{id}", put(update_comment))
        .route("/feedback/api/comments/{id}", delete(delete_comment))
        // Screenshots.
        .route("/feedback/api/screenshots/{id}", post(upload_screenshot))
        .route("/feedback/api/screenshots/{id}", get(get_screenshot))
        // Submit / batches.
        .route("/feedback/api/submit", post(submit_batch))
        .route("/feedback/api/batches", get(list_batches))
        .route("/feedback/api/batches/wait", get(wait_for_batch))
        // Wait-session signalling (browser ↔ CLI).
        .route("/feedback/api/wait-status", get(get_wait_status))
        .route("/feedback/api/cancel", post(cancel_feedback))
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
    page_url: Option<String>,
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

    // Reject run names containing path separators to prevent path traversal.
    if run_name.contains('/') || run_name.contains('\\') || run_name.contains("..") {
        return Err(StatusCode::BAD_REQUEST);
    }

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

async fn overlay_script() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        feedback_assets::OVERLAY_JS,
    )
        .into_response()
}

async fn overlay_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        feedback_assets::OVERLAY_CSS,
    )
        .into_response()
}

async fn logo_svg() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        feedback_assets::LOGO_SVG,
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
    let mut comments = store
        .get_comments()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Filter by page URL (pathname only) when requested by the overlay.
    if let Some(ref page_url) = q.page_url {
        let pathname = page_url.split('?').next().unwrap_or(page_url);
        comments.retain(|c| {
            let comment_path = c.page_url.split('?').next().unwrap_or(&c.page_url);
            comment_path == pathname
        });
    }

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
// Screenshots
// ---------------------------------------------------------------------------

async fn upload_screenshot(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    // Reject IDs containing path separators to prevent path traversal.
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(StatusCode::BAD_REQUEST);
    }

    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    // Validate: must look like PNG data and be reasonably sized (max 10 MB).
    if body.len() > 10 * 1024 * 1024 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    store
        .save_screenshot(&id, &body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::CREATED)
}

async fn get_screenshot(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<RunQuery>,
) -> Result<Response, StatusCode> {
    // Reject IDs containing path separators to prevent path traversal.
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(StatusCode::BAD_REQUEST);
    }

    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;
    let filename = format!("{id}.png");
    let path = store.screenshot_path(&filename);

    let data = std::fs::read(&path).map_err(|_| StatusCode::NOT_FOUND)?;

    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        data,
    )
        .into_response())
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

// ---------------------------------------------------------------------------
// Wait-session signalling
// ---------------------------------------------------------------------------

/// Returns `{ "waiting": true/false, "wait_id": "..." }` for the overlay to poll.
async fn get_wait_status(
    headers: axum::http::HeaderMap,
    Query(q): Query<RunQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;
    let waiting = store.is_waiting();
    let wait_id = if waiting { store.waiting_id() } else { None };
    Ok(Json(serde_json::json!({ "waiting": waiting, "wait_id": wait_id })))
}

/// Reviewer cancels the feedback session.
async fn cancel_feedback(
    headers: axum::http::HeaderMap,
    state: State<Arc<AppState>>,
) -> Result<StatusCode, StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    store
        .cancel()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Notify waiters so the CLI picks up the cancellation quickly.
    state.batch_notify.notify_waiters();

    Ok(StatusCode::NO_CONTENT)
}
