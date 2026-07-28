//! Interactive terminal sessions: a PTY spawned daemon-side ([`portable_pty`])
//! and bridged to the browser over a WebSocket.
//!
//! The PTY lives in the daemon rather than in the Electron main process
//! (`node-pty`) because the plain-browser build of `/ide` needs a daemon route
//! regardless: doing it in Electron would mean two implementations of one
//! feature and would break the "the web UI must stay fully usable without
//! Electron" invariant in `desktop/ARCHITECTURE.md`.
//!
//! # Sessions outlive their socket
//!
//! A terminal is not re-creatable state — closing it kills the shell and
//! everything running in it — so a session is **not** tied to the WebSocket
//! that happens to be showing it. Reloading the page, or Electron reloading
//! the window, drops the socket; the shell keeps running, its output keeps
//! accumulating in a [`Scrollback`] ring, and the next attach replays that
//! buffer and continues live. Consequences worth knowing before changing
//! anything here:
//!
//! - **The client names the session.** `session_id` comes from the browser
//!   (`crypto.randomUUID()`), stored in `sessionStorage` beside the pane
//!   layout, so after a reload the page can ask for the same shell back. It is
//!   a *name*, not a credential — the gates below are what authorise an
//!   attach.
//! - **A second attach takes over** rather than mirroring, the way
//!   `tmux attach -d` does. One writer, no ambiguity about who owns the input
//!   stream; the displaced socket is told why and closed.
//! - **Ending a session is explicit.** Closing a tab sends
//!   `DELETE /api/pty/sessions/{id}`. Everything else (a reload, a crash, a
//!   closed laptop) leaves it detached, and [`DETACH_GRACE`] is what
//!   eventually collects it — otherwise every abandoned tab would leak a shell
//!   for the daemon's lifetime.
//!
//! # Why this endpoint is authenticated differently from every other route
//!
//! The rest of the mutating API is gated by [`check_csrf`] — presence of the
//! custom `X-Veld-Request` header, which a cross-origin page cannot set
//! without provoking a CORS preflight the daemon never answers.
//! **WebSocket handshakes neither honour CORS nor carry custom headers**, so
//! that gate is structurally unreachable here. An unguarded PTY socket would
//! mean any page in any tab could run `new WebSocket("ws://127.0.0.1:19899/…")`
//! and get a shell on the user's machine. "It's only on loopback" was never
//! the mitigation, and it isn't one here: the helper also publishes this
//! daemon at `https://veld.localhost` (`crates/veld-helper/src/caddy.rs`).
//!
//! Two independent gates replace the one that doesn't apply:
//!
//! 1. **A single-use ticket.** `POST /api/pty/tickets` *is* CSRF-gated, and
//!    its response body is unreadable cross-origin because the daemon serves
//!    no CORS headers anywhere. The ticket is 122 bits from the OS CSRNG
//!    (`uuid` v4 → `getrandom`), expires in [`TICKET_TTL`], is consumed on
//!    first use, and carries the worktree directory it was minted for — the
//!    client never names a path, so there is nothing to traverse.
//! 2. **An `Origin` allowlist on the upgrade** ([`origin_allowed`]), failing
//!    closed when `Origin` is absent or unparseable.
//!
//! Either gate alone would hold today. Keep both: gate 1 weakens the day a
//! CORS layer is added to the daemon, gate 2 weakens if a client is ever
//! legitimately allowed to connect without an `Origin`. Removing one because
//! "the other covers it" removes the margin, not redundancy.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, MasterPty, PtyPair, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::io::unix::AsyncFd;
use tokio::sync::{broadcast, watch};
use tracing::{debug, info, warn};

use super::management::{check_csrf, open_db};

/// How long a minted ticket stays redeemable. Long enough to survive a slow
/// render and a wedged event loop, short enough that a leaked one (a shared
/// screen, a stray log) is stale before it can be used.
const TICKET_TTL: Duration = Duration::from_secs(30);

/// Ceiling on live sessions across all worktrees. Each one is a real shell
/// process plus file descriptors, a task and a scrollback buffer; without a cap
/// a scripted client (or a UI bug in a render loop) forks until the machine
/// gives up.
///
/// Sized for how the UI actually allocates: selecting a worktree opens a
/// terminal in its default layout, and that shell stays alive while the page
/// does — so *browsing* the worktree rail spends this budget, one shell per
/// worktree visited, not just deliberately opening terminals. Hitting the cap
/// is therefore an ordinary outcome rather than an attack, which is why
/// [`mint_ticket`] reports it as a readable error instead of leaving the client
/// to infer it from a failed handshake.
const MAX_SESSIONS: usize = 48;

/// How long a session with nobody attached keeps running.
///
/// This is the reload window, and it is deliberately generous: a page reload
/// reattaches in under a second, but a laptop that slept mid-build should still
/// find its build when it wakes. (`Instant` not advancing across a macOS sleep
/// only helps here — the session survives the nap either way.) Closing a
/// terminal does not wait for this: that path deletes the session outright.
///
/// It is also the bound on the one leak the model can't avoid. Closing the
/// browser window drops the `sessionStorage` that held the session ids, so
/// those shells can never be reattached — but they *are* detached, so this is
/// what collects them. Quoted as "30 minutes" in `README.md` and
/// `website/llms-full.txt`; change it in all three places.
const DETACH_GRACE: Duration = Duration::from_secs(30 * 60);

/// How often the reaper looks for sessions past [`DETACH_GRACE`].
const REAP_INTERVAL: Duration = Duration::from_secs(60);

/// Replayed to the client on attach, so a reload comes back to the screen it
/// left rather than a blank one. Raw PTY bytes including escape sequences —
/// writing them back into xterm reconstructs the display.
const SCROLLBACK_BYTES: usize = 256 * 1024;

/// Output chunks buffered for the attached socket. Past this the socket is too
/// far behind to catch up and the reader is told it lost bytes, rather than
/// stalling the shell for a client that cannot keep pace.
const OUTPUT_CHANNEL: usize = 512;

/// Grace between the shell exiting and the socket closing, so the last of its
/// output is forwarded instead of truncated.
const EXIT_DRAIN: Duration = Duration::from_millis(250);

/// Grace between hanging up the terminal's process group and killing it.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// How long daemon shutdown waits for each session's pump to deliver its
/// hangup. Short: this is on the path of every `veld update`.
const SHUTDOWN_HANGUP_GRACE: Duration = Duration::from_millis(500);

/// Upper bound on a resize request, so a hostile or buggy client cannot ask
/// the kernel for an absurd winsize. Comfortably past any real display.
const MAX_DIMENSION: u16 = 1000;

/// Read buffer for PTY output. One page: big enough that a `cat` of a large
/// file doesn't syscall per line, small enough to stay responsive.
const READ_BUF: usize = 4096;

/// Cap on a single WebSocket message. Comfortably past any realistic paste, and
/// far below the 64 MiB the transport would otherwise buffer per frame.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The vite dev server that serves `/ide` during `just dev-ui`. Only trusted
/// on a dev instance (see [`allowed_origins`]).
const VITE_DEV_PORT: u16 = 5199;

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Build an axum [`Router`] for the terminal API.
///
/// Unlike `desktop::routes`, CSRF is **not** applied as a blanket layer here:
/// `/api/pty/attach` is a GET (a WebSocket upgrade is a GET) that a layer
/// keyed on the method would wave through anyway, and it carries its own,
/// stronger gate. The other handlers call [`check_csrf`] explicitly. A new
/// route added to this router gets neither gate for free — give it one.
pub fn routes() -> Router {
    Router::new()
        .route("/api/pty/tickets", post(mint_ticket))
        .route("/api/pty/attach", get(attach))
        .route("/api/pty/sessions/{id}", delete(close_session))
}

/// Start the background task that collects sessions nobody came back for.
///
/// Separate from [`routes`] so that building a router in a test doesn't leave a
/// timer running; the daemon calls this once at startup.
pub fn spawn_session_reaper() {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(REAP_INTERVAL).await;
            reap_detached(DETACH_GRACE).await;
        }
    });
}

type ApiError = (StatusCode, Json<serde_json::Value>);

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    (code, Json(serde_json::json!({ "error": msg.into() })))
}

// ---------------------------------------------------------------------------
// Scrollback
// ---------------------------------------------------------------------------

/// Bounded ring of recent PTY output, replayed on attach.
struct Scrollback {
    buf: VecDeque<u8>,
}

impl Scrollback {
    fn new() -> Self {
        Self {
            buf: VecDeque::new(),
        }
    }

