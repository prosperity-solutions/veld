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
use veld_core::db::Db;
use veld_core::feedback::{
    Author, EventType, FeedbackStore, Message, Thread, ThreadOrigin, ThreadScope, ThreadStatus,
    new_message, new_thread,
};
use veld_core::logging::LogWriter;

#[path = "feedback_assets.rs"]
mod feedback_assets;

#[path = "management.rs"]
pub mod management;

#[path = "desktop.rs"]
mod desktop;

#[path = "worktree_trash.rs"]
pub mod worktree_trash;

#[path = "pty.rs"]
mod pty;

#[path = "settings.rs"]
mod settings;

#[path = "config_vars.rs"]
mod config_vars;

/// Note the terminal sessions being left running.
///
/// Re-exported for the daemon's shutdown path. It no longer *ends* anything: a
/// session's PTY belongs to a holder process, so the shells survive a restart —
/// which is what makes `veld update` safe to run with terminals open. See
/// `pty::shutdown_sessions`.
pub async fn shutdown_terminal_sessions() {
    pty::shutdown_sessions().await;
}

/// Serve one terminal session as a holder process (`veld-daemon --pty-holder`).
///
/// Re-exported for `main`, which dispatches to it before any of the daemon's own
/// startup: a holder binds no port, opens no database, and must not be mistaken
/// for a second daemon.
pub async fn run_pty_holder() -> anyhow::Result<()> {
    pty::holder::run_from_stdin().await
}

// The feedback HTTP server listens on this instance's daemon port —
// `veld_core::instance::daemon_port()` (19899 for the installed instance;
// a dev instance overrides via VELD_DAEMON_PORT).

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

