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
//!
//! # The PTY is not in this process
//!
//! A session's PTY master, its shell and its scrollback belong to a **holder
//! process** — one per session, `veld-daemon --pty-holder` — which the daemon
//! talks to over a unix socket. That is what makes a shell survive `veld
//! update`: the master descriptor is not the daemon's to lose. See
//! [`holder`] for why it is per-session and what must not regress.
//!
//! What this module keeps is everything a *client* can observe: the tickets, the
//! origin gate, the takeover epoch, the one-writer rule, the replay bracket, the
//! detach grace and the session cap. The scrollback here is a **mirror**, fed by
//! [`pump_holder`] exactly as it was once fed by the PTY read loop — which is
//! why [`serve_socket`]'s subscribe-and-snapshot-under-one-lock argument is
//! unchanged. The holder's own copy is read once, when a freshly started daemon
//! adopts a session it did not spawn ([`adopt_existing_sessions`]).

#[path = "pty/holder.rs"]
pub mod holder;

#[path = "pty/wire.rs"]
mod wire;

use std::collections::{HashMap, VecDeque};
use std::path::{Path as FsPath, PathBuf};
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
use portable_pty::PtySize;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, warn};

use super::management::{check_csrf, open_db};
use wire::HolderConfig;

/// How long a minted ticket stays redeemable. Long enough to survive a slow
/// render and a wedged event loop, short enough that a leaked one (a shared
/// screen, a stray log) is stale before it can be used.
const TICKET_TTL: Duration = Duration::from_secs(30);

/// Ceiling on live sessions across all worktrees. Each one is a real shell
/// process plus file descriptors, a task and a scrollback buffer; without a cap
/// a scripted client (or a UI bug in a render loop) forks until the machine
/// gives up.
///
/// Hitting it is an ordinary outcome rather than an attack — a session outlives
/// the page that opened it, and now the daemon that spawned it, so a long day of
/// opening panes reaches the cap without anything being wrong. That is why
/// [`mint_ticket`] reports it as a readable error instead of leaving the client to
/// infer it from a failed handshake.
///
/// It used to be spent by *browsing*: a worktree's default layout seeded a
/// terminal, so every worktree merely selected started a shell. It no longer does
/// (`defaultLayout` in `ui/src/panes/model.ts` seeds a `new` pane), which is what
/// makes this bound comfortable rather than tight.
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
/// what collects them.
///
/// **Now a default, not a constant.** The effective value is the
/// `terminal.detachGraceMinutes` setting, read through [`configured_detach_grace`]
/// — this is the fallback when there is no database to ask. The number is quoted
/// in `README.md` and `website/llms-full.txt`; those copies are pinned by a test
/// beside the default in `veld_core::db::settings`, so changing it there tells you
/// which files to update.
const DETACH_GRACE: Duration =
    Duration::from_secs(veld_core::db::DEFAULT_DETACH_GRACE_MINUTES as u64 * 60);

/// The detach grace to enforce right now.
///
/// Re-read on every reaper pass rather than cached at startup: a user who
/// lengthens the grace because a build keeps getting reaped should not have to
/// restart the daemon — and restarting it is the one thing the holder-process
/// design made safe, so requiring it here would be a poor trade. The read is a
/// single indexed row on a local file once a minute.
///
/// An unreachable database falls back to [`DETACH_GRACE`]. Deliberately not "never
/// reap": a daemon that cannot read its settings still leaks shells without a
/// bound, and the default is the behaviour every release before this one had.
fn configured_detach_grace() -> Duration {
    let grace = match veld_core::db::Db::open() {
        Ok(db) => db.detach_grace(),
        Err(e) => {
            warn!("could not read the detach grace setting, using the default: {e}");
            DETACH_GRACE
        }
    };
    GRACE_HINT.store(grace.as_secs(), Ordering::Relaxed);
    grace
}

/// Last grace read from the database, in seconds; `0` = never read.
///
/// The reaper refreshes it once a minute, and [`detach_grace_hint`] is what the
/// session-spawn path reads. A published value only ever gets *more* stale than the
/// database, never wrong in a way that matters: it is handed to a holder as its
/// self-destruct timeout, and being a minute behind a preference change is not a
/// behaviour anyone can observe.
static GRACE_HINT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The grace to hand a newly spawned holder, without touching the database.
///
/// `obtain_session` runs on the attach path, and opening SQLite there is exactly the
/// thing AGENTS.md's dev-database rule calls out as a design decision rather than a
/// detail: it made every terminal spawn do file I/O, and it is what let a plain
/// `cargo test` migrate a real database in the first place. The reaper already reads
/// the setting every minute, so the spawn path reads its published value and only
/// falls back to a real read when no reaper pass has happened yet.
fn detach_grace_hint() -> Duration {
    match GRACE_HINT.load(Ordering::Relaxed) {
        0 => configured_detach_grace(),
        secs => Duration::from_secs(secs),
    }
}

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

/// How long a holder gets to start, bind its socket and answer.
///
/// Generous relative to what it costs (a fork/exec, a bind and a connect —
/// milliseconds), because the alternative to waiting is reporting "could not
/// start a shell" on a loaded machine.
const HOLDER_START_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the daemon retries connecting to a holder it has just spawned.
const HOLDER_CONNECT_INTERVAL: Duration = Duration::from_millis(5);

/// How long a holder gets to send its `Hello` once connected.
///
/// Separate from [`HOLDER_START_TIMEOUT`] because it bounds a different failure:
/// a socket that accepts but never speaks. Without it, one wedged holder would
/// stall daemon startup while every session behind it waited to be adopted.
const HOLDER_HELLO_TIMEOUT: Duration = Duration::from_secs(2);