    fn push(&mut self, data: &[u8]) {
        self.buf.extend(data.iter().copied());
        if self.buf.len() <= SCROLLBACK_BYTES {
            return;
        }
        let excess = self.buf.len() - SCROLLBACK_BYTES;
        self.buf.drain(..excess);
        // Resume at a line boundary. Replaying from the middle of an escape
        // sequence writes its tail into the terminal as literal text, which is
        // how a restored screen ends up with `[?2004h` sprayed across it. This
        // does not make the replay perfect — a sequence can span lines — but
        // it removes the common case, and the bound keeps a buffer with no
        // newline at all (`yes | tr -d '\n'`) from discarding everything.
        let scan = self.buf.len().min(8 * 1024);
        if let Some(nl) = self.buf.iter().take(scan).position(|&b| b == b'\n') {
            self.buf.drain(..=nl);
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// A running shell, independent of whether anything is looking at it.
struct Session {
    id: String,
    /// Which worktree this belongs to. Checked on reattach so a session cannot
    /// be adopted from another worktree's pane.
    worktree_id: i64,
    label: String,
    /// Resize handle. A `std` mutex because `resize` is a non-blocking ioctl.
    master: Mutex<Box<dyn MasterPty + Send>>,
    write_fd: AsyncFd<File>,
    /// Live output to the attached socket, if any.
    output: broadcast::Sender<Bytes>,
    scrollback: Mutex<Scrollback>,
    /// `Some(code)` once the shell has exited.
    ///
    /// Every write to this and to `attach_epoch` uses `send_replace`, never
    /// `send`: `watch::Sender::send` returns early **without storing the
    /// value** when no receiver exists, and a session with no socket attached
    /// is exactly the normal case here. Using `send` silently discarded the
    /// exit code of any shell that finished while detached, so a reattach saw
    /// a live prompt for a dead shell and the reaper applied the wrong grace.
    exit: watch::Sender<Option<u32>>,
    /// Allocates attach epochs. Separate from the watch below because
    /// read-then-write on the watch value is not atomic, and two attaches
    /// racing must not be handed the same epoch.
    attach_seq: std::sync::atomic::AtomicU64,
    /// The epoch of the socket that currently owns the session. A socket whose
    /// own epoch stops matching has been displaced and closes itself.
    attach_epoch: watch::Sender<u64>,
    /// When the last socket went away, or `None` while one is attached.
    /// Drives [`DETACH_GRACE`].
    detached_since: Mutex<Option<Instant>>,
    /// Set when the session is being ended deliberately.
    ///
    /// The hangup and the SIGKILL escalation then happen inside `pump_output`,
    /// which is the only place still holding the **unreaped** child. Escalating
    /// from [`end_session`] instead would race `child.wait()`: the moment the
    /// child is reaped its pid is free for reuse, and a `killpg` fired after
    /// that could signal an unrelated process group.
    closing: watch::Sender<bool>,
    pid: i32,
    /// Held for the session's lifetime so the [`MAX_SESSIONS`] budget is
    /// released exactly when the session is dropped.
    _slot: SessionSlot,
}

impl Session {
    fn exited(&self) -> Option<u32> {
        *self.exit.borrow()
    }
}

static SESSIONS: LazyLock<tokio::sync::Mutex<HashMap<String, Arc<Session>>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));

static LIVE_SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// Reserved slot in the [`MAX_SESSIONS`] budget, released on drop.
struct SessionSlot {
    /// The counter to credit on drop. A field rather than a hardcoded
    /// reference to [`LIVE_SESSIONS`] so the cap can be exercised against a
    /// counter of the test's own; asserting on the global one would race
    /// every other test that opens a session.
    counter: &'static AtomicUsize,
}

impl SessionSlot {
    fn claim() -> Option<Self> {
        Self::claim_from(&LIVE_SESSIONS, MAX_SESSIONS)
    }

    /// Compare-and-swap rather than load-then-add: two simultaneous attaches
    /// must not both see `max - 1` and both proceed.
    fn claim_from(counter: &'static AtomicUsize, max: usize) -> Option<Self> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| SessionSlot { counter })
    }
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Session ids are client-chosen, so they are checked before being used as a
/// map key or printed into a log line.
fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// End a session: take it out of the registry and tell its pump to hang the
/// shell up.
///
/// The signalling deliberately happens in `pump_output` rather than here — see
/// [`Session::closing`]. This function must not signal the pid itself.
async fn end_session(id: &str, reason: &str) -> bool {
    let session = SESSIONS.lock().await.remove(id);
    let Some(session) = session else {
        return false;
    };
    info!(session = %session.id, worktree = %session.label, reason, "terminal session ended");
    // `send_replace`, not `send`: the pump is the only receiver and may already
    // have finished (a shell that exited on its own), in which case `send`
    // would drop the flag and there is nothing left to hang up anyway.
    session.closing.send_replace(true);
    true
}

/// Collect sessions that have had nobody attached for `grace`.
///
/// An exited shell gets the same grace as a live one on purpose. Its scrollback,
/// its exit notice and its exit code are exactly the post-mortem the detach
/// model exists to preserve — "the socket dropped while I was away and then the
/// build died" is the case worth answering, and a short grace would collect the
/// answer before anyone could read it.
///
/// The policy lives in [`is_reapable`] so it can be tested without waiting
/// [`DETACH_GRACE`] and without touching the process-global registry.
async fn reap_detached(grace: Duration) {
    let now = Instant::now();
    let stale: Vec<String> = {
        let sessions = SESSIONS.lock().await;
        sessions
            .values()
            .filter(|s| {
                let detached = *s.detached_since.lock().expect("detach clock poisoned");
                is_reapable(detached, now, grace)
            })
            .map(|s| s.id.clone())
            .collect()
    };
    for id in stale {
        end_session(&id, "detached past its grace period").await;
    }
}

/// Whether a session is past its grace and may be collected.
///
/// `None` means a socket is attached, which is never reapable however long it
/// has been open — somebody is looking at it.
fn is_reapable(detached_since: Option<Instant>, now: Instant, grace: Duration) -> bool {
    detached_since.is_some_and(|since| {
        // `checked_duration_since`: a stamp that appears to be in the future
        // (read across cores) must not panic here.
        now.checked_duration_since(since).unwrap_or_default() > grace
    })
}

/// Hang up every live session. Called on daemon shutdown: the shells are our
/// children but live in their own sessions, so without this a restart (`veld
/// update` hard-restarts the daemon) leaves them orphaned — and any grandchild
/// that escaped the terminal's process group outlives even the kernel's
/// hangup-on-master-close.
pub async fn shutdown_sessions() {
    let ids: Vec<String> = SESSIONS.lock().await.keys().cloned().collect();
    if ids.is_empty() {
        return;
    }
    let count = ids.len();
    for id in ids {
        end_session(&id, "daemon shutting down").await;
    }
    // `end_session` only raises the `closing` flag; the SIGHUP itself happens in
    // each session's `pump_output`, which is a separate task. Without yielding
    // to them the process would exit first and the flag would never be acted on,
    // so the shells we are trying not to orphan would be orphaned anyway.
    info!(count, "hanging up terminal sessions");
    tokio::time::sleep(SHUTDOWN_HANGUP_GRACE).await;
}

// ---------------------------------------------------------------------------
// Tickets
// ---------------------------------------------------------------------------

/// A redeemable right to attach one socket to one session.
struct Ticket {
    /// Client-chosen session name: an existing session to take over, or the
    /// name a new one will be registered under.
    session_id: String,
    worktree_id: i64,
    /// Where the shell will be spawned. Resolved from the worktree registry at
    /// mint time and never from client input.
    cwd: PathBuf,
    /// Worktree alias, for log lines only.
    label: String,
    expires_at: Instant,
}

static TICKETS: LazyLock<Mutex<HashMap<String, Ticket>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketRequest {
    worktree_id: i64,
    /// Names the session to attach to. Reusing the id of a live session
    /// reattaches to it (that is how a reload gets its shell back); an unknown
    /// id starts a new one.
    session_id: String,
}

#[derive(Serialize)]
struct TicketResponse {
    ticket: String,
    expires_in_ms: u64,
    /// True when a live session with this id is waiting — the client uses it to
    /// distinguish "your shell is still here" from "starting a new one".
    resumed: bool,
}

/// Mint a single-use ticket for a terminal in a registered worktree.
///
/// This is the CSRF-gated half of the handshake — see the module docs. The
/// worktree id is resolved to a directory here so that the WebSocket, which
/// cannot be CSRF-gated, never accepts a path from the client.
async fn mint_ticket(
    headers: HeaderMap,
    Json(body): Json<TicketRequest>,
) -> Result<Json<TicketResponse>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;

    if !valid_session_id(&body.session_id) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid session id"));
    }

    let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
    let wt = db
        .get_worktree(body.worktree_id)
        .map_err(|e| {
            warn!("pty ticket: database error: {e}");
            err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        })?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;

    // A live session is claimed by the worktree it was started in. Without
    // this check a pane could name another worktree's session and adopt a
    // shell running somewhere the user isn't looking.
    let resumed = match SESSIONS.lock().await.get(&body.session_id) {
        Some(s) if s.worktree_id != body.worktree_id => {
            return Err(err(
                StatusCode::CONFLICT,
                "session belongs to another worktree",
            ));
        }
        Some(_) => true,
        None => false,
    };

