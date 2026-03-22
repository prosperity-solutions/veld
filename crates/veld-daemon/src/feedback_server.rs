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
use veld_core::logging;
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
        // Client log collector (injected into <head> by Caddy).
        .route("/api/client-log.js", get(client_log_script))
        // Client log ingest endpoint (2MB body limit).
        .route(
            "/api/client-logs",
            post(ingest_client_logs).layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        // Overlay assets (loaded dynamically by the veld_inject bootstrap script).
        .route("/feedback/script.js", get(overlay_script))
        .route("/feedback/draw.js", get(draw_script))
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
            (header::CACHE_CONTROL, "no-cache"),
        ],
        feedback_assets::OVERLAY_JS,
    )
        .into_response()
}


async fn draw_script() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        feedback_assets::DRAW_JS,
    )
        .into_response()
}

async fn logo_svg() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        feedback_assets::LOGO_SVG,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Client log collector
// ---------------------------------------------------------------------------

async fn client_log_script() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        feedback_assets::CLIENT_LOG_JS,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Client log ingest
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ClientLogBatch {
    entries: Vec<ClientLogEntry>,
}

#[derive(Deserialize)]
struct ClientLogEntry {
    ts: String,
    level: String,
    msg: String,
    #[serde(default)]
    stack: Option<String>,
}

/// Find the largest byte index <= `max_bytes` that is a valid UTF-8 char boundary.
fn safe_truncate_boundary(s: &str, max_bytes: usize) -> usize {
    if s.len() <= max_bytes {
        return s.len();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

async fn ingest_client_logs(
    headers: axum::http::HeaderMap,
    Json(batch): Json<ClientLogBatch>,
) -> StatusCode {
    // Limit batch size to prevent abuse.
    if batch.entries.len() > 500 {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }

    // Resolve run/project from Caddy-injected headers.
    let run_name = match headers.get("x-veld-run").and_then(|v| v.to_str().ok()) {
        Some(r) => r,
        None => return StatusCode::BAD_REQUEST,
    };

    // Validate run name to prevent path traversal.
    if run_name.is_empty()
        || run_name.contains('/')
        || run_name.contains('\\')
        || run_name.contains("..")
        || !run_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return StatusCode::BAD_REQUEST;
    }

    // Resolve node:variant from the Host header via the registry.
    let host = match headers.get("host").and_then(|v| v.to_str().ok()) {
        Some(h) => h.to_string(),
        None => return StatusCode::BAD_REQUEST,
    };

    // Look up the project root from the registry instead of trusting the header.
    // This prevents path traversal via crafted X-Veld-Project values.
    let registry = match GlobalRegistry::load() {
        Ok(r) => r,
        Err(e) => {
            warn!("failed to load registry for client logs: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let project_path = match registry
        .projects
        .values()
        .find(|entry| entry.runs.contains_key(run_name))
        .map(|entry| entry.project_root.clone())
    {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND,
    };
    let project_state = match veld_core::state::ProjectState::load(&project_path) {
        Ok(s) => s,
        Err(e) => {
            warn!("failed to load project state for client logs: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let run_state = match project_state.get_run(run_name) {
        Some(r) => r,
        None => return StatusCode::NOT_FOUND,
    };

    // Find the node whose URL matches this host.
    let mut node_name = None;
    let mut variant_name = None;
    for ns in run_state.nodes.values() {
        if let Some(ref url) = ns.url {
            // Compare hostname from the URL against the Host header.
            let url_host = url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or("");
            if url_host == host || url_host == host.split(':').next().unwrap_or("") {
                node_name = Some(ns.node_name.clone());
                variant_name = Some(ns.variant.clone());
                break;
            }
        }
    }

    let (node, variant) = match (node_name, variant_name) {
        (Some(n), Some(v)) => (n, v),
        _ => {
            warn!("could not resolve host '{host}' to a node for client logs");
            return StatusCode::NOT_FOUND;
        }
    };

    // Write entries to the client log file.
    // Build the entire batch as a single string, then write it in one call
    // to avoid interleaving with concurrent requests from other tabs.
    let log_path = logging::client_log_file(&project_path, run_name, &node, &variant);
    let writer = match logging::LogWriter::new(log_path).await {
        Ok(w) => w,
        Err(e) => {
            warn!("failed to create client log writer: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let mut batch_buf = String::new();
    for entry in &batch.entries {
        // Sanitize timestamp: strip characters that could break log line parsing.
        let sanitized_ts = entry
            .ts
            .chars()
            .filter(|c| !matches!(c, '\n' | '\r' | '[' | ']'))
            .take(40) // ISO 8601 is at most ~30 chars
            .collect::<String>();
        if sanitized_ts.is_empty() {
            continue;
        }
        // Sanitize the message: replace newlines to preserve log line format.
        // Truncate to 32KB to prevent abuse from forged requests.
        // Use a char boundary to avoid panicking on multi-byte UTF-8.
        let msg_truncated = if entry.msg.len() > 32_768 {
            let end = safe_truncate_boundary(&entry.msg, 32_768);
            format!("{}...(truncated)", &entry.msg[..end])
        } else {
            entry.msg.clone()
        };
        let sanitized_msg = msg_truncated.replace('\n', "\\n").replace('\r', "\\r");
        // Validate level against known values.
        let level = match entry.level.as_str() {
            "log" | "warn" | "error" | "info" | "debug" | "exception" => &entry.level,
            _ => continue,
        };
        // Format: [client_timestamp] [level] message\n    stack_line\n...
        let mut line = format!("[{}] [{}] {}", sanitized_ts, level, sanitized_msg);
        if let Some(ref stack) = entry.stack {
            // Limit stack trace to first 50 frames / 16KB to prevent abuse.
            let stack_end = safe_truncate_boundary(stack, 16_384);
            let stack_slice = &stack[..stack_end];
            let mut frame_count = 0;
            for stack_line in stack_slice.lines() {
                let trimmed = stack_line.trim();
                if !trimmed.is_empty() {
                    line.push('\n');
                    line.push_str("    ");
                    line.push_str(&trimmed.replace('\r', ""));
                    frame_count += 1;
                    if frame_count >= 50 {
                        break;
                    }
                }
            }
        }
        line.push('\n');
        batch_buf.push_str(&line);
    }

    if !batch_buf.is_empty() {
        if let Err(e) = writer.write_raw(&batch_buf).await {
            warn!("failed to write client log batch: {e}");
        }
    }

    StatusCode::NO_CONTENT
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
            (header::CACHE_CONTROL, "no-cache"),
        ],
        data,
    )
        .into_response())
}