/// Frames queued towards a holder. Keystrokes and resizes only, so this is
/// deliberately small; the hangup does not queue behind them (see
/// [`pump_to_holder`]).
const HOLDER_INPUT_CHANNEL: usize = 64;

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
/// Worktree ids that currently have a live terminal session.
///
/// Used by worktree auto-eviction, which must not delete a checkout somebody has a
/// shell open in. A veld run is not the only sign of life — and it is not even the
/// common one here, since a worktree can be used all day through terminal panes
/// alone without `veld start` ever running. Sessions deliberately outlive a daemon
/// restart, so this outlives one too.
pub async fn worktree_ids_with_sessions() -> std::collections::HashSet<i64> {
    SESSIONS
        .lock()
        .await
        .values()
        // An exited shell is not a reason to keep a checkout alive; its pane is
        // still there, but nothing is running in it.
        .filter(|s| s.exit.borrow().is_none())
        .map(|s| s.worktree_id)
        .collect()
}

pub fn spawn_session_reaper() {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(REAP_INTERVAL).await;
            reap_detached(configured_detach_grace()).await;
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
///
/// The shell itself lives in a holder process; this is the daemon's handle on
/// it. Everything here is either a client-facing invariant (the epochs, the
/// detach clock, the cap slot) or a mirror of holder state (the scrollback, the
/// exit code).
struct Session {
    id: String,
    /// Which worktree this belongs to. Checked on reattach so a session cannot
    /// be adopted from another worktree's pane.
    worktree_id: i64,
    label: String,
    /// Frames towards the holder: keystrokes and resizes.
    to_holder: mpsc::Sender<(u8, Vec<u8>)>,
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
    /// [`pump_to_holder`] turns this into a [`wire::HANGUP`] frame, which is the
    /// only way the daemon ends a shell. It never signals the process itself:
    /// the holder owns the **unreaped** child, so only the holder can signal the
    /// group without racing `wait()` — the moment a child is reaped its pid is
    /// free for reuse, and a late `killpg` could hit an unrelated group.
    ///
    /// A `watch` rather than a queued frame so the hangup cannot sit behind a
    /// backlog of keystrokes: the writer polls it first.
    closing: watch::Sender<bool>,
    /// The shell's pid, for log lines only — it is a pid in *another* process's
    /// child list, and nothing here may signal it.
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

/// End a session: take it out of the registry and ask its holder to hang the
/// shell up.
///
/// The signalling happens in the holder, not here — see [`Session::closing`].
/// This function must not signal the pid itself.
async fn end_session(id: &str, reason: &str) -> bool {
    let session = SESSIONS.lock().await.remove(id);
    let Some(session) = session else {
        return false;
    };
    info!(session = %session.id, worktree = %session.label, reason, "terminal session ended");
    // `send_replace`, not `send`: the writer task is the only receiver and may
    // already have finished (a shell that exited on its own), in which case
    // `send` would drop the flag and there is nothing left to hang up anyway.
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

/// Leave every live session running, and say so.
///
/// This function used to hang up every shell, because it had to: the PTY masters
/// were this process's descriptors and died with it, so the choice was between a
/// deliberate SIGHUP and the kernel's hangup half a second later with orphaned
/// grandchildren. Now the masters belong to holder processes, so a daemon
/// shutdown is invisible to the shells — and `veld update` stops being something
/// to schedule around, which is the entire point of the holder split.
///
/// What still ends a shell: an explicit `DELETE /api/pty/sessions/{id}` (the
/// user closed the tab), the detach reaper, and the holder's own orphan grace if
/// no daemon ever comes back. Dropping the registry here closes each holder
/// connection, which is what starts that last clock.
pub async fn shutdown_sessions() {
    let count = SESSIONS.lock().await.len();
    if count == 0 {
        return;
    }
    info!(
        count,
        "leaving terminal sessions running for the next daemon"
    );
}

// ---------------------------------------------------------------------------
// Holders
// ---------------------------------------------------------------------------

/// Where this instance's holder sockets live.
///
/// The name belongs to `veld_core::instance::pty_dir`, because it has three
/// readers: this module binds and scans the directory, `veld doctor` reports it,
/// and `veld uninstall` sweeps it.
#[cfg(not(test))]
fn pty_dir() -> PathBuf {
    veld_core::instance::pty_dir()
}

/// Under test, holders run in-process (see [`start_holder`]) but still bind real
/// sockets, so they need a directory of their own. Derived from the pid rather
/// than an env var because mutating the environment races every other test in
/// the binary.
#[cfg(test)]
fn pty_dir() -> PathBuf {
    std::env::temp_dir().join(format!("veld-pty-test-{}", std::process::id()))
}

/// The socket path for a session.
///
/// Named by a digest of the session id rather than the id itself, because a unix
/// socket path is capped by `sockaddr_un::sun_path` — 104 bytes on macOS, 108 on
/// Linux — and a session id may be up to 64 characters
/// ([`valid_session_id`]). `/Users/<name>/.veld/pty-19899/` plus a 64-character
/// id plus `.sock` overruns that on an ordinary Mac, and the failure surfaces as
/// an unexplained "could not start a shell". A fixed 16-hex name keeps every
/// path comfortably short whatever the client names its terminals.
///
/// Nothing needs to invert this: a holder announces its own session id in its
/// [`wire::Hello`], and adoption checks that `socket_for` of *that* id is the
/// path it was found at — which is a stronger check than a filename comparison,
/// since it also catches a digest collision.
fn socket_for(session_id: &str) -> PathBuf {
    pty_dir().join(format!("{:016x}.sock", session_digest(session_id)))
}

/// Whether a path has the exact shape [`socket_for`] produces.
///
/// The gate on anything destructive in [`adopt_existing_sessions`]. It is a
/// name check, not a claim about the contents — the greeting is what proves a
/// socket is the session it should be.
fn is_holder_socket_name(path: &FsPath) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".sock"))
        .is_some_and(|stem| {
            stem.len() == 16
                && stem
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        })
}