    // Capacity is checked *here*, not only at attach time, because this is the
    // last point whose body a browser can read: the WebSocket API exposes
    // neither the status nor the body of a failed handshake, so a 503 on the
    // upgrade reaches the UI as an indistinguishable "connection lost". The
    // claim at attach time remains as the race backstop — this check is
    // advisory by construction, since nothing holds a slot between the two.
    if !resumed && LIVE_SESSIONS.load(Ordering::Acquire) >= MAX_SESSIONS {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "too many terminal sessions ({MAX_SESSIONS}) — close a terminal pane to free one"
            ),
        ));
    }

    let cwd = PathBuf::from(&wt.path);
    // Only a new session needs the directory to exist. A resumed one already has
    // its shell running there, and re-checking would refuse an attach to a live
    // process for no benefit — the shell's cwd is the kernel's business by then,
    // not ours.
    if !resumed && !cwd.is_dir() {
        return Err(err(
            StatusCode::CONFLICT,
            format!("worktree directory is not available: {}", wt.path),
        ));
    }

    let ticket = uuid::Uuid::new_v4().simple().to_string();
    let now = Instant::now();
    {
        let mut store = TICKETS.lock().expect("ticket store poisoned");
        // Opportunistic sweep: a ticket that is minted and never redeemed
        // (the user closes the tab mid-connect) would otherwise sit here for
        // the life of the daemon.
        store.retain(|_, t| t.expires_at > now);
        store.insert(
            ticket.clone(),
            Ticket {
                session_id: body.session_id,
                worktree_id: body.worktree_id,
                cwd,
                label: wt.alias.clone(),
                expires_at: now + TICKET_TTL,
            },
        );
    }

    Ok(Json(TicketResponse {
        ticket,
        expires_in_ms: TICKET_TTL.as_millis() as u64,
        resumed,
    }))
}

/// Consume a ticket. Returns `None` if it is unknown, already used, or
/// expired — all indistinguishable to the caller on purpose.
fn redeem(ticket: &str) -> Option<Ticket> {
    let mut store = TICKETS.lock().expect("ticket store poisoned");
    let t = store.remove(ticket)?;
    if t.expires_at <= Instant::now() {
        return None;
    }
    Some(t)
}

// ---------------------------------------------------------------------------
// Closing
// ---------------------------------------------------------------------------

/// End a session now, because its tab was closed.
///
/// The distinction this endpoint draws is the whole point of the detach model:
/// a socket going away means "come back later", while this means "the user is
/// done". Without it every closed tab would leave a shell running until
/// [`DETACH_GRACE`].
async fn close_session(headers: HeaderMap, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    if !valid_session_id(&id) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid session id"));
    }
    // 204 either way: closing an already-gone session is the client and daemon
    // agreeing, not an error, and the UI has nothing useful to do with a 404.
    end_session(&id, "closed by the client").await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Origin allowlist
// ---------------------------------------------------------------------------

/// Origins allowed to open a terminal socket.
///
/// Built per request rather than cached: it is a handful of `format!`s on a
/// path that already spawns a process, and the daemon's instance identity
/// (port, management host) is read from the environment.
fn allowed_origins() -> Vec<String> {
    let port = veld_core::instance::daemon_port();
    let mut origins = vec![
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
        // The installed instance's Caddy route (veld-helper's base config).
        "https://veld.localhost".to_owned(),
    ];
    // A dev instance registers its own management hostname with the helper.
    if let Some(host) = veld_core::instance::management_host() {
        origins.push(format!("https://{host}"));
    }
    // The vite dev server proxies /api (including this upgrade) to the daemon
    // it was pointed at, so the browser's Origin is vite's, not ours. Trust it
    // only on a dev instance: the installed daemon on the default port must
    // never accept an origin that a locally-running dev server could forge.
    if port != veld_core::instance::DEFAULT_DAEMON_PORT {
        origins.push(format!("http://localhost:{VITE_DEV_PORT}"));
        origins.push(format!("http://127.0.0.1:{VITE_DEV_PORT}"));
    }
    origins
}

/// Whether a request's `Origin` may open a terminal.
///
/// Fails closed on a missing or non-ASCII `Origin`. Browsers always send one
/// on a WebSocket handshake, so the only callers this turns away are
/// non-browser clients — which have a real terminal already.
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    // Exact match. Browsers serialise an origin canonically (lowercase scheme
    // and host, default port omitted), so there is nothing to normalise, and
    // prefix/suffix matching is how origin checks get bypassed.
    allowed_origins().iter().any(|a| a == origin)
}

// ---------------------------------------------------------------------------
// Attach
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AttachQuery {
    ticket: String,
    /// Terminal size. Sent up front so a new shell's first prompt is already
    /// the right width — otherwise it renders at 80x24 and reflows.
    cols: Option<u16>,
    rows: Option<u16>,
}

/// Upgrade to a WebSocket showing one terminal session.
async fn attach(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(q): Query<AttachQuery>,
) -> Response {
    if !origin_allowed(&headers) {
        // Deliberately terse: an attacker learns nothing about the allowlist,
        // and a developer sees the real origin in the daemon log.
        warn!(
            origin = ?headers.get(header::ORIGIN),
            "rejected terminal upgrade from a disallowed origin"
        );
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }

    let Some(ticket) = redeem(&q.ticket) else {
        return (StatusCode::FORBIDDEN, "invalid or expired ticket").into_response();
    };

    let size = PtySize {
        cols: clamp_dimension(q.cols, 80),
        rows: clamp_dimension(q.rows, 24),
        pixel_width: 0,
        pixel_height: 0,
    };

    // Resolve (or create) the session before upgrading, so a failure is a real
    // HTTP status rather than an immediate close of a socket just opened.
    //
    // Note what this does *not* buy: a browser cannot read the status or body of
    // a failed WebSocket handshake, so for the UI these all collapse into an
    // abnormal close. The failures a legitimate client can actually provoke are
    // therefore pre-checked in `mint_ticket`, whose JSON body it does read;
    // reaching one here means a race or a broken environment, and the daemon log
    // is the record. Non-browser clients and the tests below do see the status.
    let (session, resumed) = match obtain_session(&ticket, size).await {
        Ok(s) => s,
        Err(SessionError::AtCapacity) => {
            warn!("refusing terminal: {MAX_SESSIONS} sessions already live");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "too many terminal sessions",
            )
                .into_response();
        }
        Err(SessionError::Spawn(e)) => {
            warn!("failed to open a terminal in {}: {e}", ticket.cwd.display());
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not start a shell: {e}"),
            )
                .into_response();
        }
    };

    // Terminal traffic is keystrokes and screen updates. The default ceiling is
    // tungstenite's 64 MiB, which the daemon would buffer per frame per socket;
    // a paste, even a large one, fits in a fraction of this.
    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            serve_socket(socket, session, size, resumed).await;
        })
}

enum SessionError {
    AtCapacity,
    Spawn(anyhow::Error),
}

/// The live session for a ticket, starting one if the named session is gone.
///
/// Holds the registry lock across the spawn so two attaches racing on the same
/// id cannot both create a shell; spawning is a handful of syscalls, not I/O
/// worth yielding for.
async fn obtain_session(
    ticket: &Ticket,
    size: PtySize,
) -> Result<(Arc<Session>, bool), SessionError> {
    let mut sessions = SESSIONS.lock().await;
    if let Some(existing) = sessions.get(&ticket.session_id) {
        return Ok((existing.clone(), true));
    }

    let slot = SessionSlot::claim().ok_or(SessionError::AtCapacity)?;
    let spawned = spawn_shell(&ticket.cwd, size).map_err(SessionError::Spawn)?;
    let Spawned {
        master,
        child,
        pid,
        read_fd,
        write_fd,
    } = spawned;

    let (output, _) = broadcast::channel(OUTPUT_CHANNEL);
    let (exit, _) = watch::channel(None);
    let (attach_epoch, _) = watch::channel(0u64);
    let (closing, _) = watch::channel(false);
    let session = Arc::new(Session {
        id: ticket.session_id.clone(),
        worktree_id: ticket.worktree_id,
        label: ticket.label.clone(),
        master: Mutex::new(master),
        write_fd,
        output,
        scrollback: Mutex::new(Scrollback::new()),
        exit,
        attach_seq: std::sync::atomic::AtomicU64::new(0),
        attach_epoch,
        // Starts detached: `serve_socket` marks it attached, and if the socket
        // never arrives the reaper must still be able to collect it.
        detached_since: Mutex::new(Some(Instant::now())),
        closing,
        pid,
        _slot: slot,
    });
    sessions.insert(session.id.clone(), session.clone());

    info!(session = %session.id, worktree = %ticket.label, pid, "terminal session started");
    // Draining the PTY is the session's job, not the socket's: output produced
    // while nothing is attached still has to land in the scrollback, or a
    // reload would come back to a screen missing everything that happened
    // while the page was gone.
    tokio::spawn(pump_output(session.clone(), read_fd, child));
    Ok((session, false))
}

