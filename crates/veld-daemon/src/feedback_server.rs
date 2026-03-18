use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::Notify;
use tracing::{info, warn};
use veld_core::feedback::{
    Author, EventType, FeedbackStore, Message, Thread, ThreadOrigin, ThreadScope, ThreadStatus,
    new_message, new_thread,
};
use veld_core::state::GlobalRegistry;

#[path = "feedback_assets.rs"]
mod feedback_assets;

#[path = "management.rs"]
mod management;

/// Port the feedback HTTP server listens on.
pub const FEEDBACK_PORT: u16 = 19899;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    /// Notifier for event polling: signalled whenever a new event is appended.
    event_notify: Notify,
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// Start the feedback HTTP server on `127.0.0.1:FEEDBACK_PORT`.
pub async fn run_feedback_server() {
    let state = Arc::new(AppState {
        event_notify: Notify::new(),
    });

    let app = Router::new()
        // Overlay assets (injected by Caddy's replace-response handler).
        .route("/feedback/script.js", get(overlay_script))
        .route("/feedback/style.css", get(overlay_css))
        .route("/feedback/logo.svg", get(logo_svg))
        // Thread API.
        .route("/feedback/api/threads", get(list_threads))
        .route("/feedback/api/threads", post(create_thread))
        .route("/feedback/api/threads/{id}", get(get_thread))
        .route(
            "/feedback/api/threads/{id}/messages",
            post(add_thread_message),
        )
        .route("/feedback/api/threads/{id}/resolve", post(resolve_thread))
        .route("/feedback/api/threads/{id}/reopen", post(reopen_thread))
        .route("/feedback/api/threads/{id}/seen", put(mark_thread_seen))
        // Event API.
        .route("/feedback/api/events", get(get_events))
        // Session API (browser polls to show "Agent is listening").
        .route("/feedback/api/session", get(get_session))
        .route("/feedback/api/session/end", post(end_session))
        // Screenshots (unchanged).
        .route("/feedback/api/screenshots/{id}", post(upload_screenshot))
        .route("/feedback/api/screenshots/{id}", get(get_screenshot))
        .with_state(state)
        // Management UI (served at veld.localhost via Caddy, also reachable
        // directly on this port for debugging). Merged after with_state()
        // because management routes are stateless.
        .merge(management::routes());

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
struct ThreadListQuery {
    run: Option<String>,
    project: Option<String>,
    status: Option<String>,
    page_url: Option<String>,
}

#[derive(Deserialize)]
struct EventQuery {
    run: Option<String>,
    project: Option<String>,
    #[serde(default)]
    after: u64,
}

#[derive(Deserialize)]
struct SeenBody {
    seq: u64,
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
// Thread API
// ---------------------------------------------------------------------------

async fn list_threads(
    headers: axum::http::HeaderMap,
    Query(q): Query<ThreadListQuery>,
) -> Result<Json<Vec<Thread>>, StatusCode> {
    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;

    let status_filter = match q.status.as_deref() {
        Some("open") => Some(ThreadStatus::Open),
        Some("resolved") => Some(ThreadStatus::Resolved),
        _ => None,
    };

    let mut threads = store
        .list_threads(status_filter)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Filter by page URL if requested.
    if let Some(ref page_url) = q.page_url {
        let pathname = page_url.split('?').next().unwrap_or(page_url);
        threads.retain(|t| match &t.scope {
            ThreadScope::Element { page_url: pu, .. } | ThreadScope::Page { page_url: pu } => {
                let tp = pu.split('?').next().unwrap_or(pu);
                tp == pathname
            }
            ThreadScope::Global => true, // global threads always included
        });
    }

    Ok(Json(threads))
}

#[derive(Deserialize)]
struct CreateThreadBody {
    scope: ThreadScope,
    #[serde(default)]
    component_trace: Option<Vec<String>>,
    message: String,
    #[serde(default)]
    screenshot: Option<String>,
    #[serde(default)]
    viewport_width: Option<u32>,
    #[serde(default)]
    viewport_height: Option<u32>,
}

async fn create_thread(
    headers: axum::http::HeaderMap,
    state: State<Arc<AppState>>,
    Json(body): Json<CreateThreadBody>,
) -> Result<(StatusCode, Json<Thread>), StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    let msg = new_message(Author::Human, &body.message, body.screenshot);
    let thread = new_thread(
        body.scope,
        ThreadOrigin::Human,
        body.component_trace,
        body.viewport_width,
        body.viewport_height,
        msg,
    );

    store
        .save_thread(&thread)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    store
        .append_event(EventType::ThreadCreated {
            thread: thread.clone(),
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    state.event_notify.notify_waiters();
    Ok((StatusCode::CREATED, Json(thread)))
}

async fn get_thread(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<RunQuery>,
) -> Result<Json<Thread>, StatusCode> {
    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;

    store
        .get_thread(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

#[derive(Deserialize)]
struct AddMessageBody {
    body: String,
    #[serde(default)]
    screenshot: Option<String>,
}

async fn add_thread_message(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    state: State<Arc<AppState>>,
    Json(body): Json<AddMessageBody>,
) -> Result<(StatusCode, Json<Message>), StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    let msg = new_message(Author::Human, &body.body, body.screenshot);

    store
        .add_message(&id, &msg)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    store
        .append_event(EventType::HumanMessage {
            thread_id: id,
            message: msg.clone(),
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    state.event_notify.notify_waiters();
    Ok((StatusCode::CREATED, Json(msg)))
}

async fn resolve_thread(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    state: State<Arc<AppState>>,
) -> Result<Json<Thread>, StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    let thread = store
        .set_thread_status(&id, ThreadStatus::Resolved)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    store
        .append_event(EventType::Resolved { thread_id: id })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    state.event_notify.notify_waiters();
    Ok(Json(thread))
}

async fn reopen_thread(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    state: State<Arc<AppState>>,
) -> Result<Json<Thread>, StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    let thread = store
        .set_thread_status(&id, ThreadStatus::Open)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    store
        .append_event(EventType::Reopened { thread_id: id })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    state.event_notify.notify_waiters();
    Ok(Json(thread))
}

async fn mark_thread_seen(
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SeenBody>,
) -> Result<StatusCode, StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    store
        .mark_thread_seen(&id, body.seq)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Event API
// ---------------------------------------------------------------------------

async fn get_events(
    headers: axum::http::HeaderMap,
    Query(q): Query<EventQuery>,
) -> Result<Json<Vec<veld_core::feedback::Event>>, StatusCode> {
    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;

    let events = store
        .get_events_after(q.after)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(events))
}

// ---------------------------------------------------------------------------
// Session API
// ---------------------------------------------------------------------------

async fn get_session(
    headers: axum::http::HeaderMap,
    Query(q): Query<RunQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let store = resolve_store(q.run.as_deref(), q.project.as_deref(), &headers)?;

    let listening = store
        .is_listening(60)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({ "listening": listening })))
}

async fn end_session(
    headers: axum::http::HeaderMap,
    state: State<Arc<AppState>>,
) -> Result<StatusCode, StatusCode> {
    let run_name = headers
        .get("x-veld-run")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let store = resolve_store(Some(run_name), None, &headers)?;

    store
        .append_event(EventType::SessionEnded)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    store
        .end_session()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    state.event_notify.notify_waiters();
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Screenshots (unchanged)
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

    // Validate: max 10 MB.
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