/// FNV-1a over the session id. A hash, not a hash *function* in the security
/// sense — nothing here depends on it being hard to invert, only on distinct ids
/// landing on distinct names, which a collision would turn into a refused spawn
/// (the second holder finds a live socket and declines) rather than a crossed
/// wire.
fn session_digest(session_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in session_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// A holder that has answered: its greeting, its scrollback, and the connection.
struct Attached {
    hello: wire::Hello,
    scrollback: Vec<u8>,
    stream: UnixStream,
}

/// Connect to a holder and complete the handshake.
///
/// The first two frames are fixed — `Hello` then `Scrollback` — so that a caller
/// has everything it needs to build a [`Session`] before any live output can
/// arrive. A protocol version this build does not speak is refused *and* told to
/// hang up: leaving it would strand a shell no daemon can ever reach, since
/// every daemon would refuse it identically.
async fn connect_holder(path: &FsPath) -> anyhow::Result<Attached> {
    let mut stream = UnixStream::connect(path).await?;
    let handshake = async {
        let hello = match wire::read_frame(&mut stream).await? {
            Some(frame) if frame.kind == wire::HELLO => {
                serde_json::from_slice::<wire::Hello>(&frame.payload)?
            }
            Some(frame) => anyhow::bail!("expected a hello frame, got {:#x}", frame.kind),
            None => anyhow::bail!("holder closed before greeting"),
        };
        if hello.protocol != wire::PROTOCOL {
            // The one frame every version understands. See `wire`'s module docs.
            let _ = wire::write_frame(&mut stream, wire::HANGUP, b"").await;
            anyhow::bail!(
                "holder speaks protocol {}, this daemon speaks {}",
                hello.protocol,
                wire::PROTOCOL
            );
        }
        let scrollback = match wire::read_frame(&mut stream).await? {
            Some(frame) if frame.kind == wire::SCROLLBACK => frame.payload,
            Some(frame) => anyhow::bail!("expected a scrollback frame, got {:#x}", frame.kind),
            None => anyhow::bail!("holder closed before sending its scrollback"),
        };
        Ok::<_, anyhow::Error>((hello, scrollback))
    };
    let (hello, scrollback) = tokio::time::timeout(HOLDER_HELLO_TIMEOUT, handshake)
        .await
        .map_err(|_| anyhow::anyhow!("holder did not complete the handshake in time"))??;
    Ok(Attached {
        hello,
        scrollback,
        stream,
    })
}

/// Whether a failed [`connect_holder`] means "nothing is listening here".
///
/// The distinction is load-bearing in [`adopt_one`], which deletes the path for
/// exactly this case and must not for any other. Matched on `errno` rather than
/// `io::ErrorKind`, because the three cases do not share one kind — measured, not
/// assumed:
///
/// - `ECONNREFUSED` — a socket file whose listener is gone. The common case.
/// - `ENOENT` — the file vanished under us, e.g. a holder exiting mid-scan.
/// - `ENOTSOCK` — the path is not a socket at all (a plain file left behind).
///   `ErrorKind` has no stable variant for this one, which is how the first
///   version of this function let a stale file survive every boot.
///
/// Everything else — a handshake timeout, `EMFILE`, `EACCES`, a refused protocol
/// version — can happen to a holder that is alive and serving a shell, and must
/// leave its socket alone.
fn is_unanswered(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .and_then(|io| io.raw_os_error())
        .is_some_and(|errno| matches!(errno, libc::ECONNREFUSED | libc::ENOENT | libc::ENOTSOCK))
}

/// Start a holder for `cfg` and connect to it.
///
/// In the real daemon this spawns `veld-daemon --pty-holder`, deliberately in its
/// own process group: launchd's `bootout` and systemd's default
/// `KillMode=control-group` would otherwise take the holder down with the daemon
/// — the exact failure this design exists to prevent. (`veld_core::setup` gives
/// the daemon's unit `KillMode=process` for the same reason the helper's has it.)
#[cfg(not(test))]
async fn start_holder(cfg: &HolderConfig) -> anyhow::Result<Attached> {
    // Imported here rather than at module scope: every use of it is in this
    // function, which the test build replaces wholesale.
    use anyhow::Context;

    let exe = std::env::current_exe().context("could not find the daemon binary")?;
    let mut child = tokio::process::Command::new(exe)
        .arg("--pty-holder")
        .stdin(std::process::Stdio::piped())
        // stdout is nulled and stderr inherited: the holder's tracing lines
        // belong in the daemon's log, and a *pipe* nobody drains would block it
        // the first time it filled.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .process_group(0)
        .spawn()
        .context("could not spawn a terminal holder")?;

    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("holder stdin was not piped"))?;
        stdin
            .write_all(serde_json::to_string(cfg)?.as_bytes())
            .await
            .context("could not send the holder its configuration")?;
        // Dropping stdin is the holder's signal that the configuration is
        // complete; it reads to end of stream.
    }
    // Dropped, not `forget`ten, and not awaited. Tokio's `Child` drop does not
    // kill a child that was spawned without `kill_on_drop` — it hands it to the
    // runtime's orphan queue, which reaps it on SIGCHLD
    // (`tokio/src/process/unix/reap.rs`). So dropping is exactly the pair of
    // properties needed here: the holder outlives us, *and* its pid is collected
    // when it eventually exits. `forget` kept the first half and lost the second,
    // which left one zombie per closed terminal pane in a daemon that runs for
    // weeks — and unbounded by `MAX_SESSIONS`, which caps live sessions and not
    // churn.
    drop(child);

    let attached = await_holder(&cfg.socket).await?;
    // The holder that answered must be the one this call started. It is not
    // enough that *something* is listening at `cfg.socket`: if our child failed
    // to bind (another holder already owns that path), the poll-connect below
    // lands on the incumbent instead, and the caller would then be handed a
    // session belonging to someone else — see `verify_identity`.
    verify_identity(&attached, cfg)?;
    Ok(attached)
}