/// Drain the PTY for the session's whole life, feeding the scrollback and any
/// attached socket, then record the shell's exit status.
async fn pump_output(
    session: Arc<Session>,
    read_fd: AsyncFd<File>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
) {
    let pid = session.pid;
    // `child.wait()` is blocking; on the pool it both reaps the zombie and
    // gives us the status.
    let mut waiter = tokio::task::spawn_blocking(move || child.wait());

    let mut buf = [0u8; READ_BUF];
    let mut exit_code: Option<u32> = None;
    // Placeholder deadline. The branch below is disabled until the shell
    // exits and the timer is reset at that moment, so this initial duration
    // is never the one that fires.
    let drain = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(drain);
    let mut closing = session.closing.subscribe();
    // Seeded from the current value, not `false`: `subscribe()` marks whatever
    // is already there as seen, so a `DELETE` that lands between the session
    // being registered and this task's first poll would set the flag, never fire
    // `changed()`, and leave the shell running with nothing left to hang it up.
    let mut asked_to_close = *closing.borrow_and_update();
    if asked_to_close {
        hangup(pid);
    }

    loop {
        tokio::select! {
            // Prefer draining output: on a tie with the exit branch, the last
            // bytes the shell wrote should reach the client first.
            biased;

            r = pty_read(&read_fd, &mut buf) => match r {
                // EOF: every descriptor on the slave side is closed.
                Ok(0) => break,
                Ok(n) => {
                    let chunk = Bytes::copy_from_slice(&buf[..n]);
                    // Recorded and broadcast under ONE lock. Released in
                    // between, a chunk could land in the scrollback, be picked
                    // up by an attach snapshotting it, and then also arrive live
                    // on that attach's subscription — rendered twice, possibly
                    // splitting an escape sequence. `serve_socket` takes the
                    // same lock across subscribe+snapshot, so the two are
                    // ordered against each other.
                    let mut sb = session.scrollback.lock().expect("scrollback poisoned");
                    sb.push(&chunk);
                    // Errors here mean nothing is attached, which is normal.
                    let _ = session.output.send(chunk);
                    drop(sb);
                }
                // Linux reports the same hangup as EIO rather than EOF.
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                Err(e) => {
                    warn!("terminal read failed (pid {pid}): {e}");
                    break;
                }
            },

            status = &mut waiter, if exit_code.is_none() => {
                exit_code = Some(match status {
                    Ok(Ok(s)) => s.exit_code(),
                    Ok(Err(e)) => {
                        warn!("waiting on terminal shell (pid {pid}) failed: {e}");
                        1
                    }
                    Err(e) => {
                        warn!("terminal wait task for pid {pid} failed: {e}");
                        1
                    }
                });
                // The shell is gone, but a grandchild it left behind can still
                // hold the slave open, in which case EOF never arrives. Bound
                // the wait for it.
                drain.as_mut().reset(tokio::time::Instant::now() + EXIT_DRAIN);
            },

            _ = &mut drain, if exit_code.is_some() => break,

            // `end_session` asked for this shell to go away. Hanging up here
            // rather than there is what keeps the escalation safe: this task
            // owns the unreaped child, so the pid cannot have been recycled
            // under us. See Session::closing.
            Ok(()) = closing.changed(), if !asked_to_close => {
                if *closing.borrow_and_update() {
                    asked_to_close = true;
                    hangup(pid);
                }
            },
        }
    }

    let code = match exit_code {
        Some(c) => c,
        None => {
            // The read loop ended without the shell exiting: it was closed out
            // from under us, or a descriptor error broke the loop. Hang up (idem-
            // potent if `closing` already did) and escalate if it holds out.
            hangup(pid);
            match tokio::time::timeout(KILL_GRACE, &mut waiter).await {
                Ok(Ok(Ok(s))) => s.exit_code(),
                Ok(_) => 1,
                Err(_) => {
                    kill(pid);
                    let _ = waiter.await;
                    1
                }
            }
        }
    };

    // The notice goes out before the exit code, and to both the scrollback and
    // the live stream: the socket drains pending output when it sees the code,
    // so publishing the code first would race the notice past it, and a
    // scrollback-only notice would be invisible to the client watching now.
    let notice = format!("\r\n\x1b[2m[veld] shell exited ({code})\x1b[0m\r\n");
    let notice = Bytes::from(notice.into_bytes());
    session
        .scrollback
        .lock()
        .expect("scrollback poisoned")
        .push(&notice);
    let _ = session.output.send(notice);

    // Publishing the code is what lets an attached socket report the exit and
    // a later attach show it instead of a live prompt. `send_replace`, not
    // `send`: a shell that exits while nothing is attached has no receivers,
    // and `send` would drop the code on the floor.
    session.exit.send_replace(Some(code));
    debug!(session = %session.id, pid, code, "terminal shell exited");
}

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

/// Client → server control frames. Keystrokes travel as **binary** frames;
/// text frames are reserved for control, so a resize can never be mistaken
/// for input (and input can never be mangled by UTF-8 validation splitting a
/// multi-byte sequence across frames).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientControl {
    Resize { cols: u16, rows: u16 },
}

/// Server → client control frames. PTY output travels as binary frames.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerControl {
    /// Replayed scrollback follows, until [`ServerControl::ReplayEnd`].
    ///
    /// The client must ignore anything its terminal emulator tries to *send*
    /// while replaying. Recorded output can contain queries the shell made
    /// (device attributes, cursor position, colour), and an emulator parsing
    /// them again answers them again — that answer arrives at a shell which
    /// asked nothing, so it lands as stray keystrokes. The visible symptom was
    /// a `1;2c` fragment (the tail of a `CSI ? 1 ; 2 c` attributes reply)
    /// appearing at the prompt after every reload. Bracketing the replay lets
    /// the client gate its input for exactly that window; stripping known
    /// query sequences here instead would mean maintaining a list of them.
    ReplayBegin,
    /// End of the replayed scrollback. Always sent if `ReplayBegin` was, so a
    /// client gating on it cannot get stuck.
    ReplayEnd,
    /// The shell is attached and its output is flowing. `resumed` is true when
    /// this attach adopted an already-running shell.
    Ready { resumed: bool },
    /// The shell exited; `code` is its status (128 + signal when killed).
    Exit { code: u32 },
    /// This socket was displaced by a newer attach to the same session.
    TakenOver,
    /// Output was produced faster than this socket could take it, so the
    /// display is missing bytes.
    Lagged,
}