/// Start the feedback HTTP server on 127.0.0.1 at this instance's daemon
/// port (`veld_core::instance::daemon_port()`).
pub async fn run_feedback_server(share_manager: Arc<crate::share::manager::ShareManager>) {
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
        .merge(management::routes())
        .merge(desktop::routes())
        // User preferences. Kept out of desktop::routes() because settings are
        // not desktop-specific (runs mode reads them too) and that router's
        // blanket CSRF layer only covers routes registered before it.
        .merge(settings::routes())
        // Machine-overridable vars. Same reasoning as settings — not
        // desktop-specific, and it needs its own CSRF checks rather than that
        // router's blanket layer.
        .merge(config_vars::routes())
        // Terminal sockets. Kept out of desktop::routes() because that
        // router's CSRF layer cannot gate a WebSocket upgrade — see pty.rs.
        .merge(pty::routes())
        .merge(crate::share::api::routes(share_manager));

    // Terminal sessions outlive their socket (so a page reload keeps its
    // shell), which means something has to collect the ones nobody comes back
    // for. Started here rather than in pty::routes() so that building a router
    // in a test doesn't leave a timer running.
    // Holders whose daemon went away are still serving their shells; this is
    // where a fresh daemon takes them back. Before the reaper, so an adopted
    // session's detach clock is running by the time anything can collect it, and
    // before the listener binds, so no attach can race a half-adopted registry.
    pty::adopt_existing_sessions().await;
    pty::spawn_session_reaper();
    // The `$BROWSER` shims every new session's environment points at. Here rather
    // than on the first terminal so a machine that cannot have them says so in the
    // startup log, and so `$VELD_SHIM_DIR` is usable from the first shell.
    pty::prepare_shims();
    // Worktree removals recorded but not finished by a previous daemon resume
    // here — `worktrees.trashed_at` is the durable record, so a crash mid-removal
    // costs a restart, not a stuck row. Started for the same reason the reaper is
    // (not inside `routes()`), so building a router in a test starts no worker.
    worktree_trash::spawn();

    let addr = SocketAddr::from(([127, 0, 0, 1], veld_core::instance::daemon_port()));
    info!("feedback server listening on {addr}");

    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            // **After the bind, because binding the port is what proves this process
            // owns this instance's state.** `pty_dir` — and so the shim directory — is
            // keyed on the daemon port alone, so a second daemon started on the default
            // port (a bare `cargo run -p veld-daemon`, which also builds no `veld`
            // sibling to resolve) would otherwise delete the *installed*, running
            // daemon's shims and then fail to bind and keep going, leaving every
            // terminal opened afterwards without `$BROWSER` until that daemon restarts.
            // Writing them before the bind is harmless — it is idempotent and produces
            // the same bytes — but deleting is not.
            pty::clear_unbacked_shims();
            if let Err(e) = axum::serve(listener, app).await {
                warn!("feedback server error: {e}");
            }
        }
        Err(e) => {
            warn!(
                "failed to bind feedback server on {addr}: {e} — is another \
                 veld-daemon instance already running on this port? The daemon \
                 will keep running WITHOUT its HTTP API (no dashboard, no \
                 feedback, no shares) until restarted"
            );
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

/// Read `X-Veld-Project` as UTF-8.
///
/// NOT `HeaderValue::to_str()`, which rejects every byte >= 0x80 (it only
/// accepts visible ASCII). This header carries a filesystem path — Caddy sets it
/// from `project_root.to_string_lossy()` — so a project living under
/// `/Users/José/app` or `~/项目/app` produces a perfectly valid header that
/// `to_str()` refuses to read. Parsing the raw bytes covers every path the
/// database can store, since both `root_key` and `Path::display()` are lossy
/// UTF-8 round-trips of the same value.
fn project_header(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get("x-veld-project")?;
    let value = std::str::from_utf8(raw.as_bytes()).ok()?;
    // Trim only to decide emptiness; the value is used verbatim. Not because a
    // surrounding space could survive the wire (HTTP field parsing strips
    // leading/trailing OWS, and HTTP/2 forbids it) but so this reader never
    // rewrites a filesystem path it is about to match for equality.
    if value.trim().is_empty() {
        return None;
    }
    Some(value.to_owned())
}

fn resolve_store(
    run: Option<&str>,
    project: Option<&str>,
    headers: &axum::http::HeaderMap,
) -> Result<FeedbackStore, StatusCode> {
    let run_name = run
        .or_else(|| headers.get("x-veld-run").and_then(|v| v.to_str().ok()))
        .ok_or(StatusCode::BAD_REQUEST)?;

    // Reject run names containing path separators — they can't be valid run
    // names and would otherwise leak into scope keys.
    if run_name.contains('/') || run_name.contains('\\') || run_name.contains("..") {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Open per request: cheap for a local server, and it self-heals across
    // CLI upgrades that migrate the schema.
    let db = Db::open().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // A caller-supplied `?project=` is validated against the registry; the
    // Caddy-injected header is not, and the asymmetry is the point.
    //
    // Caddy `set`s `X-Veld-Project` server-side, overriding anything the client
    // sends, so it is as trustworthy as the route itself — and validating it
    // would 404 reads of threads whose run has been garbage collected, since
    // threads outlive their run. The query param is the opposite: it is pure
    // client input, it *beats* the header below, and naming another project's
    // real root would return that project's real threads (not, as previously
    // claimed here, a harmless empty scope).
    //
    // The reach is LOCAL ONLY — an earlier version of this comment claimed a
    // `veld share --web` page could read across projects, which is false: a
    // web-shared page is dialled straight at the node's app port
    // (`veld-share`'s host), bypassing Caddy, so it gets neither the
    // `/__veld__/*` subroute nor the injected overlay, and this server binds
    // 127.0.0.1. So this is hardening against local callers, not a remote hole.
    //
    // Validating it inherits the same limitation as validating the header would:
    // `resolve_run_project` needs the (project, run) pair in the registry, and a
    // garbage-collected environment isn't, so threads outliving GC become
    // unreachable through this param. That's acceptable only because nothing
    // shipped sends it — the overlay relies on the header, and the CLI uses
    // `FeedbackStore` directly — so there is no caller to regress. It does NOT
    // require the run to be *live*: the registry keeps each environment's latest
    // run whatever its status, so reads on a stopped run still work.
    if let Some(project_path) = project {
        let registry = db
            .registry()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let root = management::resolve_run_project(&registry, project_path, run_name)?;
        return Ok(FeedbackStore::new(db, &root, run_name));
    }
    if let Some(project_path) = project_header(headers) {
        return Ok(FeedbackStore::new(
            db,
            std::path::Path::new(&project_path),
            run_name,
        ));
    }

    // Fallback: search the global registry for a project with this run. Run
    // names are unique per project, not globally (two repos both on `main`
    // each have an environment called `main`), so two matches are a 409 — the
    // first-match answer would silently read and write another project's
    // feedback threads.
    let registry = db
        .registry()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut holders = registry
        .projects
        .values()
        .filter(|entry| entry.runs.contains_key(run_name));
    match (holders.next(), holders.next()) {
        (Some(only), None) => Ok(FeedbackStore::new(db, &only.project_root, run_name)),
        (None, _) => Err(StatusCode::NOT_FOUND),
        (Some(a), Some(b)) => {
            // These handlers answer with status codes only, so a bare 409 is
            // undiagnosable — log the candidates. Sorted and complete: a
            // message whose contents shuffle between runs, or that truncates to
            // the first two of N, is the same class of defect as the resolution
            // bug this scoping exists to fix.
            let mut roots: Vec<String> = [a, b]
                .into_iter()
                .chain(holders)
                .map(|e| e.project_root.display().to_string())
                .collect();
            roots.sort_unstable();
            warn!(
                "feedback scope for run '{run_name}' is ambiguous across {} \
                 projects ({}) — reach the overlay through Caddy so it injects \
                 X-Veld-Project, which resolves this exactly",
                roots.len(),
                roots.join(", "),
            );
            Err(StatusCode::CONFLICT)
        }
    }
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

    // The project half of the address. In THIS handler `X-Veld-Project` is a
    // *lookup key*, not a path — `resolve_store` above deliberately uses the
    // same header verbatim as a path, for the reasons given there.
    // `resolve_run_project` accepts it only if the registry
    // records that exact project running that exact name, so a crafted value
    // can't invent a scope key or point at a directory the daemon doesn't know.
    // Never resolve the run name alone here — names repeat across projects, so
    // a first-match scan writes one repo's console logs under another's
    // (project, run_id). See `RunScope` in management.rs.
    let project_header = match project_header(&headers) {
        Some(p) => p,
        None => {
            // The client discards this response (the XHR has no handler and
            // `sendBeacon` reports nothing), so without this line a broken
            // Caddy invariant would kill client logging with no diagnostic
            // anywhere.
            warn!(
                "client log batch for run '{run_name}' has no readable \
                 X-Veld-Project header — is it reaching the daemon through \
                 Caddy's /__veld__/ route?"
            );
            return StatusCode::BAD_REQUEST;
        }
    };

    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => {
            warn!("failed to open database for client logs: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    let registry = match db.registry() {
        Ok(r) => r,
        Err(e) => {
            warn!("failed to load registry for client logs: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let project_path = match management::resolve_run_project(&registry, &project_header, run_name) {
        Ok(p) => p,
        Err(code) => return code,
    };

    let run_state = match db.get_run(&project_path, run_name) {
        Ok(Some(r)) => r,
        Ok(None) => return StatusCode::NOT_FOUND,
        Err(e) => {
            warn!("failed to load run state for client logs: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
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

    // Write entries to the client log stream. Each entry becomes one row,
    // timestamped with the client-provided timestamp (falling back to the
    // ingest time when it doesn't parse).
    let writer = LogWriter::for_node(
        db,
        &project_path,
        run_name,
        run_state.run_id,
        &node,
        &variant,
        veld_core::db::LogStream::Client,
    );

    for entry in &batch.entries {
        let ts = chrono::DateTime::parse_from_rfc3339(entry.ts.trim())
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        // Sanitize the message: replace newlines to keep one entry per row.
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
        // Format: [level] message\n    stack_line\n...
        let mut line = format!("[{}] {}", level, sanitized_msg);
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
        if let Err(e) = writer.write_with_ts(ts, &line) {
            warn!("failed to write client log entry: {e}");
            break;
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

    let msg = new_message(Author::Human, &body.message, body.screenshot, None);
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

    let msg = new_message(Author::Human, &body.body, body.screenshot, None);

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

    // Don't report "listening" once the reviewer clicked Done — even while the
    // agent drains the last items — so the FAB doesn't re-pulse after Done.
    let listening = store
        .is_listening(60)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        && !store
            .is_ended()
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

    let data = store
        .get_screenshot(&filename)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        data,
    )
        .into_response())
}