/// Reject a holder whose greeting does not match what we asked for.
///
/// Two things make this load-bearing rather than defensive:
///
/// - **The socket path is derived, not unique.** `socket_for` digests the
///   client-chosen session id, so a collision — or a stale holder for the same id
///   that adoption skipped — puts a *different* live session behind the path this
///   spawn expects. `register` keys the registry off the greeting, so without this
///   check that session's entry would be replaced and its pump would report a
///   bogus exit.
/// - **`worktree_id` comes from the greeting too**, and `mint_ticket`'s
///   cross-worktree guard compares the *registry's* value. A greeting accepted
///   unchecked therefore walks straight past that guard.
///
/// Deliberately no hangup on failure: the holder that answered is somebody else's
/// and may have a live shell in it. `adopt_one` has the same rule for the same
/// reason.
fn verify_identity(attached: &Attached, cfg: &HolderConfig) -> anyhow::Result<()> {
    let hello = &attached.hello;
    if hello.session_id != cfg.session_id || hello.worktree_id != cfg.worktree_id {
        anyhow::bail!(
            "holder at {} answers for session {:?} of worktree {}, not {:?} of {}",
            cfg.socket.display(),
            hello.session_id,
            hello.worktree_id,
            cfg.session_id,
            cfg.worktree_id
        );
    }
    Ok(())
}

/// Under test, the holder runs as a task in this process: `current_exe()` is the
/// test binary, which has no `--pty-holder` mode. It still binds a real socket
/// and speaks the real protocol, so everything except the process boundary is
/// exercised; the boundary itself — and survival across a daemon restart — is
/// covered by `tests/pty_recovery.rs`, which drives the real binary.
#[cfg(test)]
async fn start_holder(cfg: &HolderConfig) -> anyhow::Result<Attached> {
    let owned = cfg.clone();
    tokio::spawn(async move {
        if let Err(e) = holder::run(owned).await {
            warn!("in-process test holder failed: {e}");
        }
    });
    let attached = await_holder(&cfg.socket).await?;
    verify_identity(&attached, cfg)?;
    Ok(attached)
}

/// Poll-connect until the holder is listening, or give up.
///
/// Polling rather than passing a pre-bound listener descriptor to the child: fd
/// inheritance means `pre_exec` and manual `dup2`, and this loop costs a couple
/// of 5 ms sleeps on a normal spawn.
async fn await_holder(socket: &FsPath) -> anyhow::Result<Attached> {
    let deadline = Instant::now() + HOLDER_START_TIMEOUT;
    let mut last: Option<anyhow::Error> = None;
    while Instant::now() < deadline {
        match connect_holder(socket).await {
            Ok(attached) => return Ok(attached),
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(HOLDER_CONNECT_INTERVAL).await;
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("holder did not start in time")))
}

/// Tell a holder we do not want it after all, and let it clean itself up.
///
/// Only ever called on a holder this daemon started (or adopted) and then decided
/// against: dropping the connection alone would leave its shell running until the
/// orphan grace. **Never call it on a holder that failed
/// [`verify_identity`]** — that one belongs to another session and may have a live
/// shell in it.
async fn discard_holder(attached: Attached, reason: &str) {
    let mut stream = attached.stream;
    debug!(session = %attached.hello.session_id, reason, "discarding a holder");
    let _ = wire::write_frame(&mut stream, wire::HANGUP, b"").await;
}