impl ServerControl {
    fn frame(&self) -> Message {
        // Serialising a fixed-shape enum cannot fail; the fallback keeps the
        // client from hanging on a silence it has no timeout for.
        Message::Text(
            serde_json::to_string(self)
                .unwrap_or_else(|_| r#"{"type":"lagged"}"#.to_owned())
                .into(),
        )
    }
}

// ---------------------------------------------------------------------------
// Socket
// ---------------------------------------------------------------------------

/// Show one session on one socket, until the socket closes, the shell exits,
/// or a newer attach takes over.
/// `resumed` says whether this attach adopted an already-running session. It
/// comes from the registry, not from "is the scrollback non-empty" — a brand
/// new shell can print its prompt before this function snapshots, so the
/// buffer is no evidence either way.
async fn serve_socket(socket: WebSocket, session: Arc<Session>, size: PtySize, resumed: bool) {
    let (mut ws_tx, ws_rx) = socket.split();

    // Subscribe and snapshot under the SAME scrollback lock. Subscribing after
    // the snapshot would lose whatever arrived in between; subscribing before it
    // without the lock would deliver such a chunk twice (once replayed, once
    // live). Holding the lock across both makes the cut exact — `pump_output`
    // records and broadcasts under that same lock.
    let (mut output, replay) = {
        let sb = session.scrollback.lock().expect("scrollback poisoned");
        (session.output.subscribe(), sb.snapshot())
    };

    // Claim the session: any socket already attached sees this and leaves.
    // Subscribe first, so a takeover that lands between the claim and the
    // subscription is still observed.
    let mut epoch_rx = session.attach_epoch.subscribe();
    let epoch = session.attach_seq.fetch_add(1, Ordering::AcqRel) + 1;
    session.attach_epoch.send_replace(epoch);
    *session
        .detached_since
        .lock()
        .expect("detach clock poisoned") = None;

    // A reattaching client's terminal is whatever size it is now, which is not
    // necessarily the size the shell last knew.
    resize_session(&session, size.cols, size.rows);

    if !replay.is_empty() {
        // Bracketed so the client can gate its terminal's replies — see
        // ServerControl::ReplayBegin.
        let framed = [
            ServerControl::ReplayBegin.frame(),
            Message::Binary(replay.into()),
            ServerControl::ReplayEnd.frame(),
        ];
        for frame in framed {
            if ws_tx.send(frame).await.is_err() {
                mark_detached(&session, epoch);
                return;
            }
        }
    }
    if ws_tx
        .send(ServerControl::Ready { resumed }.frame())
        .await
        .is_err()
    {
        mark_detached(&session, epoch);
        return;
    }

    // Input runs in its own task: if it shared this one, a shell that stops
    // reading (`yes` filling the input buffer while flooding output) would
    // block the loop that is supposed to be draining that output — a deadlock
    // between the two directions. It carries its own epoch so it enforces the
    // one-writer rule itself rather than relying on this loop to abort it.
    let mut input = tokio::spawn(pump_input(ws_rx, session.clone(), epoch));

    let mut exit_rx = session.exit.subscribe();
    // An exit that happened before this attach is reported immediately, so a
    // reload onto a finished shell shows the exit instead of a live prompt.
    if let Some(code) = session.exited() {
        let _ = ws_tx.send(ServerControl::Exit { code }.frame()).await;
        let _ = ws_tx.close().await;
        input.abort();
        mark_detached(&session, epoch);
        return;
    }

    loop {
        // Deliberately NOT `biased`: with output first in a biased select, a
        // shell producing continuously (a build, a `yes`) keeps that branch
        // ready forever and the exit, takeover and socket-closed branches are
        // never polled. Random polling costs at most a reordered final chunk —
        // the exit branch drains what is pending before reporting.
        tokio::select! {
            chunk = output.recv() => match chunk {
                Ok(bytes) => {
                    if ws_tx.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(session = %session.id, dropped = n, "terminal client fell behind");
                    if ws_tx.send(ServerControl::Lagged.frame()).await.is_err() {
                        break;
                    }
                }
                // Unreachable in practice: `Session` owns the sender and this
                // task holds an `Arc<Session>` for the whole loop, so the
                // channel cannot close under us. Handled rather than
                // `unreachable!()` so a future change that does drop the sender
                // degrades into reporting the exit instead of panicking — do not
                // rely on this as the exit-detection path; that is `exit_rx`.
                Err(broadcast::error::RecvError::Closed) => {
                    if let Some(code) = session.exited() {
                        let _ = ws_tx.send(ServerControl::Exit { code }.frame()).await;
                    }
                    break;
                }
            },

            Ok(()) = exit_rx.changed() => {
                // Copied out of the guard on its own line: holding a
                // `watch::Ref` across the awaits below makes this future
                // non-Send, which axum's upgrade handler requires.
                let exited = *exit_rx.borrow_and_update();
                if let Some(code) = exited {
                    // Let the pump's last chunks through before the notice.
                    tokio::time::sleep(EXIT_DRAIN).await;
                    while let Ok(bytes) = output.try_recv() {
                        if ws_tx.send(Message::Binary(bytes)).await.is_err() {
                            break;
                        }
                    }
                    let _ = ws_tx.send(ServerControl::Exit { code }.frame()).await;
                    break;
                }
            },

            Ok(()) = epoch_rx.changed() => {
                let current = *epoch_rx.borrow_and_update();
                if current != epoch {
                    let _ = ws_tx.send(ServerControl::TakenOver.frame()).await;
                    break;
                }
            },

            // The socket's read half ended: the tab closed, or the connection
            // dropped. The session stays alive for a reattach.
            _ = &mut input => break,
        }
    }

    input.abort();
    let _ = ws_tx.close().await;
    mark_detached(&session, epoch);
}

/// Start the detach clock, unless a newer socket has already claimed the
/// session — in which case that socket owns the clock and this one must not
/// reset it.
fn mark_detached(session: &Session, epoch: u64) {
    if *session.attach_epoch.borrow() != epoch {
        return;
    }
    *session
        .detached_since
        .lock()
        .expect("detach clock poisoned") = Some(Instant::now());
    debug!(session = %session.id, "terminal socket detached");
}

fn resize_session(session: &Session, cols: u16, rows: u16) {
    let size = PtySize {
        cols: clamp_dimension(Some(cols), 80),
        rows: clamp_dimension(Some(rows), 24),
        pixel_width: 0,
        pixel_height: 0,
    };
    if let Err(e) = session
        .master
        .lock()
        .expect("pty master poisoned")
        .resize(size)
    {
        debug!("terminal resize failed: {e}");
    }
}

/// Forward client frames to the PTY until the socket ends.
///
/// `epoch` is this socket's claim on the session. It is re-checked before every
/// write rather than trusting `serve_socket` to abort this task: that loop can
/// be busy forwarding a flood of output when a takeover lands, and until it
/// notices, a displaced socket would still be writing into the shell — two
/// writers on one input stream, which the module's contract rules out.
async fn pump_input(
    mut ws_rx: futures_util::stream::SplitStream<WebSocket>,
    session: Arc<Session>,
    epoch: u64,
) {
    while let Some(Ok(msg)) = ws_rx.next().await {
        if *session.attach_epoch.borrow() != epoch {
            return;
        }
        match msg {
            Message::Binary(data) => {
                if pty_write(&session.write_fd, &data).await.is_err() {
                    return;
                }
            }
            Message::Text(text) => match serde_json::from_str::<ClientControl>(&text) {
                Ok(ClientControl::Resize { cols, rows }) => resize_session(&session, cols, rows),
                Err(e) => debug!("ignoring unparseable terminal control frame: {e}"),
            },
            Message::Close(_) => return,
            // axum answers pings itself; nothing to do.
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
}

/// Clamp a client-supplied dimension into a sane range, falling back to
/// `default` when absent or zero. A zero here reaches `TIOCSWINSZ` and leaves
/// curses programs dividing by it.
fn clamp_dimension(v: Option<u16>, default: u16) -> u16 {
    match v {
        Some(n) if n > 0 => n.min(MAX_DIMENSION),
        _ => default,
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

struct Spawned {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    pid: i32,
    read_fd: AsyncFd<File>,
    write_fd: AsyncFd<File>,
}

/// Open a PTY and start the user's login shell in `cwd`.
fn spawn_shell(cwd: &std::path::Path, size: PtySize) -> anyhow::Result<Spawned> {
    let PtyPair { master, slave } = native_pty_system().openpty(size)?;

    let shell = login_shell();
    let mut cmd = CommandBuilder::new(&shell);
    // A *login* shell, which is also what makes this an exception to the
    // AGENTS.md "resolve the user's PATH with `resolve_user_path()`" rule.
    // That helper exists because a daemon running `sh -c '<config command>'`
    // inherits launchd's bare PATH; it gets the real one by spawning
    // `$SHELL -l -i -c 'command env'` and scraping it. Here the thing being
    // spawned *is* that login shell, so it computes the same PATH itself —
    // calling the helper first would spawn a second shell and add its startup
    // cost (up to its 10s timeout on a wedged rc file) to every terminal, to
    // arrive at the value this shell is about to compute anyway.
    cmd.arg("-l");
    cmd.cwd(cwd);
    // xterm.js speaks xterm-256color; without TERM the shell assumes "dumb"
    // and disables colour and line editing.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let child = slave.spawn_command(cmd)?;
    // Close our copy of the slave. While the daemon holds it, the master
    // never reaches EOF after the shell exits, and the session would hang
    // until its drain timer instead of ending cleanly.
    drop(slave);

    let pid = child.process_id().unwrap_or(0) as i32;
    let raw = master
        .as_raw_fd()
        .ok_or_else(|| anyhow::anyhow!("pty master has no file descriptor"))?;

    // Two independent descriptors so the read and write loops cannot block
    // each other. They share one open file description, so O_NONBLOCK set on
    // either applies to both — which is what we want, and why the writer is
    // also driven through AsyncFd rather than blocking on the shared flag.
    let read_fd = async_dup(raw)?;
    let write_fd = async_dup(raw)?;

    Ok(Spawned {
        master,
        child,
        pid,
        read_fd,
        write_fd,
    })
}

/// The user's shell, falling back to a POSIX shell that is present on every
/// supported platform. `SHELL` comes from the daemon's own environment
/// (launchd/systemd propagate the user's), never from the client.
fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

/// Duplicate a descriptor, mark it non-blocking, and hand it to tokio's
/// readiness machinery.
///
/// A duplicate rather than the master's own descriptor because `AsyncFd`
/// wants ownership, while `master` must stay alive to serve resizes.
fn async_dup(raw: RawFd) -> anyhow::Result<AsyncFd<File>> {
    // SAFETY: `raw` is owned by the live `MasterPty` for the duration of this
    // call; F_DUPFD_CLOEXEC returns a new descriptor we own outright, and
    // close-on-exec keeps it out of any process spawned from another thread
    // between here and the `OwnedFd`.
    let dup = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `dup` is a fresh descriptor with no other owner.
    let owned = unsafe { OwnedFd::from_raw_fd(dup) };

    // SAFETY: `owned` keeps the descriptor alive across both calls.
    let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    Ok(AsyncFd::new(File::from(owned))?)
}

/// Read PTY output, waiting for readiness. `Ok(0)` means hangup.
async fn pty_read(fd: &AsyncFd<File>, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        let mut guard = fd.readable().await?;
        match guard.try_io(|inner| {
            let mut file = inner.get_ref();
            file.read(buf)
        }) {
            Ok(result) => return result,
            // Readiness was stale; wait for it again.
            Err(_would_block) => continue,
        }
    }
}

/// Write all of `data` to the PTY, waiting for writability between partial
/// writes.
async fn pty_write(fd: &AsyncFd<File>, data: &[u8]) -> std::io::Result<()> {
    let mut rest = data;
    while !rest.is_empty() {
        let mut guard = fd.writable().await?;
        match guard.try_io(|inner| {
            let mut file = inner.get_ref();
            file.write(rest)
        }) {
            // A pty master accepts zero bytes only if it is gone; looping on
            // it would spin forever.
            Ok(Ok(0)) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(Ok(n)) => rest = &rest[n..],
            Ok(Err(e)) => return Err(e),
            Err(_would_block) => continue,
        }
    }
    Ok(())
}

/// Hang up the terminal's process group, the way closing a real terminal
/// does.
///
/// `portable-pty` puts the shell in its own session (`setsid`) with the PTY as
/// its controlling terminal, so its process-group id equals its pid and
/// `killpg` reaches the shell together with whatever job is in the foreground.
/// A shell that honours SIGHUP hangs up its background jobs on the way out —
/// which is why this is preferable to signalling the shell alone
/// (`ChildKiller::kill`, which sends SIGHUP to the pid only).
fn hangup(pid: i32) {
    signal_group(pid, libc::SIGHUP);
}

/// Kill the terminal's process group outright.
fn kill(pid: i32) {
    signal_group(pid, libc::SIGKILL);
}

fn signal_group(pid: i32, sig: i32) {
    // A non-positive pid would be catastrophic here: `killpg(0, …)` signals
    // *the daemon's own* process group. `process_id()` returning None lands
    // on 0, so this guard is load-bearing, not defensive dressing.
    if pid <= 0 {
        warn!("refusing to signal terminal process group {pid}");
        return;
    }
    // SAFETY: `killpg` is async-signal-safe and takes no pointers; a pid that
    // has already been reaped simply yields ESRCH, which we ignore.
    if unsafe { libc::killpg(pid, sig) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            debug!("killpg({pid}, {sig}) failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_dimensions() {
        assert_eq!(clamp_dimension(None, 80), 80);
        // Zero would reach TIOCSWINSZ and make curses programs divide by it.
        assert_eq!(clamp_dimension(Some(0), 24), 24);
        assert_eq!(clamp_dimension(Some(120), 80), 120);
        assert_eq!(clamp_dimension(Some(u16::MAX), 80), MAX_DIMENSION);
    }

    #[test]
    fn session_ids_are_validated_before_use_as_keys() {
        assert!(valid_session_id("0191f0c4-9c1a-7b1e-8f00-1c2d3e4f5a6b"));
        assert!(valid_session_id("term_1"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id(&"x".repeat(65)));
        // Path-ish and whitespace-ish shapes stay out of log lines and keys.
        assert!(!valid_session_id("../etc"));
        assert!(!valid_session_id("a b"));
        assert!(!valid_session_id("a\nb"));
    }

    #[test]
    fn origin_must_be_present_and_exact() {
        let allowed = allowed_origins();
        let installed = "https://veld.localhost";
        assert!(allowed.iter().any(|o| o == installed));

        let origin_of = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::ORIGIN, v.parse().unwrap());
            h
        };

        assert!(origin_allowed(&origin_of(installed)));
        // Absent Origin fails closed — the whole point of the gate.
        assert!(!origin_allowed(&HeaderMap::new()));
        // Substring and suffix tricks must not pass an exact-match check.
        assert!(!origin_allowed(&origin_of(
            "https://veld.localhost.evil.com"
        )));
        assert!(!origin_allowed(&origin_of("https://evil.veld.localhost")));
        assert!(!origin_allowed(&origin_of("http://veld.localhost")));
        assert!(!origin_allowed(&origin_of("null")));
    }

    #[test]
    fn installed_instance_does_not_trust_the_dev_server() {
        // The default-port daemon is the installed one; a dev server running
        // on the same machine must not be able to open a shell through it.
        // (This reads the environment, so it asserts the default case only
        // when the test process has no VELD_DAEMON_PORT override.)
        if veld_core::instance::daemon_port() == veld_core::instance::DEFAULT_DAEMON_PORT {
            let vite = format!("http://localhost:{VITE_DEV_PORT}");
            assert!(!allowed_origins().contains(&vite));
        }
    }

    fn planted(id: &str, ttl: Duration) -> String {
        let key = uuid::Uuid::new_v4().simple().to_string();
        TICKETS.lock().unwrap().insert(
            key.clone(),
            Ticket {
                session_id: id.to_owned(),
                worktree_id: 1,
                cwd: std::env::temp_dir(),
                label: "t".to_owned(),
                expires_at: Instant::now() + ttl,
            },
        );
        key
    }

    #[test]
    fn tickets_are_single_use_and_expiring() {
        let good = planted("s1", TICKET_TTL);
        assert!(redeem(&good).is_some());
        // Replaying the same ticket must not open a second shell.
        assert!(redeem(&good).is_none());

        // `planted` with a negative TTL isn't expressible, so plant directly.
        let stale = uuid::Uuid::new_v4().simple().to_string();
        TICKETS.lock().unwrap().insert(
            stale.clone(),
            Ticket {
                session_id: "s2".to_owned(),
                worktree_id: 1,
                cwd: std::env::temp_dir(),
                label: "t".to_owned(),
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        assert!(redeem(&stale).is_none());
    }

    #[test]
    fn scrollback_is_bounded_and_resumes_on_a_line_boundary() {
        let mut sb = Scrollback::new();
        for i in 0..20_000u32 {
            sb.push(format!("line {i}\n").as_bytes());
        }
        let snap = sb.snapshot();
        assert!(snap.len() <= SCROLLBACK_BYTES, "ring must stay bounded");
        // Trimming mid-line would replay a partial escape sequence as text.
        assert!(
            snap.starts_with(b"line "),
            "replay should start at a line boundary, got {:?}",
            String::from_utf8_lossy(&snap[..20.min(snap.len())])
        );
        // The newest output is what a reattaching client most needs.
        assert!(String::from_utf8_lossy(&snap).ends_with("line 19999\n"));
    }

    #[test]
    fn scrollback_without_newlines_still_keeps_the_tail() {
        // `yes | tr -d '\n'` produces no line boundary to trim to; dropping
        // everything would leave a reattach with a blank screen.
        let mut sb = Scrollback::new();
        for _ in 0..(SCROLLBACK_BYTES / 16 + 100) {
            sb.push(&[b'x'; 16]);
        }
        let snap = sb.snapshot();
        assert!(!snap.is_empty(), "tail must survive");
        assert!(snap.len() <= SCROLLBACK_BYTES);
    }

    #[test]
    fn only_detached_sessions_past_their_grace_are_reapable() {
        let now = Instant::now();
        let grace = Duration::from_secs(60);

        // Attached: never reapable, however long it has been open. Somebody is
        // looking at it — this is the guard that keeps the reaper from killing
        // a terminal a user is typing into.
        assert!(!is_reapable(None, now, grace));

        // Detached, still inside the reload/sleep window.
        assert!(!is_reapable(Some(now), now, grace));
        assert!(!is_reapable(
            Some(now - Duration::from_secs(59)),
            now,
            grace
        ));
        // Exactly at the grace is not yet past it.
        assert!(!is_reapable(Some(now - grace), now, grace));

        assert!(is_reapable(Some(now - Duration::from_secs(61)), now, grace));

        // A stamp that reads as being in the future must not panic (and must not
        // be treated as infinitely old).
        assert!(!is_reapable(Some(now + Duration::from_secs(5)), now, grace));

        // A zero grace still exempts an attached session.
        assert!(!is_reapable(None, now, Duration::ZERO));
    }

    #[test]
    fn session_slots_are_bounded_and_returned() {
        // A counter of our own: the live one is shared with every e2e test
        // that opens a session, so asserting exact values on it would race.
        static SLOTS: AtomicUsize = AtomicUsize::new(0);
        const MAX: usize = 3;

        let mut held = Vec::new();
        while let Some(slot) = SessionSlot::claim_from(&SLOTS, MAX) {
            held.push(slot);
            assert!(held.len() <= MAX, "claim_from() exceeded the cap");
        }
        assert_eq!(held.len(), MAX);

        // Releasing one must free exactly one, not open the gate.
        drop(held.pop());
        let regained = SessionSlot::claim_from(&SLOTS, MAX);
        assert!(regained.is_some());
        assert!(SessionSlot::claim_from(&SLOTS, MAX).is_none());

        drop(regained);
        drop(held);
        // Dropping must return every slot, or the daemon refuses terminals
        // forever after a burst.
        assert_eq!(SLOTS.load(Ordering::Acquire), 0);
    }

    /// Ticket minting and closing reject before they touch the database, so
    /// these run against the real router with no test DB.
    #[tokio::test]
    async fn mutations_require_the_csrf_header() {
        use axum::body::Body;
        use tower::ServiceExt;

        for (method, uri, body) in [
            (
                "POST",
                "/api/pty/tickets",
                r#"{"worktree_id":1,"session_id":"a"}"#,
            ),
            ("DELETE", "/api/pty/sessions/a", ""),
        ] {
            let res = routes()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_owned()))
                        .unwrap(),
                )
                .await
                .unwrap();
            // Without this, a cross-origin page could mint the ticket the
            // WebSocket gate is built on, or kill someone's shell.
            assert_eq!(
                res.status(),
                StatusCode::FORBIDDEN,
                "{method} {uri} must require the CSRF header"
            );
        }
    }

    /// End-to-end over a real socket: a real listener, a real handshake, a
    /// real shell. The unit tests above pin the guards in isolation; these
    /// pin that the guards are actually *reached*, and that the bridge moves
    /// bytes in both directions and survives a reconnect.
    ///
    /// Tickets are planted directly in the store so none of this needs the
    /// worktree database (`mint_ticket`'s own path is covered above).
    mod e2e {
        use super::*;
        use std::net::SocketAddr;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::{Message as WsMessage, http};

        /// Bound on any single read in these tests. Generous: CI boxes are
        /// slow and a login shell can source a lot of rc files.
        const STEP_TIMEOUT: Duration = Duration::from_secs(30);

        type Client = tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >;

        async fn serve() -> SocketAddr {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, routes()).await.unwrap();
            });
            addr
        }

        /// A session id unique to one test, so tests can run in parallel
        /// against the process-global registry.
        fn session_id() -> String {
            uuid::Uuid::new_v4().simple().to_string()
        }

        fn plant_ticket(session: &str, cwd: &std::path::Path) -> String {
            let key = uuid::Uuid::new_v4().simple().to_string();
            TICKETS.lock().unwrap().insert(
                key.clone(),
                Ticket {
                    session_id: session.to_owned(),
                    worktree_id: 1,
                    cwd: cwd.to_path_buf(),
                    label: "test".to_owned(),
                    expires_at: Instant::now() + TICKET_TTL,
                },
            );
            key
        }

        /// The origin the daemon under test actually trusts. Derived from the
        /// allowlist rather than hardcoded, so this can't drift away from it.
        fn good_origin() -> String {
            allowed_origins()
                .into_iter()
                .find(|o| o.starts_with("http://127.0.0.1:"))
                .expect("loopback origin is always allowed")
        }

        fn attach_request(
            addr: SocketAddr,
            query: &str,
            origin: Option<&str>,
        ) -> http::Request<()> {
            let mut req = format!("ws://{addr}/api/pty/attach?{query}")
                .into_client_request()
                .unwrap();
            if let Some(o) = origin {
                req.headers_mut()
                    .insert(http::header::ORIGIN, o.parse().unwrap());
            }
            req
        }

        /// Status code from a handshake that is expected to fail.
        async fn handshake_status(req: http::Request<()>) -> http::StatusCode {
            match tokio_tungstenite::connect_async(req).await {
                Ok(_) => panic!("handshake unexpectedly succeeded"),
                Err(tokio_tungstenite::tungstenite::Error::Http(res)) => res.status(),
                Err(e) => panic!("unexpected handshake error: {e}"),
            }
        }

        #[tokio::test]
        async fn upgrade_requires_an_allowed_origin() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();

            // A page on the open internet, which is the whole threat model.
            let t = plant_ticket(&session_id(), dir.path());
            assert_eq!(
                handshake_status(attach_request(
                    addr,
                    &format!("ticket={t}"),
                    Some("https://evil.example.com"),
                ))
                .await,
                http::StatusCode::FORBIDDEN,
            );

            // No Origin at all must fail closed, not fall through.
            let t = plant_ticket(&session_id(), dir.path());
            assert_eq!(
                handshake_status(attach_request(addr, &format!("ticket={t}"), None)).await,
                http::StatusCode::FORBIDDEN,
            );
        }