/// Forward frames to the holder, with the hangup jumping the queue.
///
/// `biased`, unlike the socket loop: here the priority order is the point. A
/// hangup must not wait behind a backlog of keystrokes, and it cannot starve the
/// other branches because the watch fires once and this task returns
/// immediately after acting on it.
async fn pump_to_holder(
    mut writer: OwnedWriteHalf,
    mut rx: mpsc::Receiver<(u8, Vec<u8>)>,
    mut closing: watch::Receiver<bool>,
    mut exit: watch::Receiver<Option<u32>>,
) {
    // Seeded from the current values, not defaults: `subscribe()` marks what is
    // already there as seen, so a `DELETE` that lands between registration and
    // this task's first poll would set the flag, never fire `changed()`, and
    // leave the shell running with nothing left to hang it up.
    if *closing.borrow_and_update() {
        let _ = wire::write_frame(&mut writer, wire::HANGUP, b"").await;
        return;
    }
    if exit.borrow_and_update().is_some() {
        return;
    }
    loop {
        tokio::select! {
            biased;

            Ok(()) = closing.changed() => {
                if *closing.borrow_and_update() {
                    let _ = wire::write_frame(&mut writer, wire::HANGUP, b"").await;
                    return;
                }
            }

            // The shell is gone, so nothing more can be written to it. Closing
            // the connection here is what lets the holder exit promptly instead
            // of lingering with a dead shell.
            Ok(()) = exit.changed() => {
                if exit.borrow_and_update().is_some() {
                    return;
                }
            }

            frame = rx.recv() => match frame {
                Some((kind, payload)) => {
                    if wire::write_frame(&mut writer, kind, &payload).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
        }
    }
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
        Err(SessionError::Starting) => {
            // Two sockets attaching the same session id at once, with no shell
            // there yet. The other one is milliseconds from registering it, and
            // this client's own reconnect will then resume it — which is a better
            // answer than starting a second shell for a tab that wanted one.
            // `warn`, not `debug`: reaching this means a spawn outlived its whole
            // timeout, and the client cannot tell the user anything more specific
            // than "connection lost".
            warn!(
                session = %ticket.session_id,
                "a concurrent start for this session never finished"
            );
            return (
                StatusCode::CONFLICT,
                "that terminal is already starting — reconnecting will attach to it",
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
    /// Another attach for the same session id is mid-spawn. The caller cannot
    /// usefully wait, because the shell it would get is about to exist under an
    /// id it already knows — it retries.
    Starting,
}

/// Session ids currently being spawned, so exactly one holder is ever started
/// per id.
///
/// The registry alone cannot express this: it holds *finished* sessions, and the
/// window that matters is between the spawn and the insert. Two attaches for one
/// id inside that window used to both spawn a holder at the same socket path,
/// which is a path with no room for two — the second holder refuses to bind and
/// exits, so the second *daemon task* poll-connects onto the first holder and
/// then, believing it owned it, told it to hang up. That killed a live shell.
/// [`verify_identity`] would now catch it, but not starting the second spawn at
/// all is what makes the whole class impossible.
static STARTING: LazyLock<Mutex<std::collections::HashSet<String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Marks a session id as mid-spawn, clearing it on drop.
///
/// A guard rather than a pair of calls: every error path out of the spawn — a
/// failed `fork`, a refused bind, an identity mismatch, a `?` anywhere — must
/// release the id, and a leaked entry would make that session permanently
/// unopenable for the life of the daemon.
struct StartGuard(String);

impl StartGuard {
    fn claim(id: &str) -> Option<Self> {
        let mut starting = STARTING.lock().expect("starting set poisoned");
        starting
            .insert(id.to_owned())
            .then(|| StartGuard(id.to_owned()))
    }
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        STARTING
            .lock()
            .expect("starting set poisoned")
            .remove(&self.0);
    }
}

/// The live session for a ticket, starting a holder if the named session is
/// gone.
///
/// The registry lock is released across the holder start — holding it across a
/// `fork`/`exec` and a poll-connect would serialise every other pane's attach
/// behind this one — and [`STARTING`] is what keeps that safe for two
/// attaches naming the *same* session.
async fn obtain_session(
    ticket: &Ticket,
    size: PtySize,
) -> Result<(Arc<Session>, bool), SessionError> {
    if let Some(existing) = SESSIONS.lock().await.get(&ticket.session_id) {
        return Ok((existing.clone(), true));
    }

    // Claimed before the capacity check so a retry cannot spend a slot.
    let _starting = match StartGuard::claim(&ticket.session_id) {
        Some(guard) => guard,
        // Another socket is starting this very session. Wait for it rather than
        // failing: the client has no retry — `ws.onclose` puts the pane in an error
        // state — so answering with a status here left a dead pane needing a second
        // manual reconnect. Bounded by the same timeout the spawn itself gets, and
        // what it waits for is a registry entry, which is exactly what a resumed
        // attach reads.
        None => {
            let deadline = Instant::now() + HOLDER_START_TIMEOUT;
            loop {
                tokio::time::sleep(HOLDER_CONNECT_INTERVAL).await;
                if let Some(existing) = SESSIONS.lock().await.get(&ticket.session_id) {
                    return Ok((existing.clone(), true));
                }
                if Instant::now() >= deadline {
                    return Err(SessionError::Starting);
                }
            }
        }
    };
    // Re-check: the spawn we were waiting behind may have finished between the
    // read above and this claim.
    if let Some(existing) = SESSIONS.lock().await.get(&ticket.session_id) {
        return Ok((existing.clone(), true));
    }

    let slot = SessionSlot::claim().ok_or(SessionError::AtCapacity)?;
    let cfg = HolderConfig {
        session_id: ticket.session_id.clone(),
        worktree_id: ticket.worktree_id,
        label: ticket.label.clone(),
        cwd: ticket.cwd.clone(),
        cols: size.cols,
        rows: size.rows,
        socket: socket_for(&ticket.session_id),
        // The holder's own self-destruct grace, used when the *daemon* is gone and
        // nothing is left to reap it. Read at spawn, so a holder keeps the value
        // that was configured when its shell started: changing the setting affects
        // the daemon-side reaper immediately but cannot reach into holders already
        // running, and reaching into them would mean a protocol message whose only
        // job is to move a timer nobody is waiting on.
        orphan_grace_secs: detach_grace_hint().as_secs(),
    };
    let attached = start_holder(&cfg).await.map_err(SessionError::Spawn)?;

    let mut sessions = SESSIONS.lock().await;
    if let Some(existing) = sessions.get(&ticket.session_id) {
        discard_holder(attached, "another attach won the race").await;
        return Ok((existing.clone(), true));
    }
    let session = register(&mut sessions, attached, slot);
    info!(
        session = %session.id,
        worktree = %ticket.label,
        pid = session.pid,
        "terminal session started"
    );
    Ok((session, false))
}

/// Put an attached holder in the registry and start its two pumps.
///
/// Takes the already-held registry guard so that inserting and spawning cannot
/// be interleaved with another attach observing a half-registered session.
fn register(
    sessions: &mut HashMap<String, Arc<Session>>,
    attached: Attached,
    slot: SessionSlot,
) -> Arc<Session> {
    let Attached {
        hello,
        scrollback,
        stream,
    } = attached;
    let (reader, writer) = stream.into_split();

    let (output, _) = broadcast::channel(OUTPUT_CHANNEL);
    // Seeded with the holder's answer, so a session adopted after its shell
    // already finished reports the exit instead of presenting a dead prompt as a
    // live one.
    let (exit, _) = watch::channel(hello.exited);
    let (attach_epoch, _) = watch::channel(0u64);
    let (closing, _) = watch::channel(false);
    let (to_holder, holder_rx) = mpsc::channel(HOLDER_INPUT_CHANNEL);

    // The mirror starts as the holder's copy: for a session this daemon just
    // started that is empty, and for an adopted one it is everything the
    // previous daemon (or no daemon at all) saw.
    let mut mirror = Scrollback::new();
    mirror.push(&scrollback);

    let session = Arc::new(Session {
        id: hello.session_id.clone(),
        worktree_id: hello.worktree_id,
        label: hello.label.clone(),
        to_holder,
        output,
        scrollback: Mutex::new(mirror),
        exit,
        attach_seq: std::sync::atomic::AtomicU64::new(0),
        attach_epoch,
        // Starts detached: `serve_socket` marks it attached, and if the socket
        // never arrives the reaper must still be able to collect it.
        //
        // Backdated by however long the holder has been without a daemon, so an
        // adopted session inherits the clock it was already on. Restarting the
        // clock here instead meant `DETACH_GRACE` never elapsed for a daemon that
        // restarts more often than the grace, which is the one leak it exists to
        // bound. `checked_sub` because a holder reporting an elapsed time larger
        // than this process's uptime must not panic; that falls back to a *full*
        // fresh grace, which is the generous answer rather than the strict one —
        // it is only reachable from a corrupt greeting, and refusing to hold a
        // live shell open is worse than holding one 30 minutes too long.
        detached_since: Mutex::new(Some(
            hello
                .detached_secs
                .and_then(|secs| Instant::now().checked_sub(Duration::from_secs(secs)))
                .unwrap_or_else(Instant::now),
        )),
        closing,
        pid: hello.pid,
        _slot: slot,
    });
    sessions.insert(session.id.clone(), session.clone());

    tokio::spawn(pump_to_holder(
        writer,
        holder_rx,
        session.closing.subscribe(),
        session.exit.subscribe(),
    ));
    // Draining the holder is the session's job, not the socket's: output
    // produced while nothing is attached still has to land in the mirror, or a
    // reload would come back to a screen missing everything that happened while
    // the page was gone.
    tokio::spawn(pump_holder(session.clone(), reader));
    session
}

/// Re-adopt the sessions of holders that outlived a previous daemon.
///
/// Called once at startup, before the reaper. Every socket in the directory is a
/// candidate: one that answers becomes a session again — with its scrollback, its
/// shell and whatever is running in it — and one that does not is a leftover file
/// from a holder that is gone, which is removed so it is not probed again on
/// every boot.
///
/// Concurrent rather than sequential because a socket that accepts but never
/// speaks costs [`HOLDER_HELLO_TIMEOUT`], and one wedged holder must not add that
/// to the startup of every session behind it.
pub async fn adopt_existing_sessions() {
    let dir = pty_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No directory means no holders, which is the common case.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            warn!("could not read the terminal holder directory {dir:?}: {e}");
            return;
        }
    };

    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Only names this daemon could have written, because a failed handshake
        // ends in `remove_file`: `VELD_PTY_DIR` is a plain env var, and pointed
        // one level up at `~/.veld` a boot would otherwise delete `daemon.sock`.
        // Anything else in the directory is not ours to reason about.
        if is_holder_socket_name(&path) {
            candidates.push(path);
        }
    }
    if candidates.is_empty() {
        return;
    }

    let adopted = futures_util::future::join_all(
        candidates
            .into_iter()
            .map(|path| async move { adopt_one(&path).await }),
    )
    .await
    .into_iter()
    .filter(|ok| *ok)
    .count();
    if adopted > 0 {
        info!(adopted, "adopted terminal sessions from a previous daemon");
    }
}