        #[tokio::test]
        async fn upgrade_requires_an_unused_ticket() {
            let addr = serve().await;
            let origin = good_origin();

            // Never minted.
            assert_eq!(
                handshake_status(attach_request(addr, "ticket=made-up", Some(&origin))).await,
                http::StatusCode::FORBIDDEN,
            );

            // Minted, but already past its TTL.
            let stale = uuid::Uuid::new_v4().simple().to_string();
            TICKETS.lock().unwrap().insert(
                stale.clone(),
                Ticket {
                    session_id: session_id(),
                    worktree_id: 1,
                    cwd: std::env::temp_dir(),
                    label: "test".to_owned(),
                    expires_at: Instant::now() - Duration::from_secs(1),
                },
            );
            assert_eq!(
                handshake_status(attach_request(
                    addr,
                    &format!("ticket={stale}"),
                    Some(&origin)
                ))
                .await,
                http::StatusCode::FORBIDDEN,
            );
        }

        /// A rejected origin must not burn the ticket — otherwise a
        /// cross-site probe could deny the real client its session.
        #[tokio::test]
        async fn a_rejected_origin_leaves_the_ticket_redeemable() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let t = plant_ticket(&sid, dir.path());

            assert_eq!(
                handshake_status(attach_request(
                    addr,
                    &format!("ticket={t}"),
                    Some("https://evil.example.com"),
                ))
                .await,
                http::StatusCode::FORBIDDEN,
            );