/// Adopt one holder, or clean up after it. `true` if it became a session.
async fn adopt_one(path: &FsPath) -> bool {
    // The slot is claimed *before* connecting, not after. Connecting first meant
    // an over-cap holder got a connection that was immediately dropped, and a
    // dropped connection restarts its orphan clock — so a daemon restarting more
    // often than the grace kept a 49th shell alive forever.
    let Some(slot) = SessionSlot::claim() else {
        warn!("not adopting {path:?}: already at {MAX_SESSIONS} sessions");
        return false;
    };

    let attached = match connect_holder(path).await {
        Ok(attached) => attached,
        Err(e) => {
            // Remove the door **only** when nobody is behind it. Every other
            // failure — the handshake timing out, `EMFILE`, a protocol version
            // this build refuses — can happen to a holder that is alive and
            // serving a shell, and unlinking its socket would strand that shell
            // permanently: the listener keeps the removed inode, so no later
            // daemon can ever reach it again. Left in place, the next boot (or the
            // next build) can.
            if is_unanswered(&e) {
                debug!("removing stale holder socket {path:?}: {e}");
                let _ = std::fs::remove_file(path);
            } else {
                warn!("leaving holder socket {path:?} in place: {e}");
            }
            return false;
        }
    };
    let id = attached.hello.session_id.clone();
    // The id becomes a registry key and a log field, so it is validated even
    // though it arrived from a process of ours: the holder was told this id by a
    // *previous* daemon, and this one has no other record of it.
    //
    // The path check is what pins the id to the socket it was found at. Without
    // it the registry could be keyed by one id while the holder answers for
    // another, and a later attach to the real id would start a second shell in
    // the same directory.
    if !valid_session_id(&id) || socket_for(&id) != path {
        // Not hung up and not unlinked: a holder answering for another session is
        // one we know nothing about, and it may have a live shell in it. Loud,
        // because it means either a digest collision or a hand-planted socket.
        warn!(
            "ignoring holder at {path:?}: it answers for session {id:?}, which does not belong there"
        );
        return false;
    }

    let mut sessions = SESSIONS.lock().await;
    if sessions.contains_key(&id) {
        // Adoption runs before the router serves traffic, so this is a
        // duplicate socket rather than a race — but registering twice would
        // orphan the first holder's pumps.
        discard_holder(attached, "session id is already registered").await;
        return false;
    }
    let session = register(&mut sessions, attached, slot);
    info!(
        session = %session.id,
        worktree = %session.label,
        pid = session.pid,
        exited = ?session.exited(),
        "adopted a terminal session"
    );
    true
}