            let (mut ws, _) = tokio_tungstenite::connect_async(attach_request(
                addr,
                &format!("ticket={t}"),
                Some(&good_origin()),
            ))
            .await
            .expect("the ticket should still be good");
            let _ = ws.close(None).await;
            end_session(&sid, "test cleanup").await;
        }

        /// Replaying a ticket must not attach a second socket.
        #[tokio::test]
        async fn a_ticket_cannot_be_replayed() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let t = plant_ticket(&sid, dir.path());
            let origin = good_origin();

            let (mut ws, _) = tokio_tungstenite::connect_async(attach_request(
                addr,
                &format!("ticket={t}"),
                Some(&origin),
            ))
            .await
            .unwrap();

            assert_eq!(
                handshake_status(attach_request(addr, &format!("ticket={t}"), Some(&origin))).await,
                http::StatusCode::FORBIDDEN,
            );
            let _ = ws.close(None).await;
            end_session(&sid, "test cleanup").await;
        }

        /// Drive a live session: read frames until `want` appears in the
        /// terminal output, returning everything seen.
        async fn read_until(ws: &mut Client, want: &str) -> String {
            let mut seen = String::new();
            loop {
                let msg = tokio::time::timeout(STEP_TIMEOUT, ws.next())
                    .await
                    .unwrap_or_else(|_| panic!("timed out waiting for {want:?}; saw: {seen:?}"))
                    .expect("socket closed early")
                    .expect("socket error");
                match msg {
                    WsMessage::Binary(b) => {
                        seen.push_str(&String::from_utf8_lossy(&b));
                        if seen.contains(want) {
                            return seen;
                        }
                    }
                    WsMessage::Text(t) => {
                        assert!(
                            !t.contains(r#""type":"exit""#),
                            "shell exited before {want:?}; saw: {seen:?}"
                        );
                    }
                    _ => {}
                }
            }
        }

        /// Collect the replayed scrollback of a reattach.
        ///
        /// Also asserts the bracketing the client depends on: `replay_begin`
        /// must come first and `replay_end` must arrive, because the client
        /// gates its terminal's own replies to that window (see
        /// `ServerControl::ReplayBegin`) and would deadlock its input without
        /// the closing frame.
        async fn read_replay(ws: &mut Client) -> String {
            let mut seen = String::new();
            let mut begun = false;
            loop {
                let msg = tokio::time::timeout(STEP_TIMEOUT, ws.next())
                    .await
                    .expect("timed out waiting for the replay")
                    .expect("socket closed early")
                    .expect("socket error");
                match msg {
                    WsMessage::Binary(b) => {
                        assert!(begun, "scrollback arrived before replay_begin");
                        seen.push_str(&String::from_utf8_lossy(&b));
                    }
                    WsMessage::Text(t) => {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                        match v["type"].as_str() {
                            Some("replay_begin") => begun = true,
                            Some("replay_end") => return seen,
                            other => panic!("unexpected control frame during replay: {other:?}"),
                        }
                    }
                    _ => {}
                }
            }
        }

        /// Read control frames until one of `type` arrives, returning it.
        async fn read_control(ws: &mut Client, ty: &str) -> serde_json::Value {
            loop {
                let msg = tokio::time::timeout(STEP_TIMEOUT, ws.next())
                    .await
                    .unwrap_or_else(|_| panic!("timed out waiting for a {ty:?} frame"))
                    .expect("socket closed early")
                    .expect("socket error");
                if let WsMessage::Text(t) = msg {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == ty {
                        return v;
                    }
                }
            }
        }

        async fn open(addr: SocketAddr, sid: &str, cwd: &std::path::Path, extra: &str) -> Client {
            let t = plant_ticket(sid, cwd);
            let (ws, _) = tokio_tungstenite::connect_async(attach_request(
                addr,
                &format!("ticket={t}{extra}"),
                Some(&good_origin()),
            ))
            .await
            .expect("handshake");
            ws
        }

        #[tokio::test]
        async fn runs_a_command_and_reports_the_shell_exit_code() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let mut ws = open(addr, &sid, dir.path(), "").await;
            let ready = read_control(&mut ws, "ready").await;
            assert_eq!(
                ready["resumed"], false,
                "a new session has nothing to replay"
            );

            // Split the marker so the shell's echo of the command line can't
            // be mistaken for the command's output.
            ws.send(WsMessage::Binary(
                b"printf 'veld%s\\n' '-pty-ok'\n".to_vec().into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, "veld-pty-ok").await;

            ws.send(WsMessage::Binary(b"exit 7\n".to_vec().into()))
                .await
                .unwrap();

            // The exit status has to survive the trip, or the UI can't tell a
            // clean logout from a crash.
            let exit = read_control(&mut ws, "exit").await;
            assert_eq!(exit["code"], 7, "exit code must reach the client");
            end_session(&sid, "test cleanup").await;
        }

        #[tokio::test]
        async fn spawns_in_the_ticket_directory() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            // macOS hands out /var/… symlinks for temp dirs while the shell
            // reports the resolved /private/var/… path.
            let real = dir.path().canonicalize().unwrap();
            let sid = session_id();
            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;

            // Tagged, because an interactive prompt (and the terminal-title
            // escape it emits) contains the cwd too — a bare `pwd` would
            // "pass" without the command ever running.
            ws.send(WsMessage::Binary(
                b"printf 'cwd=%s\\n' $PWD\n".to_vec().into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, &format!("cwd={}", real.display())).await;
            end_session(&sid, "test cleanup").await;
        }

        #[tokio::test]
        async fn initial_size_and_resize_reach_the_kernel() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            // The size the client already knows at connect time, so the first
            // prompt doesn't render at 80x24 and reflow.
            let mut ws = open(addr, &sid, dir.path(), "&cols=100&rows=40").await;
            read_control(&mut ws, "ready").await;

            ws.send(WsMessage::Binary(b"stty size\n".to_vec().into()))
                .await
                .unwrap();
            read_until(&mut ws, "40 100").await;

            ws.send(WsMessage::Text(
                r#"{"type":"resize","cols":132,"rows":50}"#.into(),
            ))
            .await
            .unwrap();
            ws.send(WsMessage::Binary(b"stty size\n".to_vec().into()))
                .await
                .unwrap();
            read_until(&mut ws, "50 132").await;
            end_session(&sid, "test cleanup").await;
        }

        /// The reload case: the socket goes away, the shell keeps running, and
        /// reattaching with the same session id gets it back with its
        /// scrollback.
        #[tokio::test]
        async fn a_session_survives_losing_its_socket() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            read_control(&mut ws, "ready").await;
            // Set a shell variable: proving it survives proves this is the
            // same shell process, not a fresh one in the same directory.
            ws.send(WsMessage::Binary(
                b"VELD_MARK=kept; printf 'set%s\\n' '-ok'\n".to_vec().into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, "set-ok").await;

            // Drop the socket the way a page reload does — no close frame.
            drop(ws);

            let mut again = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            let ready = read_control(&mut again, "ready").await;
            assert_eq!(ready["resumed"], true, "reattach must report a resume");

            again
                .send(WsMessage::Binary(
                    b"printf 'mark=%s\\n' $VELD_MARK\n".to_vec().into(),
                ))
                .await
                .unwrap();
            read_until(&mut again, "mark=kept").await;
            end_session(&sid, "test cleanup").await;
        }

        /// Output produced while nothing is attached still has to be there on
        /// return, or a reload during a build loses the build log.
        #[tokio::test]
        async fn output_while_detached_is_replayed_on_return() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;
            ws.send(WsMessage::Binary(
                b"printf 'up%s\\n' '-ok'\n".to_vec().into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, "up-ok").await;

            // Schedule output for after we are gone.
            ws.send(WsMessage::Binary(
                b"(sleep 1; printf 'while%s\\n' '-detached') &\n"
                    .to_vec()
                    .into(),
            ))
            .await
            .unwrap();
            drop(ws);
            tokio::time::sleep(Duration::from_secs(3)).await;

            let mut again = open(addr, &sid, dir.path(), "").await;
            let seen = read_replay(&mut again).await;
            assert!(
                seen.contains("while-detached"),
                "detached output must be replayed; saw: {seen:?}"
            );
            end_session(&sid, "test cleanup").await;
        }

        /// A second attach takes over and the first is told why, rather than
        /// two sockets silently fighting over one shell's input.
        #[tokio::test]
        async fn a_second_attach_takes_over() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut first = open(addr, &sid, dir.path(), "").await;
            read_control(&mut first, "ready").await;

            let mut second = open(addr, &sid, dir.path(), "").await;
            read_control(&mut second, "ready").await;

            let taken = read_control(&mut first, "taken_over").await;
            assert_eq!(taken["type"], "taken_over");
            end_session(&sid, "test cleanup").await;
        }

        /// Closing a tab must end the shell now, not at the detach grace.
        #[tokio::test]
        async fn closing_a_session_hangs_up_the_shell() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;

            // Park a marker process in the terminal's process group and learn
            // its pid, so we can watch it die rather than guess. The pid goes
            // through a file rather than the terminal: an interactive shell's
            // prompt, bracketed-paste escapes and history expansion make
            // scraping stdout for it needlessly brittle.
            let pidfile = dir.path().join("job.pid");
            ws.send(WsMessage::Binary(
                format!("sleep 300 & echo $! > {}\n", pidfile.display())
                    .into_bytes()
                    .into(),
            ))
            .await
            .unwrap();

            let deadline = Instant::now() + STEP_TIMEOUT;
            let pid: i32 = loop {
                if let Ok(s) = std::fs::read_to_string(&pidfile) {
                    if let Ok(p) = s.trim().parse() {
                        break p;
                    }
                }
                assert!(Instant::now() < deadline, "background job never started");
                tokio::time::sleep(Duration::from_millis(50)).await;
            };

            // Dropping the socket alone must NOT kill it — that is the reload
            // case, and killing there is the bug this model exists to avoid.
            drop(ws);
            tokio::time::sleep(Duration::from_millis(500)).await;
            // SAFETY: signal 0 performs the permission/existence check only.
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                0,
                "detaching must not kill the shell"
            );

            assert!(end_session(&sid, "test").await);

            let deadline = Instant::now() + KILL_GRACE + Duration::from_secs(10);
            loop {
                if unsafe { libc::kill(pid, 0) } != 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "pid {pid} survived the session close"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        /// The regression this module's `send_replace` comment describes.
        ///
        /// A shell that exits while **nothing is attached** has no `watch`
        /// receivers, and `watch::Sender::send` returns early without storing in
        /// that case — so the exit code was silently lost and the next attach
        /// presented a dead shell as a live prompt. The sibling test below exits
        /// while a socket is attached, which keeps a receiver alive and passes
        /// either way; only this ordering pins the invariant.
        #[tokio::test]
        async fn an_exit_while_detached_is_still_reported_on_return() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;
            // Queue a delayed exit, then leave before it happens. Deliberately
            // not a signal: an *interactive* shell ignores SIGTERM, so
            // `kill -TERM $$` waits forever on a shell that never dies.
            ws.send(WsMessage::Binary(b"sleep 2; exit 5\n".to_vec().into()))
                .await
                .unwrap();
            drop(ws);
            tokio::time::sleep(Duration::from_secs(5)).await;

            let mut again = open(addr, &sid, dir.path(), "").await;
            let exit = read_control(&mut again, "exit").await;
            assert_eq!(
                exit["code"], 5,
                "the exit of a shell that died while detached must survive"
            );
            end_session(&sid, "test cleanup").await;
        }

        /// Dropping a socket must start the detach clock — the reaper's only
        /// input. `a_session_survives_losing_its_socket` proves the shell lives;
        /// this proves it becomes collectable.
        #[tokio::test]
        async fn losing_a_socket_starts_the_detach_clock() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;
            {
                let live = SESSIONS.lock().await;
                let s = live.get(&sid).expect("session registered");
                assert!(
                    s.detached_since.lock().unwrap().is_none(),
                    "an attached session must have no detach clock running"
                );
            }

            drop(ws);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let stamped = {
                    let live = SESSIONS.lock().await;
                    live.get(&sid)
                        .map(|s| s.detached_since.lock().unwrap().is_some())
                        .unwrap_or(false)
                };
                if stamped {
                    break;
                }
                assert!(Instant::now() < deadline, "detach clock never started");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            end_session(&sid, "test cleanup").await;
        }

        /// A reattach after the shell has already exited reports the exit
        /// rather than presenting a dead prompt as live.
        #[tokio::test]
        async fn reattaching_after_exit_reports_the_exit() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;
            ws.send(WsMessage::Binary(b"exit 5\n".to_vec().into()))
                .await
                .unwrap();
            assert_eq!(read_control(&mut ws, "exit").await["code"], 5);
            drop(ws);

            let mut again = open(addr, &sid, dir.path(), "").await;
            assert_eq!(read_control(&mut again, "exit").await["code"], 5);
            end_session(&sid, "test cleanup").await;
        }
    }
}