/// Drain the holder connection for the session's whole life, feeding the mirror
/// and any attached socket, then record the shell's exit status.
async fn pump_holder(session: Arc<Session>, mut reader: OwnedReadHalf) {
    let pid = session.pid;
    loop {
        let frame = match wire::read_frame(&mut reader).await {
            Ok(Some(frame)) => frame,
            // The holder closed, or died. Either way there is nothing left to
            // read; whether that was orderly is decided below by whether an
            // exit code arrived first.
            Ok(None) => break,
            Err(e) => {
                warn!(session = %session.id, "holder connection failed: {e}");
                break;
            }
        };
        match frame.kind {
            wire::OUTPUT => {
                let chunk = Bytes::from(frame.payload);
                // Recorded and broadcast under ONE lock. Released in between, a
                // chunk could land in the mirror, be picked up by an attach
                // snapshotting it, and then also arrive live on that attach's
                // subscription — rendered twice, possibly splitting an escape
                // sequence. `serve_socket` takes the same lock across
                // subscribe+snapshot, so the two are ordered against each other.
                let mut sb = session.scrollback.lock().expect("scrollback poisoned");
                sb.push(&chunk);
                // Errors here mean nothing is attached, which is normal.
                let _ = session.output.send(chunk);
                drop(sb);
            }
            wire::EXIT => {
                let Some(code) = wire::decode_exit(&frame.payload) else {
                    warn!(session = %session.id, "ignoring a malformed exit frame");
                    continue;
                };
                // The holder sends its exit notice as output before this, so the
                // ordering the client depends on is already on the wire.
                //
                // `send_replace`, not `send`: a shell that exits while nothing is
                // attached has no receivers, and `send` would drop the code on
                // the floor — leaving a reattach to present a dead shell as a
                // live prompt and the reaper to apply the wrong grace.
                session.exit.send_replace(Some(code));
                debug!(session = %session.id, pid, code, "terminal shell exited");
                return;
            }
            other if frame.is_ignorable() => {
                debug!(session = %session.id, "ignoring holder frame {other:#x}");
            }
            other => {
                warn!(session = %session.id, "holder sent an unexpected frame {other:#x}");
                break;
            }
        }
    }

    if session.exited().is_none() {
        if *session.closing.borrow() {
            // The connection ended because *we* asked the holder to hang up, and
            // the writer closed behind the request. Nothing went wrong and there
            // is nobody to tell — the session left the registry before the
            // hangup was sent — but the exit is still published so any socket
            // still attached stops presenting a dead shell as a live prompt.
            debug!(session = %session.id, pid, "holder closed after a hangup");
            session.exit.send_replace(Some(1));
            return;
        }
        // The holder went away with the shell still running: it was killed, or
        // it crashed. The shell is gone with it (the master died with the
        // holder), so this is an exit like any other — reported, rather than
        // left as a live-looking prompt that swallows keystrokes.
        let notice = Bytes::from(
            "\r\n\x1b[2m[veld] the terminal's holder process went away\x1b[0m\r\n"
                .as_bytes()
                .to_vec(),
        );
        let mut sb = session.scrollback.lock().expect("scrollback poisoned");
        sb.push(&notice);
        let _ = session.output.send(notice);
        drop(sb);
        warn!(session = %session.id, pid, "terminal holder disappeared");
        session.exit.send_replace(Some(1));
    }
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
    resize_session(&session, size.cols, size.rows).await;

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

/// Ask the holder to resize the PTY.
///
/// Clamped here as well as in the holder: this is the edge a client's numbers
/// arrive at, and the holder must be able to trust its own peer no more than the
/// daemon trusts a browser.
async fn resize_session(session: &Session, cols: u16, rows: u16) {
    let payload = wire::encode_size(
        clamp_dimension(Some(cols), 80),
        clamp_dimension(Some(rows), 24),
    );
    if session
        .to_holder
        .send((wire::RESIZE, payload.to_vec()))
        .await
        .is_err()
    {
        debug!(session = %session.id, "resize dropped: the holder is gone");
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
                // Awaiting the queue is the backpressure that used to come from
                // waiting on the PTY's writability: a shell that stops reading
                // must slow this socket down, not buffer without bound.
                if session
                    .to_holder
                    .send((wire::INPUT, data.to_vec()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Message::Text(text) => match serde_json::from_str::<ClientControl>(&text) {
                Ok(ClientControl::Resize { cols, rows }) => {
                    resize_session(&session, cols, rows).await
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A holder speaking a version this build does not know must be refused —
    /// **and** told to hang up, or its shell would outlive every daemon that will
    /// ever refuse it identically.
    ///
    /// The fake holder here is hand-written on purpose: it is the only test that
    /// speaks the wire without going through our own encoder, which is what makes
    /// it a check of the *contract* rather than of a round-trip. It also documents
    /// that `HANGUP` is five bytes any implementation can produce.
    ///
    /// It covers the daemon's half only — that the refusal is *sent*. A real holder
    /// cannot be made to speak another version (the constant is compiled in), so
    /// the other half is
    /// `holder::tests::a_hangup_written_by_a_peer_that_closes_still_ends_the_shell`,
    /// which pins that a holder acts on a hangup from a peer that never reads its
    /// greeting. The two compose into the guarantee; neither alone is it.
    #[tokio::test]
    async fn a_holder_from_an_unknown_protocol_is_refused_and_hung_up() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("0000000000000001.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();

        let served = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let hello = serde_json::json!({
                "protocol": wire::PROTOCOL + 41,
                "session_id": "fromthefuture",
                "worktree_id": 1,
                "label": "future",
                "cwd": "/tmp",
                "pid": 1,
                "exited": null,
            });
            wire::write_frame(
                &mut stream,
                wire::HELLO,
                &serde_json::to_vec(&hello).unwrap(),
            )
            .await
            .unwrap();
            // A scrollback larger than the socket buffer (~8 KiB on macOS), which
            // is what a real holder sends and what the first version of this test
            // omitted. Without it the refusal path looked correct: the daemon wrote
            // HANGUP and closed while a real holder would still have been blocked
            // writing *this*, so the hangup went unread and the shell lived on. A
            // session's ring is 256 KiB, so anything smaller here re-hides it.
            let _ = wire::write_frame(&mut stream, wire::SCROLLBACK, &vec![b'x'; 64 * 1024]).await;
            // Whatever the daemon says next is what this test is about.
            wire::read_frame(&mut stream).await.unwrap()
        });

        let err = match connect_holder(&socket).await {
            Ok(_) => panic!("a future protocol must not be accepted"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("protocol"),
            "the log line must name the reason: {err}"
        );
        // Not "unanswered": there is a live holder here, so `adopt_one` must leave
        // its socket in place for a later build that can speak to it.
        assert!(!is_unanswered(&err));

        let reply = served.await.unwrap().expect("the daemon must answer");
        assert_eq!(
            reply.kind,
            wire::HANGUP,
            "a refused holder must be told to end its shell"
        );
        assert!(reply.payload.is_empty(), "HANGUP carries no payload");
    }

    #[test]
    fn only_names_this_daemon_could_have_written_are_adoption_candidates() {
        // The gate on a destructive path: `adopt_one` removes a socket nobody
        // answers, and `VELD_PTY_DIR` is a plain env var — pointed one level up at
        // `~/.veld`, an ungated boot would delete `daemon.sock`.
        assert!(is_holder_socket_name(FsPath::new(
            "/x/0123456789abcdef.sock"
        )));
        assert!(!is_holder_socket_name(FsPath::new("/x/daemon.sock")));
        assert!(!is_holder_socket_name(FsPath::new(
            "/x/0123456789abcde.sock"
        )));
        assert!(!is_holder_socket_name(FsPath::new(
            "/x/0123456789abcdef0.sock"
        )));
        // Uppercase is not what `{:016x}` emits, so it is not ours.
        assert!(!is_holder_socket_name(FsPath::new(
            "/x/0123456789ABCDEF.sock"
        )));
        assert!(!is_holder_socket_name(FsPath::new("/x/0123456789abcdef")));
        assert!(!is_holder_socket_name(FsPath::new(
            "/x/0123456789abcdefg.sock"
        )));
        // The real thing must pass the gate it is checked against.
        assert!(is_holder_socket_name(&socket_for("some-session-id")));
    }

    #[tokio::test]
    async fn only_an_unanswered_path_is_treated_as_stale() {
        /// `expect_err` would need `Debug` on `Attached`, and a derived one would
        /// print a scrollback full of terminal output — the hazard `Frame`'s hand
        /// written `Debug` exists to avoid.
        async fn connect_err(path: &FsPath) -> anyhow::Error {
            match connect_holder(path).await {
                Ok(_) => panic!("expected {path:?} to fail"),
                Err(e) => e,
            }
        }
        let dir = tempfile::tempdir().unwrap();

        // A path that does not exist.
        let missing = dir.path().join("missing.sock");
        assert!(is_unanswered(&connect_err(&missing).await));

        // A plain file: not a socket at all. `io::ErrorKind` has no variant for
        // this, which is why the check is on `errno`.
        let plain = dir.path().join("plain.sock");
        std::fs::write(&plain, b"").unwrap();
        assert!(is_unanswered(&connect_err(&plain).await));

        // A socket file whose listener is gone.
        let dead = dir.path().join("dead.sock");
        drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
        assert!(is_unanswered(&connect_err(&dead).await));

        // A listener that accepts and then says nothing: alive, and its socket
        // must NOT be removed — unlinking it would strand a live shell forever.
        let mute = dir.path().join("mute.sock");
        let listener = tokio::net::UnixListener::bind(&mute).unwrap();
        let _accepting = tokio::spawn(async move {
            let _held = listener.accept().await;
            std::future::pending::<()>().await;
        });
        let e = connect_err(&mute).await;
        assert!(
            !is_unanswered(&e),
            "a holder that accepts but does not greet is alive: {e}"
        );
    }

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
