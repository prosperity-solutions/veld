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
//! daemon at `veld.localhost` (`crates/veld-helper/src/caddy.rs`) — over HTTP
//! as well as HTTPS, since one Caddy server block owns both listeners. See
//! [`management_ports`] for what that means for the allowlist.
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

#[path = "pty/shims.rs"]
mod shims;

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
use tokio::sync::{broadcast, mpsc, oneshot, watch};
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

/// Out-of-band control frames buffered for the attached socket.
///
/// Small on purpose: these are user-initiated (a click, or a process opening a
/// URL), so a backlog of sixteen means sixteen browser panes are already opening
/// and the seventeenth is not the problem to solve.
const CONTROL_CHANNEL: usize = 16;

/// Grace between the shell exiting and the socket closing, so the last of its
/// output is forwarded instead of truncated.
const EXIT_DRAIN: Duration = Duration::from_millis(250);

/// Grace between hanging up the terminal's process group and killing it.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// How long a busy query waits for the holder to answer.
///
/// Long enough that a loaded machine cannot miss it (the reply is one frame on
/// a local socket), short enough that a wedged holder does not stall the close
/// gesture. On timeout the caller treats the terminal as idle.
const BUSY_QUERY_TIMEOUT: Duration = Duration::from_millis(1000);

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
/// stronger gate.
///
/// **A new route gets no gate for free.** Which gate it needs depends on its
/// method, and the distinction matters because getting it backwards fails in
/// opposite directions:
///
/// - **A mutating route must call [`check_csrf`] explicitly**, as `mint_ticket`
///   and `close_session` do. A blanket layer would not have helped — the one
///   route that most needs a gate is a GET.
/// - **A safe route's gate is the absent CORS layer.** The daemon sends no
///   `Access-Control-Allow-Origin`, so another origin can issue the request but
///   never read the answer. `list_pane_sessions` relies on exactly this, and
///   `check_csrf` is the *wrong* gate for it: the UI sends `X-Veld-Request` only
///   on mutations, so requiring it would break the call. A safe route must
///   therefore also stay genuinely safe — if it ever grows a side effect, it
///   needs the header and the client needs to send it.
pub fn routes() -> Router {
    Router::new()
        .route("/api/pty/tickets", post(mint_ticket))
        .route("/api/pty/attach", get(attach))
        .route("/api/pty/panes/{worktree_id}", get(list_pane_sessions))
        .route("/api/pty/sessions/{id}/busy", get(session_busy_status))
        .route("/api/pty/sessions/{id}", delete(close_session))
        .route("/api/pty/sessions/{id}/open-url", post(open_url))
}

/// Write the shim directory a terminal's `$BROWSER` points into.
///
/// At startup rather than lazily on the first terminal, for two reasons: a machine
/// where no `veld` CLI resolves (`veld_core::paths::cli_for_exe`) logs the warning
/// where somebody is looking, and `$VELD_SHIM_DIR` (the documented opt-in for
/// `open`/`xdg-open`) is live for the very first shell instead of the second. Three
/// small files.
///
/// Idempotent, and deliberately not fatal: everything else about a terminal works
/// without it.
pub fn prepare_shims() {
    let _ = shims::dir();
}

/// Remove shim scripts no `veld` CLI backs any more.
///
/// Separate from [`prepare_shims`] and called **after the daemon has bound its port**
/// — see the call site and [`shims::clear_unbacked`]. Writing the scripts early is
/// harmless; deleting them is only safe once this process has proved it owns this
/// instance's state.
pub fn clear_unbacked_shims() {
    shims::clear_unbacked();
}

/// Which of a worktree's config-declared panes have something to resume.
///
/// The UI needs this to label a restored pane's button — "Resume Claude" when
/// the tool has a conversation waiting, "Start Claude" when it does not — and
/// the answer lives in the database, not in the browser storage the layout came
/// from. One request per worktree rather than one per pane, and never the token:
/// the client has no use for it.
async fn list_pane_sessions(
    Path(worktree_id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
    let rows = db.resumable_panes(worktree_id).map_err(|e| {
        warn!("pane sessions: database error: {e}");
        err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
    })?;
    let resumable: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(session_id, spec_id)| serde_json::json!({ "session_id": session_id, "pane": spec_id }))
        .collect();
    Ok(Json(serde_json::json!({ "resumable": resumable })))
}

/// Start the background task that collects sessions nobody came back for.
///
/// Separate from [`routes`] so that building a router in a test doesn't leave a
/// timer running; the daemon calls this once at startup.
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
    /// Out-of-band control frames for the attached socket — today only
    /// [`ServerControl::OpenUrl`].
    ///
    /// A `broadcast` rather than a `watch` for the reason the `exit` field's comment
    /// spells out from the other side: `watch` keeps one *current value*, and two
    /// URLs opened in quick succession would collapse into one. It also gives the
    /// sender a receiver count, which is how the endpoint answers "is anyone
    /// looking?" — the caller needs that to fall back to the system browser instead
    /// of dropping the URL on the floor.
    control: broadcast::Sender<ServerControl>,
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
    /// Set when this daemon has given the session up **without ending it** —
    /// see [`release_session`].
    ///
    /// Deliberately not the attach epoch, which would be the obvious way to make
    /// attached sockets leave: an epoch change means *takeover* on the wire
    /// ([`ServerControl::TakenOver`]), and the UI renders that as a terminal that
    /// ended — "opened in another window", with Restart as the only button, which
    /// deletes the very shell this path exists to keep alive. A socket that ends
    /// on this signal instead closes with no control frame at all, which the
    /// client already reads as a dropped connection and answers with Reconnect —
    /// the truthful affordance, and the one that reaches the live holder again
    /// through [`obtain_session`].
    released: watch::Sender<bool>,
    /// The shell's pid, for log lines only — it is a pid in *another* process's
    /// child list, and nothing here may signal it.
    pid: i32,
    /// Whether the holder answers a [`wire::QUERY_BUSY`]. Held so the daemon
    /// never sends that frame to an older holder that would drop the connection
    /// rather than ignore it.
    busy_supported: bool,
    /// Slot for one in-flight busy query: the handler stores a oneshot sender
    /// here before sending [`wire::QUERY_BUSY`], and [`pump_holder`] completes it
    /// with the [`wire::BUSY`] reply. Serialised by [`Session::busy_lock`], so
    /// there is never more than one outstanding.
    busy_query: Mutex<Option<oneshot::Sender<bool>>>,
    /// Serialises busy queries. An `async` mutex because a query awaits the
    /// holder's reply while holding it.
    busy_lock: tokio::sync::Mutex<()>,
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

/// Sessions this daemon gave up while their shells kept running
/// ([`release_session`]), and the worktree each belongs to.
///
/// The registry is otherwise the only record that a session exists, and
/// `mint_ticket` reads it to decide whether an attach is a *resume* — which is
/// what exempts it from the rules that govern starting a shell (no new terminal
/// in a trashed worktree, the directory must exist, capacity, resolving a pane's
/// command). A released session is a live shell with no registry entry, so
/// without this it would be gated as a fresh spawn and could be refused a
/// reattach the trash check explicitly promises to allow.
///
/// A tombstone rather than "does the holder socket still exist": the file is
/// evidence of a *holder*, not of this session, and a leftover one from a killed
/// holder would exempt a genuinely fresh spawn from those same rules. This
/// carries the worktree id for the same reason the registry's entry does.
///
/// Entries leave when the session is registered again ([`register`]), when its
/// shell is ended ([`hang_up_released_holder`]), and — the one that matters for
/// correctness — when [`released_worktree`] reads one whose holder socket is
/// gone. `release_session`'s own sweep is a bound on entries nobody ever reads
/// back, not the thing that keeps them true.
static RELEASED: LazyLock<Mutex<HashMap<String, i64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The worktree a released session belongs to, if this daemon released one and
/// a holder could still be serving it.
///
/// The socket is checked as a **necessary** condition, never a sufficient one,
/// and the difference is the whole reason this map exists: a socket file is
/// evidence of some holder, not of *this* session, so a leftover one must not
/// make a fresh spawn look like a resume — but its absence is proof the holder
/// this record describes is gone (a holder unlinks its socket on the way out).
/// Pruning here rather than only in `release_session` is what keeps a record from
/// outliving the shell it stands for: a released session whose shell then exits
/// on its own is a case nothing else observes, because this daemon gave up the
/// link that would have told it — that is what "released" means.
///
/// **Do not "improve" this into a `holder_is_alive` handshake.** It was written
/// that way for one round and reverted: the greeting a holder sends a peer that
/// connects after its shell has exited *contains the exit code*, and delivering
/// it is what ends the holder ([`holder::deliver_exit`]). A probe from here is
/// issued by the handler that runs immediately before the attach which would have
/// adopted that holder — so the probe consumed the post-mortem, the holder exited,
/// and the reattach found nothing and started a fresh shell, losing the scrollback
/// the release existed to preserve. The residual it was meant to fix is far
/// smaller: a `SIGKILL`ed holder leaves its socket, which reads as a resume and
/// skips the trash and cwd gates for a genuinely fresh spawn. A terminal in a
/// trashed worktree beats a silently discarded one.
fn released_worktree(id: &str) -> Option<i64> {
    let mut released = RELEASED.lock().expect("released set poisoned");
    let worktree_id = *released.get(id)?;
    if !socket_for(id).exists() {
        released.remove(id);
        return None;
    }
    Some(worktree_id)
}

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
        // Not in the registry is not the same as not running: [`release_session`]
        // gives a live shell up while its holder keeps serving it. Without this,
        // closing such a pane returned "closed" having done nothing, and the shell
        // — with whatever agent or dev server is inside it — ran on until the
        // holder's orphan grace, half an hour after the user asked for it to stop.
        return hang_up_released_holder(id, reason).await;
    };
    info!(session = %session.id, worktree = %session.label, reason, "terminal session ended");
    // `send_replace`, not `send`: the writer task is the only receiver and may
    // already have finished (a shell that exited on its own), in which case
    // `send` would drop the flag and there is nothing left to hang up anyway.
    session.closing.send_replace(true);
    true
}

/// End a session this daemon no longer has a registry entry for, by asking its
/// holder directly. `true` if a holder answered for that id and was told to stop.
///
/// The only way to a live shell that is absent from the registry, which
/// [`release_session`] makes reachable. Two things keep it from ending somebody
/// else's shell: the socket is derived from the id, and the greeting must claim
/// that same id — the check `adopt_one` makes for the same reason.
async fn hang_up_released_holder(id: &str, reason: &str) -> bool {
    if !valid_session_id(id) {
        return false;
    }
    let attached = match connect_holder(&socket_for(id)).await {
        Ok(attached) => attached,
        Err(e) => {
            // Nothing is behind the socket: the shell this record stood for is
            // gone, so the record goes too. Every *other* failure leaves it alone
            // — this connect is single-shot, and a live holder parks its own loop
            // for longer than a handshake gets (see `holder_is_alive`). Dropping
            // the record there would classify the next attach as a fresh spawn and
            // let the trash and cwd gates refuse a shell that is still running.
            if is_unanswered(&e) {
                RELEASED.lock().expect("released set poisoned").remove(id);
            }
            return false;
        }
    };
    if attached.hello.session_id != id {
        // A digest collision, or a hand-planted socket. Not ours to hang up, and
        // not ours to forget either.
        warn!("not ending {id:?}: the holder at its socket answers for another session");
        return false;
    }
    info!(session = %id, pid = attached.hello.pid, reason, "ending a released terminal session");
    discard_holder(attached, reason).await;
    RELEASED.lock().expect("released set poisoned").remove(id);
    true
}

/// Ask the holder whether a foreground job is running in this session.
///
/// This is the "is something running?" signal a real terminal uses — the
/// foreground process group differs from the shell's while a command executes,
/// and the holder reads it with `tcgetpgrp` on the master. It is *derived on
/// demand* from the live process, never stored: a foreground job starts and
/// stops constantly, so a persisted value would be stale the moment it was
/// written, and the future sidebar rail just calls the same endpoint.
///
/// `None` when the answer cannot be learned: the session is gone, the holder is
/// an older build that does not speak `QUERY_BUSY`, or it did not answer in
/// time. Callers treat `None` as *idle* — never block a close on a terminal we
/// cannot read.
async fn session_busy(session: Arc<Session>) -> Option<bool> {
    if !session.busy_supported || session.exited().is_some() {
        return None;
    }
    // One in flight at a time: a reply is matched to a sender, so two
    // simultaneous queries would race for the slot.
    let _guard = session.busy_lock.lock().await;
    let (tx, rx) = oneshot::channel();
    *session.busy_query.lock().expect("busy query poisoned") = Some(tx);
    if session
        .to_holder
        .send((wire::QUERY_BUSY, Vec::new()))
        .await
        .is_err()
    {
        return None;
    }
    tokio::time::timeout(BUSY_QUERY_TIMEOUT, rx)
        .await
        .ok()
        .and_then(|r| r.ok())
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

// The predicate that decides whether a path has the shape `socket_for` produces
// — the gate on anything destructive in `adopt_existing_sessions` — lives in
// `veld_core::instance`, together with the `holder_sockets_in` scan built on it,
// because `veld doctor` and `veld uninstall` walk the same directory and have to
// apply the same rule. Doctor did not, and connected to anything ending in
// `.sock`.

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
/// Three callers, and what they have in common is the rule: **never call this on
/// a holder whose greeting has not been checked against the session id it is
/// supposed to be serving** — one that belongs to another session may have a live
/// shell in it, and this frame ends whatever it reaches.
///
/// - a holder this daemon started and then decided against (dropping the
///   connection alone would leave its shell running until the orphan grace);
/// - one it adopted and then decided against, same reason;
/// - one serving a session this daemon has *released*
///   ([`hang_up_released_holder`]), where the check is that function's own
///   `hello.session_id` comparison rather than [`verify_identity`] — there is no
///   [`HolderConfig`] to compare against, because nothing here spawned anything.
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
    /// What to run instead of a login shell, for a config-declared pane.
    /// Resolved here, from the project's own config, so the client never gets
    /// to say what the daemon executes.
    pane: Option<PaneLaunch>,
    /// `terminal.openUrlsInApp`, read at the same moment and for the same reason as
    /// the field below. It gates the session's whole environment: with the feature
    /// off, veld puts nothing in the shell.
    open_urls_in_app: bool,
    /// `terminal.interceptSystemOpen`, read while [`mint_ticket`] already had the
    /// database open. It rides the ticket rather than being read at spawn time so
    /// that nothing puts a `Db::open()` on the session-spawn path — see the comment
    /// where it is read, and the AGENTS.md note it points at.
    intercept_system_open: bool,
    expires_at: Instant,
}

/// A resolved config-declared pane launch, ready to hand to a holder.
struct PaneLaunch {
    /// The `ide.panes[].id` this came from.
    spec_id: String,
    /// The pane's display label, so the holder's exit notice can name what
    /// actually ran instead of claiming a shell exited.
    label: String,
    /// The interpolated command. A `shell` spec is already wrapped as
    /// `["sh", "-c", …]` here so the holder only ever deals with an argv.
    argv: Vec<String>,
    env: Vec<(String, String)>,
    /// The token this launch runs under, and whether it is new.
    ///
    /// A fresh launch's token is recorded only once the holder is up (see
    /// [`obtain_session`]); a resumed one is already in the database, so there
    /// is nothing to write.
    token: String,
    fresh: bool,
}

/// Which command a config-declared pane should run.
///
/// Request-only: the response does not echo it back. The client already knows
/// what it asked for, and derives everything it renders from that plus
/// `resumed` — an echoed field with no reader is a contract nobody honours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PaneMode {
    /// Start the tool from scratch under a newly minted token.
    Fresh,
    /// Re-run the pane's `resume` command under the token it launched with.
    Resume,
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
    /// The `ide.panes[].id` this pane was created from, for a config-declared
    /// pane. Absent for an ordinary terminal, which runs a login shell.
    ///
    /// Only ever a *name*: the command it resolves to comes from the project's
    /// config, read here. A client that could post a command would be a client
    /// that could make the daemon run anything.
    #[serde(default)]
    pane: Option<String>,
    /// Which of the pane's two commands to run. Ignored without `pane`, and
    /// ignored when the session is already live — there is nothing to spawn.
    #[serde(default)]
    mode: Option<PaneMode>,
}

#[derive(Serialize)]
struct TicketResponse {
    ticket: String,
    expires_in_ms: u64,
    /// True when a live session with this id is waiting — the client uses it to
    /// distinguish "your shell is still here" from "starting a new one".
    resumed: bool,
}

/// Resolve a config-declared pane into the command a holder should run.
///
/// Everything the client supplied is a *name*; the command comes from the
/// project's own `veld.json`, read here from the worktree the ticket already
/// resolved. That is the same boundary the actions API keeps — the daemon never
/// executes a command a request body contained.
async fn resolve_pane(
    db: &veld_core::db::Db,
    spec_id: &str,
    mode: PaneMode,
    worktree_id: i64,
    session_id: &str,
    worktree_path: &FsPath,
    branch: &str,
) -> Result<PaneLaunch, ApiError> {
    // `root_config_in`, not `discover_config`: the worktree *is* the project
    // root here, and walking upward would find a parent repo's config and offer
    // its panes in a checkout that never declared them. Both legal filenames are
    // handled, which is the other half of why this helper exists.
    let config_path = veld_core::config::root_config_in(worktree_path).ok_or_else(|| {
        err(
            StatusCode::CONFLICT,
            "this worktree has no veld.json, so it declares no panes",
        )
    })?;
    let config = veld_core::config::parse_config(&config_path).map_err(|e| {
        err(
            StatusCode::CONFLICT,
            format!("this worktree's veld.json could not be read: {e}"),
        )
    })?;
    let section = config.ide_section();
    let pane = section.pane(spec_id).ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            format!("this project declares no pane called {spec_id:?}"),
        )
    })?;

    let path = veld_core::user_path::cached_user_path().await;
    // Re-checked here and not only in the menu: the config can change, or a tool
    // can be uninstalled, between the pane being offered and being clicked.
    if let Some(missing) = pane
        .requires_bin
        .iter()
        .find(|bin| which_on_path(bin, &path).is_none())
    {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "{missing} is not installed, so the {} pane cannot start",
                pane.label
            ),
        ));
    }

    let veld_core::ide::PaneBody::Terminal(terminal) = &pane.body;
    let (spec, token, fresh) = match mode {
        PaneMode::Fresh => (&terminal.launch, veld_core::db::mint_pane_token(), true),
        PaneMode::Resume => {
            let resume = terminal.resume.as_ref().ok_or_else(|| {
                err(
                    StatusCode::CONFLICT,
                    format!("the {} pane has no resume command", pane.label),
                )
            })?;
            // No token means this pane never launched, so there is nothing for
            // the tool to resume. Falling back to a fresh launch here is the one
            // thing this must not do: it would silently start a new billable
            // conversation and read to the user as the old one being lost.
            let recorded = db
                .pane_session(session_id)
                .map_err(|e| {
                    warn!("pty ticket: could not read the pane session: {e}");
                    err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
                })?
                .filter(|s| s.worktree_id == worktree_id && s.spec_id == spec_id)
                .ok_or_else(|| {
                    err(
                        StatusCode::CONFLICT,
                        "this pane has nothing to resume — start it fresh",
                    )
                })?;
            (resume, recorded.token, false)
        }
    };

    let ctx = pane_context(pane, &token, worktree_path, branch, &config);
    let resolved = spec.interpolate(&ctx).map_err(|e| {
        err(
            StatusCode::CONFLICT,
            format!(
                "the {} pane's command could not be resolved: {e}",
                pane.label
            ),
        )
    })?;
    // A pane runs inside the user's **login+interactive** shell — the exact
    // shell a real terminal opens — rather than being spawned directly. That is
    // what makes a pane inherit the whole environment a terminal gives: the
    // `.zprofile`/`.zshrc` exports (model tokens, tool paths, `JAVA_HOME`) that
    // a directly-spawned `argv` on the daemon's bare service environment never
    // saw. The shell runs the command and exits with its status, so
    // `close_on_exit` and exit reporting are unchanged.
    let shell = holder::login_shell();
    let argv = match resolved {
        veld_core::config::CommandSpec::Argv(argv) => {
            if argv.is_empty() || argv[0].is_empty() {
                return Err(err(
                    StatusCode::CONFLICT,
                    format!("the {} pane's command is empty", pane.label),
                ));
            }
            login_shell_command(&shell, &argv)
        }
        // A `shell` spec is already a command string, so it is handed to the
        // login shell as-is — no re-quoting, and `shell` keeps meaning exactly
        // what it means for every other command position in the config. It just
        // runs under the user's shell now, like a terminal command, instead of
        // a bare `sh` with no rc files.
        veld_core::config::CommandSpec::Shell(script) => {
            vec![
                shell,
                "-l".to_owned(),
                "-i".to_owned(),
                "-c".to_owned(),
                script,
            ]
        }
    };

    Ok(PaneLaunch {
        spec_id: spec_id.to_owned(),
        label: pane.label.clone(),
        argv,
        env: vec![
            // A floor, not the answer: the login shell computes the user's own
            // PATH (and the rc files typically refine it), but a shell with no
            // rc files would otherwise inherit the daemon's bare service PATH.
            // `VELD_PANE_*` are this pane's own and must win over the shim env.
            ("PATH".to_owned(), path),
            ("VELD_PANE_ID".to_owned(), pane.id.clone()),
            ("VELD_PANE_TOKEN".to_owned(), token.clone()),
        ],
        token,
        fresh,
    })
}

/// Wrap an `argv` pane command for the user's login shell:
/// `$SHELL -l -i -c '<each argv element single-quoted>'`.
///
/// `-l -i` are the flags a terminal's shell runs with, so `.zprofile` *and*
/// `.zshrc` both load — the `-l -i -c 'command env'` shape `resolve_user_path`
/// relies on for the same reason. Each argument is re-quoted with
/// [`veld_core::console::quote`] so spaces, `$`, backticks or a single quote in
/// a value can never become a second command.
fn login_shell_command(shell: &str, argv: &[String]) -> Vec<String> {
    let command = argv
        .iter()
        .map(|arg| veld_core::console::quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        shell.to_owned(),
        "-l".to_owned(),
        "-i".to_owned(),
        "-c".to_owned(),
        command,
    ]
}

/// The variables a pane command may interpolate.
///
/// Deliberately small — a pane has no run, no node and no ports, so most of the
/// `${veld.*}` family would resolve to nothing here. The names must be exactly
/// `veld_core::ide::PANE_BUILTINS`, which is what `veld lint` accepts: a name
/// this populates but lint rejects is unreachable, and a name lint accepts but
/// this omits produces a pane that passes lint and then dies at spawn with
/// "command could not be resolved". `pane_context_populates_every_lintable_name`
/// is what actually holds the two together.
fn pane_context(
    pane: &veld_core::ide::PaneDef,
    token: &str,
    worktree_path: &FsPath,
    branch: &str,
    config: &veld_core::config::VeldConfig,
) -> veld_core::variables::VariableContext {
    let mut builtins = HashMap::new();
    builtins.insert("pane.id".to_owned(), pane.id.clone());
    builtins.insert("pane.label".to_owned(), pane.label.clone());
    builtins.insert("pane.token".to_owned(), token.to_owned());
    // **The same meanings these names have everywhere else.** `${veld.worktree}`
    // is the slugified directory *name* and `${veld.branch}` is slugified too
    // (`orchestrator.rs`'s `BuiltinScope::apply`, and the reference table in
    // docs/configuration.md); `${veld.root}` is the path. Redefining `worktree`
    // as a path here — which an earlier revision did, because a pane command
    // wants a path far more often than a slug — made one name mean two things
    // depending on scope, and the next person to "fix" it toward the documented
    // meaning would silently break every `-C "${veld.worktree}"` command.
    //
    // Slugifying `branch` also closes the one value here an outsider chooses:
    // check out someone's pull-request branch and the name is theirs, so a
    // `shell` pane interpolating it raw would run whatever they named it.
    builtins.insert(
        "worktree".to_owned(),
        veld_core::url::slugify(
            &worktree_path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        ),
    );
    builtins.insert("root".to_owned(), worktree_path.display().to_string());

    builtins.insert("branch".to_owned(), veld_core::url::slugify(branch));
    builtins.insert("project".to_owned(), config.name.clone());
    builtins.insert(
        "username".to_owned(),
        veld_core::orchestrator::whoami_username(),
    );
    veld_core::variables::VariableContext {
        builtins,
        ..Default::default()
    }
}

/// How long a `requires_bin` answer is reused before the filesystem is asked
/// again.
///
/// The worktree listing is polled, so an uncached lookup would `stat` every
/// `PATH` entry for every declared pane of every worktree, several times a
/// minute. A minute of staleness costs at most one poll's delay before a
/// just-installed tool's pane appears, and the launch path re-checks anyway.
const BIN_CACHE_TTL: Duration = Duration::from_secs(60);

static BIN_CACHE: LazyLock<Mutex<HashMap<String, (bool, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Which of a pane's required executables are **not** installed, for the pane
/// menu.
///
/// Returns the names rather than a boolean because the menu has to say *which*
/// tool is missing: the pane's own id is not it (`claude-yolo` needs `claude`,
/// `git-log` needs `git`), and guessing produced tooltips like "Git log needs
/// git-log".
///
/// Optimistic when the user's `PATH` has not been resolved yet — a daemon that
/// has just started publishes it within about a second, and hiding a pane
/// because the answer is not in yet is worse than showing one whose launch then
/// explains itself. The launch path re-checks against the real `PATH` and is the
/// gate that actually decides.
pub(crate) fn missing_pane_binaries(required: &[String]) -> Vec<String> {
    if required.is_empty() {
        return Vec::new();
    }
    let Some(path) = veld_core::user_path::published_user_path() else {
        return Vec::new();
    };
    required
        .iter()
        .filter(|bin| !installed(bin, &path))
        .cloned()
        .collect()
}

/// One cached `PATH` lookup. Never spawns anything — see the caller.
fn installed(bin: &str, path: &str) -> bool {
    let now = Instant::now();
    if let Some((found, at)) = BIN_CACHE.lock().expect("bin cache poisoned").get(bin)
        && now.duration_since(*at) < BIN_CACHE_TTL
    {
        return *found;
    }
    let found = which_on_path(bin, path).is_some();
    BIN_CACHE
        .lock()
        .expect("bin cache poisoned")
        .insert(bin.to_owned(), (found, now));
    found
}

/// Whether `name` resolves to an executable file on `path`.
///
/// A plain lookup rather than spawning anything: deciding whether to *offer* a
/// pane must never run a config-declared command, and this is called often
/// enough (once per pane, per menu render) that a fork would be felt.
fn which_on_path(name: &str, path: &str) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &FsPath) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
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
    let registered = match SESSIONS.lock().await.get(&body.session_id) {
        Some(s) if s.worktree_id != body.worktree_id => {
            return Err(err(
                StatusCode::CONFLICT,
                "session belongs to another worktree",
            ));
        }
        Some(_) => true,
        None => false,
    };
    // Absent from the registry is not the same as gone. `release_session` hands a
    // live shell back to its holder without an entry, and everything below this
    // line applies a *fresh spawn's* rules — refusing a reattach into a trashed
    // worktree, or one whose directory has moved, which is exactly what the note
    // under the trash check promises not to do.
    let resumed = registered
        || match released_worktree(&body.session_id) {
            Some(worktree_id) if worktree_id != body.worktree_id => {
                // The same claim the registry arm above makes, and it has to be
                // made here too: a released session is still owned by the worktree
                // it was started in.
                return Err(err(
                    StatusCode::CONFLICT,
                    "session belongs to another worktree",
                ));
            }
            Some(_) => true,
            None => false,
        };

    // No NEW shells in a checkout that is in the trash. It is still a real directory
    // for the whole retention period, so nothing stops a URL or a direct API call
    // from opening a terminal in one — and nothing reaps sessions when a worktree is
    // deleted, so the eventual `git worktree remove` would pull the directory out
    // from under that shell with no warning.
    //
    // **Reattaching is deliberately still allowed.** A worktree can be binned from
    // another window while a terminal is open in it, and that shell keeps running for
    // the whole retention period — so refusing the reattach would strand a live
    // session behind a page reload and make the user restore the worktree just to
    // reach output that never went anywhere. Hence the check sits after `resumed` is
    // known rather than at the top of the handler.
    if !resumed && !wt.trashed_at.is_empty() {
        return Err(err(
            StatusCode::CONFLICT,
            "this worktree is in the trash — restore it to open a new terminal",
        ));
    }

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

    // Read here rather than at spawn time, because this handler already has the
    // database open. A `Db::open()` on the session-spawn path is the thing AGENTS.md
    // warns about — one reached that path before and twelve unrelated tests migrated
    // a real user database as a side effect.
    let open_urls_in_app = db.terminal_open_urls_in_app();
    let intercept_system_open = db.terminal_intercept_system_open();

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

    // Only a spawn needs a command. A reattach is already running whatever it
    // was started with, so resolving here would read the config, check PATH and
    // possibly refuse — for a session the answer cannot change.
    //
    // Keyed on `registered`, not on `resumed`: a *released* session is one this
    // daemon can no longer reach through the registry, and if its holder has since
    // died the attach below spawns rather than adopts. Without a resolved pane
    // that spawn is a bare login shell where the user's pane command should be —
    // silently, and in a pane whose whole identity is that command. Resolving one
    // we then do not use costs a config read; not resolving one we need replaces
    // the terminal.
    let pane = match (&body.pane, registered) {
        (Some(spec_id), false) => Some(
            resolve_pane(
                &db,
                spec_id,
                body.mode.unwrap_or(PaneMode::Fresh),
                body.worktree_id,
                &body.session_id,
                &cwd,
                &wt.branch,
            )
            .await?,
        ),
        _ => None,
    };
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
                pane,
                open_urls_in_app,
                intercept_system_open,
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

/// Report whether a terminal has a foreground job running.
///
/// Read-only — the UI calls this before offering to close a tab, and the future
/// sidebar rail will poll it. No CSRF gate, for the reason `list_pane_sessions`
/// documents: it is a safe GET, and the daemon sends no CORS headers, so another
/// origin can issue it but never read the answer.
///
/// A session the daemon cannot answer for (gone, or an old holder that does not
/// speak the query) reports `false`, never an error: the caller is deciding
/// whether to *ask* before closing, and an unknown answer must not block a
/// close.
async fn session_busy_status(Path(id): Path<String>) -> Result<Json<serde_json::Value>, ApiError> {
    if !valid_session_id(&id) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid session id"));
    }
    let session = SESSIONS.lock().await.get(&id).cloned();
    let Some(session) = session else {
        return Ok(Json(serde_json::json!({ "busy": false })));
    };
    let busy = session_busy(session).await.unwrap_or(false);
    Ok(Json(serde_json::json!({ "busy": busy })))
}

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
// Opening a URL from a terminal
// ---------------------------------------------------------------------------

/// Longest URL this endpoint accepts.
///
/// Well past any real login or preview link (8 KB is the de-facto limit browsers
/// and servers agree on for a request line) and short of putting a megabyte into a
/// WebSocket frame because a shell printed a file.
const MAX_URL_LEN: usize = 8 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenUrlRequest {
    url: String,
}

#[derive(Serialize)]
struct OpenUrlResponse {
    target: veld_core::ide::UrlTarget,
    /// Why, when the answer is [`UrlTarget::System`] for a reason the caller could
    /// not have worked out itself — no window attached, or the preference off. The
    /// CLI prints it, so "it opened in Safari again" has an answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// Route a URL produced by a terminal session: a Veld browser pane, or the system
/// browser.
///
/// **Both entry points come here.** A click on a link in the terminal and a process
/// in the shell invoking `$BROWSER` are the same question, and the answer depends on
/// the project's `veld.json` — which the renderer does not read — so the decision
/// lives on this side. See `veld_core::ide::route_url`.
///
/// The reply says what *will* happen, not what happened: for a pane, the frame is
/// already on the socket by the time this returns. A caller that gets
/// [`UrlTarget::System`] is expected to open the URL itself, which is why the answer
/// is a body rather than a status.
///
/// CSRF-gated. Without the gate, any page in a browser could push a URL of its
/// choosing into a Veld browser pane on the developer's machine.
async fn open_url(
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<OpenUrlRequest>,
) -> Result<Json<OpenUrlResponse>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    if !valid_session_id(&id) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid session id"));
    }
    if body.url.len() > MAX_URL_LEN {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "url is too long"));
    }
    // Parsed **once**, here, with the same standard the thing that will load it
    // implements — and it is the canonical serialisation that travels onward, never
    // the caller's spelling. Routing on one string and opening another is not a
    // tidiness point: `https://accounts.google.com\@evil.com` scans as `evil.com`
    // by eye and loads `accounts.google.com` in a browser, which silently sidesteps
    // the exempt list. See `veld_core::ide::parse_web_url`.
    //
    // Refused rather than answered with `system`: a caller sending a non-web URL has
    // a bug, and reporting "opened in the system browser" would hide it. The one
    // place a non-web argument is *expected* — a shim invoked as `open report.pdf` —
    // never gets this far, because `veld open-url` execs the real tool without asking.
    let Some(web) = veld_core::ide::parse_web_url(&body.url) else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "not an http(s) URL with a host",
        ));
    };

    let session = SESSIONS
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such terminal session"))?;

    // One `Db::open` per URL a human (or an agent) opens. Not a hot path in the
    // sense AGENTS.md warns about — this is user-initiated and rare — and the
    // alternative is caching a preference that must then be invalidated when the
    // settings screen writes it.
    let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
    let mut external = db.external_origins();
    external.extend(project_external_origins(&db, session.worktree_id));
    let open_in_app = db.terminal_open_urls_in_app();

    match veld_core::ide::route_url(web.canonical.as_str(), open_in_app, &external) {
        veld_core::ide::UrlTarget::System => Ok(Json(OpenUrlResponse {
            target: veld_core::ide::UrlTarget::System,
            reason: Some(
                if open_in_app {
                    "this origin is on the exempt list (browser.externalOrigins, or \
                     ide.externalOrigins in the project's config)"
                } else {
                    "terminal.openUrlsInApp is off"
                }
                .to_owned(),
            ),
        })),
        veld_core::ide::UrlTarget::Pane => {
            // Nobody is looking at this terminal — the page was closed, or the
            // session is between attaches. Answering `pane` would drop the URL: the
            // frame is not queued for a future attach, deliberately, because a login
            // page that arrives ten minutes late is worse than one that opened in the
            // wrong browser.
            //
            // The **detach clock**, not `control.receiver_count()`. A displaced socket
            // is still a subscriber until it notices the takeover, so the count can be
            // non-zero while every subscriber will discard the frame on the epoch check
            // in `serve_socket` — answering `pane` there drops the URL silently, which
            // is the exact failure this branch exists to prevent. `detached_since` is
            // `None` only while a socket owns the session, and `serve_socket`
            // subscribes before it claims the epoch, so `None` implies a live
            // subscriber.
            let attached = session
                .detached_since
                .lock()
                .expect("detach clock poisoned")
                .is_none();
            if !attached {
                return Ok(Json(OpenUrlResponse {
                    target: veld_core::ide::UrlTarget::System,
                    reason: Some(
                        "no Veld window is attached to this terminal right now".to_owned(),
                    ),
                }));
            }
            let _ = session
                .control
                .send(ServerControl::OpenUrl { url: web.canonical });
            debug!(session = %id, "routed a URL to a browser pane");
            Ok(Json(OpenUrlResponse {
                target: veld_core::ide::UrlTarget::Pane,
                reason: None,
            }))
        }
    }
}

/// The project half of the exempt list, for the worktree a session belongs to.
///
/// Read from the config on every call rather than cached, which is what makes
/// editing `ide.externalOrigins` take effect on the next URL instead of on the next
/// daemon restart. A worktree that has been removed, or has no config, contributes
/// nothing — an unreadable config must not turn into "everything is exempt" or into
/// a failed request.
fn project_external_origins(
    db: &veld_core::db::Db,
    worktree_id: i64,
) -> Vec<veld_core::ide::OriginPattern> {
    let Ok(Some(wt)) = db.get_worktree(worktree_id) else {
        return Vec::new();
    };
    // `root_config_in`, never a hardcoded root filename — a `veld.jsonc` project
    // is a project (AGENTS.md).
    let Some(path) = veld_core::config::root_config_in(FsPath::new(&wt.path)) else {
        return Vec::new();
    };
    match veld_core::config::parse_config(&path) {
        Ok(cfg) => cfg.ide_section().external_origins,
        Err(e) => {
            // `warn`, not `debug`: this is the one input to the routing decision that
            // can fail silently, and the failure sends a URL the project meant to
            // exempt into a pane instead. Degrading to "no project exemptions" rather
            // than to "everything is exempt" is deliberate — the alternative turns one
            // broken config into the feature not working at all — but it must be
            // visible, because nothing else about this URL says why it went where it did.
            warn!(
                "ide.externalOrigins ignored — config at {} does not load: {e}",
                path.display()
            );
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Origin allowlist
// ---------------------------------------------------------------------------

/// The ports the helper's Caddy is listening on, once it has told us.
///
/// **The allowlist cannot ask.** [`origin_allowed`] is synchronous and runs on an
/// upgrade; the helper is a unix-socket round trip. So the daemon learns the pair
/// on its own timer ([`track_helper_ports`]) and this is where the answer waits.
/// `None` until the first successful status — see [`dashboard_ports`] for what
/// stands in until then.
static HELPER_WEB_PORTS: std::sync::RwLock<Option<(u16, u16)>> = std::sync::RwLock::new(None);

/// Keep [`HELPER_WEB_PORTS`] current. Spawned once by the daemon at startup.
///
/// **Polls rather than reading once, because of the order `veld setup` does
/// things in.** It reinstalls (and so restarts) the daemon in step 4, installs the
/// helper in step 5, and writes the new mode to `setup.json` after that
/// (`veld/src/commands/setup/privileged.rs`) — so a fresh daemon's first look at
/// the world finds no helper and a mode file that still describes the *previous*
/// setup, or none at all. A value read once at boot would therefore be wrong for
/// the whole life of that daemon, and being wrong here means refusing every
/// upgrade the dashboard makes, silently.
///
/// What this does **not** need to cover, stated because the obvious reading is
/// that it does: nothing ships a way to move a machine's ports without restarting
/// this process. Both setup commands go through `install_daemon`. So the long
/// interval is a cheap re-check, and the short one is the half that matters.
pub async fn track_helper_ports() {
    // Faster while nothing is known (a cold boot races the helper's own start),
    // then slow: this is a status call on a unix socket, not free, and the answer
    // changes only when someone re-runs `veld setup`.
    const UNKNOWN: Duration = Duration::from_secs(5);
    const KNOWN: Duration = Duration::from_secs(60);
    loop {
        let mut learned = false;
        if let Ok(helper) = veld_core::helper::HelperClient::connect().await
            && let Ok(Some(ports)) = helper.web_ports().await
        {
            learned = true;
            let changed = *HELPER_WEB_PORTS.read().expect("helper ports poisoned") != Some(ports);
            if changed {
                info!(
                    https = ports.0,
                    http = ports.1,
                    "dashboard ports, from the helper"
                );
                *HELPER_WEB_PORTS.write().expect("helper ports poisoned") = Some(ports);
            }
        }
        tokio::time::sleep(if learned { KNOWN } else { UNKNOWN }).await;
    }
}

/// The HTTPS and HTTP ports the dashboard is served on.
///
/// The helper's answer when there is one, because the setup mode and the helper
/// **can disagree** — a helper started by hand, a `veld setup privileged` that
/// died after writing the mode file, or a stray user helper on the high pair while
/// the privileged LaunchDaemon is down, which `veld doctor` has a check for. The
/// mode is the fallback for the window before the first status lands, and for a
/// daemon running with no helper at all.
fn dashboard_ports() -> (u16, u16) {
    if let Some(ports) = *HELPER_WEB_PORTS.read().expect("helper ports poisoned") {
        return ports;
    }
    ports_for_mode(veld_core::setup::read_setup_mode().as_deref())
}

/// The pair a given setup mode puts in front.
///
/// Split out so the mapping is testable without a `~/.veld/setup.json` on the
/// machine running the test. Every failure to read that file — absent, mid-write,
/// unparseable, an unknown mode, a `HOME` that is not the user's — collapses to
/// `None` here and takes the privileged pair, which is the conservative answer:
/// those ports are root-only, so trusting an origin on one cannot hand anything to
/// a process that merely happens to be running.
fn ports_for_mode(mode: Option<&str>) -> (u16, u16) {
    match mode {
        // `veld setup unprivileged`, and the auto-bootstrap that writes
        // `{"mode":"auto"}` — both run Caddy on the high pair.
        Some("unprivileged") | Some("auto") => (
            veld_core::instance::UNPRIVILEGED_HTTPS_PORT,
            veld_core::instance::UNPRIVILEGED_HTTP_PORT,
        ),
        _ => (443, 80),
    }
}

/// The scheme/port pairs a management hostname may be reached at.
///
/// **HTTP as well as HTTPS, and that is not a concession.** One Caddy server block
/// listens on both ports (`veld-helper/src/caddy.rs`), which is how Caddy is told
/// not to install its automatic HTTP→HTTPS redirects — so `http://veld.localhost`
/// is a fully served surface, not a mistake, and a browser that lands there (Chrome
/// does not HTTPS-upgrade a loopback host you type) got a dashboard whose every
/// WebSocket was refused: 229 rejected upgrades in one session's daemon log, with
/// nothing on screen saying why, because a browser cannot read a failed
/// handshake's status.
///
/// **The plaintext half only on a root-only port.** Not because a plaintext origin
/// is weaker in itself — the name resolves on loopback, so nothing is on the path —
/// but because an origin is only as trustworthy as the port it names: a port in the
/// unprivileged range that Caddy is not currently bound to (before the helper
/// starts, during a `veld update`, after a crash) can be bound by any process on
/// the machine, which then serves a page holding an allowlisted origin and can
/// reverse-proxy `/api` to make [`mint_ticket`]'s header check same-origin.
///
/// The limit of that rule, since it is a number standing in for a property:
/// `port < 1024` means root-only on macOS, and on Linux it is one writable
/// `sysctl net.ipv4.ip_unprivileged_port_start` (or a `CAP_NET_BIND_SERVICE` on any
/// binary) away from meaning nothing. It is still the best test available here —
/// the helper reports which ports it is *configured* for, not which are bound — and
/// the window it leaves needs Caddy to be down on a machine whose sysctl has been
/// lowered.
///
/// This costs the no-sudo install a plaintext dashboard origin on 18080. Caddy does
/// serve the dashboard there, and nothing advertises it (`veld ui`, the share join
/// base, `veld doctor` and the setup tip all print `https://…:18443`), so nobody
/// arrives by typing a hostname — which was the whole reason the plaintext surface
/// mattered. A page that *does* arrive there is now told rather than left to
/// wonder: no control socket means the UI's channel banner
/// (`CHANNEL_DOWN_NOTICE_MS` in `ui/src/App.tsx`) says so on screen.
///
/// Stated plainly, because the first version of this comment overclaimed: the
/// `Origin` gate defends against a **browser-mediated** attacker only. A local
/// process can set any `Origin` it likes on a handcrafted upgrade, and
/// `mint_ticket`'s CSRF check is header-presence, not origin — so a local attacker
/// never needed a squatted port. What the narrowing removes is a page in the user's
/// *own browser* being handed the dashboard's authority.
fn management_ports() -> Vec<(&'static str, u16)> {
    let (https, http) = dashboard_ports();
    management_ports_from(https, http)
}

/// [`management_ports`] with the pair passed in — the whole of the rule, without
/// the helper or the filesystem.
fn management_ports_from(https: u16, http: u16) -> Vec<(&'static str, u16)> {
    let mut ports = vec![("https", https)];
    if http < 1024 {
        ports.push(("http", http));
    }
    ports
}

/// The origins one management hostname can legitimately be reached at.
fn management_origins(host: &str) -> Vec<String> {
    management_origins_for(host, &management_ports())
}

/// [`management_origins`] with the ports passed in, which is the whole of the
/// formatting rule and the half worth testing.
///
/// Exact pairs, never a cross product: nothing serves HTTPS on the HTTP port, so
/// `http://veld.localhost:18443` is not an origin any page can have and does not
/// belong on an allowlist. The default port for the scheme is omitted, because
/// that is how a browser serialises it.
fn management_origins_for(host: &str, ports: &[(&str, u16)]) -> Vec<String> {
    ports
        .iter()
        .map(|(scheme, port)| match (*scheme, *port) {
            ("https", 443) | ("http", 80) => format!("{scheme}://{host}"),
            _ => format!("{scheme}://{host}:{port}"),
        })
        .collect()
}

/// Origins allowed to open a terminal socket.
///
/// Built per request rather than cached: it is a handful of `format!`s on a
/// path that already spawns a process, and the daemon's instance identity
/// (port, management host) is read from the environment.
fn allowed_origins() -> Vec<String> {
    allowed_origins_with(veld_core::instance::dev_trusted_origins())
}

/// The allowlist, with the dev instance's contribution passed in.
///
/// Split out for one reason: `dev_trusted_origins` reads the environment and is
/// empty in a test process, so a test calling [`allowed_origins`] can only ever
/// assert things about the *base* list. The first version of the test below
/// looped over an empty collection and would have stayed green if the `extend`
/// were deleted — which is precisely the wiring it claimed to pin.
fn allowed_origins_with(dev_origins: Vec<String>) -> Vec<String> {
    let port = veld_core::instance::daemon_port();
    let mut origins = vec![
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
    ];
    // The installed instance's Caddy route (veld-helper's base config).
    origins.extend(management_origins(veld_core::instance::MANAGEMENT_HOST));
    // A dev instance registers its own management hostname with the helper.
    //
    // **The plaintext half only for a `.localhost` name.** `management_host`
    // accepts any hostname-shaped value, so `VELD_MANAGEMENT_HOST=dev.example.com`
    // would otherwise put `http://dev.example.com` on the allowlist — an origin an
    // on-path attacker on the developer's network can serve, where the `https` one
    // needs that name's key. A `.localhost` name resolves on loopback and cannot be
    // reached from off the machine, which is what makes dropping the scheme safe
    // for the dashboard's own host.
    if let Some(host) = veld_core::instance::management_host() {
        // ASCII-lowered because `management_host` accepts uppercase, and a
        // case-sensitive suffix test would send `Dev.LOCALHOST` down the
        // treat-as-remote branch.
        let local = host.to_ascii_lowercase().ends_with(".localhost");
        let (https, _) = dashboard_ports();
        let ports = if local {
            management_ports()
        } else {
            vec![("https", https)]
        };
        origins.extend(management_origins_for(&host, &ports));
    }
    // The vite dev server proxies /api (including this upgrade) to the daemon
    // it was pointed at, so the browser's Origin is vite's, not ours. Trust it
    // only on a dev instance: the installed daemon on the default port must
    // never accept an origin that a locally-running dev server could forge.
    //
    // This pair is the BOOTSTRAP tier's vite — `just dev-ui`, whose port is a
    // constant because `just dev-daemon`'s port is. The dev stack started as a
    // veld run allocates both, and contributes its origins through
    // `dev_trusted_origins` below instead.
    if port != veld_core::instance::DEFAULT_DAEMON_PORT {
        origins.push(format!("http://localhost:{VITE_DEV_PORT}"));
        origins.push(format!("http://127.0.0.1:{VITE_DEV_PORT}"));
    }
    // A daemon running as a veld node: its own veld-assigned URL, plus any dev
    // server declared as proxying its /api. Empty on the installed instance —
    // that gate lives inside `dev_trusted_origins`, not here.
    origins.extend(dev_origins);
    origins
}

/// Whether a request's `Origin` may open a terminal.
///
/// Fails closed on a missing or non-ASCII `Origin`. Browsers always send one
/// on a WebSocket handshake, so the only callers this turns away are
/// non-browser clients — which have a real terminal already.
///
/// Shared with the IDE control socket (`ide::channel`), which is the same kind
/// of upgrade with the same inability to carry a CSRF header. One allowlist, so
/// the two sockets cannot drift into disagreeing about who may connect.
pub(super) fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    // Exact match. Browsers serialise an origin canonically (lowercase scheme
    // and host, default port omitted), so there is nothing to normalise, and
    // prefix/suffix matching is how origin checks get bypassed.
    allowed_origins().iter().any(|a| a == origin)
}

/// Log a refused upgrade, with the list it was refused against.
///
/// **The allowlist goes in the log, and it has to.** The response cannot say
/// anything — a browser will not show a failed handshake's status, let alone a
/// body — so this line is the only account of a refusal that exists. Terse was
/// the rule and it produced 229 identical lines naming an origin that *looked*
/// right, with no way to see that the list had a different scheme in it. The
/// daemon's own log is not a disclosure channel: reading it already means reading
/// the user's files.
pub(super) fn log_rejected_origin(what: &str, headers: &HeaderMap) {
    warn!(
        origin = ?headers.get(header::ORIGIN),
        allowed = ?allowed_origins(),
        "rejected {what} from a disallowed origin"
    );
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
        // The response stays terse — an attacker learns nothing from it, and a
        // browser could not read it anyway. The log carries the diagnosis.
        log_rejected_origin("terminal upgrade", &headers);
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
        Err(SessionError::Unreachable(e)) => {
            warn!(
                session = %ticket.session_id,
                "could not reach the holder at this session's socket: {e}"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("could not reach that terminal's holder: {e}"),
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
    /// The holder at this session's socket could not be reached, or is not this
    /// session's — a wedged holder, a protocol version this build refuses, a
    /// greeting that answers for another id. Separate from
    /// [`SessionError::Spawn`] because nothing was spawned: "could not start a
    /// shell" sends whoever reads it looking for a startup failure that never
    /// happened, when the thing to look at is the holder.
    Unreachable(anyhow::Error),
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
        // What lets a process inside the shell open a URL in Veld: `$BROWSER`
        // pointing at a generated shim, the session id it names when it does, and —
        // for zsh, unless the setting is off — the `ZDOTDIR` handoff that gets the
        // shim directory onto `PATH` after the user's own startup files have run.
        // Computed here rather than in the holder because the holder is a dumb PTY
        // owner — it knows nothing about instances, ports or the CLI's location.
        //
        // A config-declared pane's own entries are layered on top: its resolved
        // `PATH` is a floor (a login shell computes the user's own PATH, and the
        // rc files typically refine it) and `VELD_PANE_*` are this pane's alone,
        // so they must win over anything the shim env carries.
        env: {
            let mut env = shims::session_env(
                &ticket.session_id,
                &holder::login_shell(),
                ticket.open_urls_in_app,
                ticket.intercept_system_open,
            );
            if let Some(pane) = ticket.pane.as_ref() {
                env.extend(pane.env.iter().cloned());
            }
            env
        },
        // The holder's own self-destruct grace, used when the *daemon* is gone and
        // nothing is left to reap it. Read at spawn, so a holder keeps the value
        // that was configured when its shell started: changing the setting affects
        // the daemon-side reaper immediately but cannot reach into holders already
        // running, and reaching into them would mean a protocol message whose only
        // job is to move a timer nobody is waiting on.
        orphan_grace_secs: detach_grace_hint().as_secs(),
        argv: ticket.pane.as_ref().map(|p| p.argv.clone()),
        pane_label: ticket.pane.as_ref().map(|p| p.label.clone()),
    };
    // A holder for this session id may still be running with a live shell in it:
    // this daemon's link to it broke (a takeover, a socket error), or it started
    // after the boot-time adoption sweep and this daemon has no record of it. Its
    // socket is the one `cfg` names, so adopt it rather than spawning a second
    // holder — the socket path has room for exactly one, so that spawn always
    // fails to bind, and only reaches the right shell by accident: the doomed
    // holder's own "is somebody already there?" probe lands on the live holder as
    // a connection, and `await_holder` then poll-connects onto it. Before
    // `TAKEOVER_PROBATION` those two connections displaced whatever the daemon had
    // just attached, so every resume both worked *and* immediately reported the
    // shell as exited — the loop the user sees as "it comes back for a moment and
    // then drops".
    let (attached, adopted) = match connect_holder(&cfg.socket).await {
        Ok(attached) => (attached, true),
        // Nothing behind the socket: this is a session to start, not to adopt.
        // The retry below deliberately does not cover this case — a fresh session
        // is the common one, and it would spend `HOLDER_START_TIMEOUT` doing
        // nothing before every single spawn.
        Err(e) if is_unanswered(&e) => (
            start_holder(&cfg).await.map_err(SessionError::Spawn)?,
            false,
        ),
        // Something *is* listening and did not complete the handshake — a holder
        // whose main loop is momentarily parked (it writes greetings and waits on
        // its daemon from that loop, both with longer bounds than this
        // handshake's), `EMFILE`, a full command channel. Poll it the way
        // `start_holder` polls a holder it just spawned: giving up on one attempt
        // would fail the reattach outright, and riding transients out is the
        // behaviour this path is replacing.
        Err(_) => (
            await_holder(&cfg.socket)
                .await
                .map_err(SessionError::Unreachable)?,
            true,
        ),
    };
    if adopted {
        // Never hung up on a mismatch: that holder belongs to another session and
        // may have a live shell in it — the same rule `adopt_one` has.
        // `start_holder` runs this check itself for the holder it spawned.
        verify_identity(&attached, &cfg).map_err(SessionError::Unreachable)?;
        debug!(session = %ticket.session_id, "adopted the holder already serving this session");
    }

    let mut sessions = SESSIONS.lock().await;
    if let Some(existing) = sessions.get(&ticket.session_id) {
        if adopted {
            // Never `discard_holder` an adopted one. That writes `HANGUP`, which a
            // holder honours whatever the generation and whoever is attached — and
            // the holder behind an adoption is the one the winning `existing`
            // session is serving, so the "cleanup" would kill its live shell. Only
            // a holder *this call spawned* is ours to throw away.
            return Ok((existing.clone(), true));
        }
        discard_holder(attached, "another attach won the race").await;
        return Ok((existing.clone(), true));
    }
    let session = register(&mut sessions, attached, slot);
    // **Released before the database write below.** `record_pane_launch` opens
    // SQLite synchronously, and `Db::open` sets a 10s busy timeout — so one
    // contended writer (the stats sampler, a concurrent `veld` CLI) would hold
    // the *global* session registry, and park a runtime worker, for up to ten
    // seconds. Every other attach, resize, close and the reaper queues behind it.
    drop(sessions);
    // Only now, with a holder actually running the command, does the pane's
    // identity become a fact worth remembering — the row is what makes this pane
    // resumable, and one written before the spawn would outlive a failure and
    // offer a resume for a conversation that was never created. Never on an
    // adoption: nothing was launched, the row for that launch already exists, and
    // writing a second one would offer a resume for a conversation this attach
    // did not start.
    if let Some(pane) = ticket.pane.as_ref().filter(|p| p.fresh && !adopted) {
        record_pane_launch(ticket, pane);
    }
    if adopted {
        info!(
            session = %session.id,
            worktree = %ticket.label,
            pid = session.pid,
            "adopted a terminal session from its own holder"
        );
    } else {
        info!(
            session = %session.id,
            worktree = %ticket.label,
            pid = session.pid,
            "terminal session started"
        );
    }
    // An adopted shell is one that was already running, which is exactly what
    // `resumed` means to the client — it is the difference between replaying a
    // terminal and presenting a fresh one.
    Ok((session, adopted))
}

/// Remember the identity a freshly launched pane is running under.
///
/// Best-effort by design: the shell is already up, and a database that cannot be
/// written is not a reason to tear it down. The cost of the failure is that the
/// pane cannot be resumed later, which is the same position every pane without a
/// `resume` command is in.
fn record_pane_launch(ticket: &Ticket, pane: &PaneLaunch) {
    let outcome = veld_core::db::Db::open().and_then(|db| {
        db.record_pane_session(
            &ticket.session_id,
            ticket.worktree_id,
            &pane.spec_id,
            &pane.token,
        )
        .map(|_| ())
    });
    if let Err(e) = outcome {
        warn!(
            session = %ticket.session_id,
            pane = %pane.spec_id,
            "could not record the pane session, so this pane will not be resumable: {e}"
        );
    }
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
    let (control, _) = broadcast::channel(CONTROL_CHANNEL);
    // Seeded with the holder's answer, so a session adopted after its shell
    // already finished reports the exit instead of presenting a dead prompt as a
    // live one.
    let (exit, _) = watch::channel(hello.exited);
    let (attach_epoch, _) = watch::channel(0u64);
    let (closing, _) = watch::channel(false);
    let (released, _) = watch::channel(false);
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
        control,
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
        released,
        pid: hello.pid,
        busy_supported: hello.supports_busy,
        busy_query: Mutex::new(None),
        busy_lock: tokio::sync::Mutex::new(()),
        _slot: slot,
    });
    sessions.insert(session.id.clone(), session.clone());
    // Whatever this session was before, it is in the registry now, which is the
    // record `RELEASED` stands in for while it is not.
    RELEASED
        .lock()
        .expect("released set poisoned")
        .remove(&session.id);

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
    // Only names this daemon could have written, because a failed handshake ends
    // in `remove_file`: `VELD_PTY_DIR` is a plain env var, and pointed one level
    // up at `~/.veld` a boot would otherwise delete `daemon.sock`. Anything else
    // in the directory is not ours to reason about. A missing directory is an
    // empty list, which is the common case.
    let candidates = veld_core::instance::holder_sockets_in(&pty_dir());
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
            wire::BUSY => {
                let Some(busy) = wire::decode_busy(&frame.payload) else {
                    warn!(session = %session.id, "ignoring a malformed busy frame");
                    continue;
                };
                // Completes a pending query, if any. A reply nobody asked for
                // (a stale one, or a duplicated holder) is dropped — there is
                // nothing to do with it.
                if let Some(tx) = session
                    .busy_query
                    .lock()
                    .expect("busy query poisoned")
                    .take()
                {
                    let _ = tx.send(busy);
                }
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
        // Losing the connection is not the same fact as losing the holder, and
        // the two used to be conflated here. A peer that connects to the holder
        // displaces this connection, and the shell behind it is untouched —
        // publishing an exit for it reports a live shell as dead, hands every
        // attached socket a status nothing produced, and leaves the reaper to hang
        // up a terminal somebody is still working in. So ask the holder before
        // speaking for it.
        if holder_is_alive(&session.id).await {
            warn!(
                session = %session.id,
                pid,
                "this daemon's connection to the holder was displaced; releasing the session \
                 rather than reporting an exit — reattaching will pick the shell up again"
            );
            release_session(&session).await;
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

/// Whether a holder may still be serving `id` — and **fails safe**, because the
/// answer decides whether a live shell is about to be declared dead.
///
/// Only the three errnos [`is_unanswered`] names mean "nothing is behind this
/// socket". Everything else — a handshake that did not finish inside
/// [`HOLDER_HELLO_TIMEOUT`], `EMFILE`, a refused protocol version — is a holder
/// that may well be alive, and one of those is *routine* here: the holder's main
/// loop is parked while it writes a greeting (up to `GREETING_TIMEOUT`) or waits
/// on a daemon that is not reading (up to `OUTPUT_SEND_TIMEOUT`), both longer than
/// the timeout on this handshake, and a parked loop cannot answer this probe. A
/// wedged holder therefore leaks its shell until its own orphan grace, which is
/// the better half of the trade: the other half is inventing an exit for a shell
/// somebody is working in.
///
/// Deliberately a full handshake rather than "does the socket accept?": a socket
/// file outlives the process that bound it. The connection is then dropped, which
/// is the probe shape `holder::TAKEOVER_PROBATION` makes harmless — it is
/// greeted, never promoted, and the holder's own daemon keeps the session. The
/// one exception is a protocol version this build refuses, where
/// [`connect_holder`] writes a `HANGUP` and ends that shell; that is the
/// deliberate policy documented there, not something this probe gets to opt out
/// of, and it is unreachable from here in practice — a holder cannot change
/// version, and this daemon only reaches this function for a session it already
/// spoke to.
async fn holder_is_alive(id: &str) -> bool {
    match connect_holder(&socket_for(id)).await {
        // The socket path is derived from the id, but a digest collision would
        // put another session behind it — and that one being alive says nothing
        // about this one.
        Ok(attached) => attached.hello.session_id == id,
        Err(e) => !is_unanswered(&e),
    }
}

/// Give a session up without ending its shell.
///
/// The counterpart to [`end_session`], for the case where this daemon has lost
/// its link to a holder that is still running: the registry entry is useless
/// (nothing can reach the shell through it any more) but the shell itself is
/// fine, so nothing may be hung up and no exit may be published.
///
/// Attached sockets are closed rather than told anything, via
/// [`Session::released`] — a client reads that as a dropped connection and offers
/// to reattach, which goes through [`obtain_session`], finds the live holder at
/// its socket and adopts it. The user does have to ask for that reattach; what
/// they must never be offered is the *destructive* answer, which is what
/// signalling this as a takeover produced.
async fn release_session(session: &Arc<Session>) {
    {
        let mut released = RELEASED.lock().expect("released set poisoned");
        // A bound, not the correctness rule — `released_worktree` prunes what it
        // reads. This is what keeps entries for ids nobody ever asks about again
        // from accumulating for the life of the daemon, and it runs here because
        // this is the rare path.
        released.retain(|id, _| socket_for(id).exists());
        released.insert(session.id.clone(), session.worktree_id);
    }
    {
        let mut sessions = SESSIONS.lock().await;
        // Only if it is still *this* session: an entry re-registered under the
        // same id in the meantime belongs to a working link, and removing it
        // would strand the shell it is serving.
        if sessions
            .get(&session.id)
            .is_some_and(|s| Arc::ptr_eq(s, session))
        {
            sessions.remove(&session.id);
        }
    }
    // `send_replace` for the reason `exit` documents: with no socket attached
    // there is no receiver, and `send` would drop the flag — which matters here
    // because a socket that attaches *after* this point (a reattach that raced
    // the release) must still see it and leave, rather than sit on a session
    // nothing feeds.
    session.released.send_replace(true);
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
#[derive(Serialize, Clone, Debug)]
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
    /// Open `url` in a browser pane beside this terminal.
    ///
    /// The field is a [`veld_core::ide::CanonicalUrl`] rather than a `String` so that
    /// the caller's spelling cannot be forwarded here by accident — see that type.
    ///
    /// Pushed from an HTTP handler rather than derived from the PTY stream, which
    /// makes it the one server→client frame that is not about terminal bytes. It
    /// travels on this socket because the socket *is* the routing decision: the page
    /// attached to a session is the window whose dock holds that terminal, so no
    /// window id has to be invented, stored or kept correct across a detach.
    OpenUrl { url: veld_core::ide::CanonicalUrl },
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

    // Subscribed before the epoch is claimed, for the same reason the output
    // subscription is: a URL routed in the window between the two would otherwise
    // be sent to nobody.
    let mut control = session.control.subscribe();

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

    // Released between `obtain_session` handing this socket the session and the
    // upgrade completing — a browser round trip wide, which is not a theoretical
    // window. **Before the replay, deliberately**: sending the scrollback and a
    // `Ready` and only then closing is precisely the "it comes back for a moment
    // showing everything, then drops" that this whole change exists to remove, and
    // it would be this code producing it. Nothing is attached yet, so there is no
    // input task to abort.
    let mut released_rx = session.released.subscribe();
    if *released_rx.borrow_and_update() {
        let _ = ws_tx.close().await;
        mark_detached(&session, epoch);
        return;
    }

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

            frame = control.recv() => match frame {
                Ok(frame) => {
                    // Only the socket that currently owns the session forwards. A
                    // displaced one is still in this loop until it notices the
                    // takeover, and two windows opening the same URL is a pane in a
                    // window the user is not looking at.
                    if *session.attach_epoch.borrow() == epoch
                        && ws_tx.send(frame.frame()).await.is_err()
                    {
                        break;
                    }
                }
                // Dropped control frames are not worth telling the client about:
                // there is nothing to redraw, and `Lagged` means "your screen is
                // missing bytes", which would be a lie here.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(session = %session.id, dropped = n, "control frames dropped");
                }
                Err(broadcast::error::RecvError::Closed) => {}
            },

            Ok(()) = epoch_rx.changed() => {
                let current = *epoch_rx.borrow_and_update();
                if current != epoch {
                    let _ = ws_tx.send(ServerControl::TakenOver.frame()).await;
                    break;
                }
            },

            // This daemon gave the session up while the shell kept running
            // (`release_session`). No control frame: a close with none is what the
            // client already reads as "the pipe broke, the shell probably did
            // not", which is both true here and the state whose button is
            // Reconnect. `TakenOver` would be a different claim with a
            // *destructive* button behind it.
            Ok(()) = released_rx.changed() => {
                if *released_rx.borrow_and_update() {
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

    /// An `argv` pane runs inside the user's login+interactive shell — the
    /// whole point being that a pane inherits the same environment a real
    /// terminal gives, not just the injected `PATH`. The shape must be exactly
    /// `$SHELL -l -i -c '<quoted argv>'`, and every argv element must be
    /// single-quoted so spaces, `$`, backticks or a quote in a value can never
    /// become a second command.
    #[test]
    fn a_pane_argv_is_wrapped_in_the_login_shell() {
        let argv = login_shell_command(
            "/bin/zsh",
            &[
                "pi".to_owned(),
                "--session-id".to_owned(),
                "a1b2c3".to_owned(),
            ],
        );
        assert_eq!(
            argv,
            vec![
                "/bin/zsh".to_owned(),
                "-l".to_owned(),
                "-i".to_owned(),
                "-c".to_owned(),
                "'pi' '--session-id' 'a1b2c3'".to_owned(),
            ]
        );

        // A space or a single quote in a value stays one argument when the
        // shell parses the re-quoted line.
        let tricky = login_shell_command(
            "/bin/zsh",
            &[
                "pi".to_owned(),
                "My Projects/veld".to_owned(),
                "it's a 'quote'".to_owned(),
            ],
        );
        assert_eq!(
            tricky[4],
            "'pi' 'My Projects/veld' 'it'\\''s a '\\''quote'\\'''"
        );
    }

    /// `veld lint` and the spawn path must agree on the pane variable scope,
    /// exactly.
    ///
    /// Two hand-maintained lists: `ide::PANE_BUILTINS` (what lint accepts) and
    /// the `builtins.insert` calls in [`pane_context`] (what actually resolves).
    /// A name in the first but not the second is a pane that lints clean and
    /// then dies at spawn with "command could not be resolved" — the failure a
    /// closed scope exists to prevent. A name in the second but not the first is
    /// unreachable. The comment on `pane_context` used to *claim* this was
    /// checked; nothing checked it.
    #[test]
    fn pane_context_populates_every_lintable_name() {
        let pane = veld_core::ide::PaneDef {
            id: "probe".to_owned(),
            label: "Probe".to_owned(),
            description: None,
            icon: None,
            requires_bin: Vec::new(),
            body: veld_core::ide::PaneBody::Terminal(veld_core::ide::TerminalPane {
                launch: veld_core::config::CommandSpec::Argv(vec!["true".to_owned()]),
                resume: None,
                auto_resume: false,
                close_on_exit: true,
                allow_terminal_renaming: false,
            }),
        };
        let cfg: veld_core::config::VeldConfig = serde_json::from_value(serde_json::json!({
            "schemaVersion": "3",
            "name": "probe",
            "nodes": {},
        }))
        .expect("minimal config");
        let ctx = pane_context(&pane, "tok", FsPath::new("/tmp/wt"), "main", &cfg);
        let mut names: Vec<&str> = ctx.builtins.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, veld_core::ide::PANE_BUILTINS.to_vec());
    }

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
        use veld_core::instance::is_holder_socket_name;

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
        //
        // Asserted through a bounded retry, and the reason is the same one
        // [`is_unanswered`] documents from the other side: under real resource
        // pressure a `connect` can fail with an errno that says nothing about the
        // peer (`EMFILE`, `ENFILE`, `EINTR`, `EAGAIN`), and those are deliberately
        // *not* "unanswered" — treating a busy machine as a dead holder would unlink
        // the socket of a live shell. So the classification this test is about
        // (`ECONNREFUSED` ⇒ stale) is only decidable on an answer that carries peer
        // information; anything else is the machine talking, not the code under test.
        // This flaked under a saturated CPU while the whole workspace was compiling.
        let dead = dir.path().join("dead.sock");
        drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
        let mut classified = false;
        let mut seen = Vec::new();
        for _ in 0..5 {
            let e = connect_err(&dead).await;
            if is_unanswered(&e) {
                classified = true;
                break;
            }
            let errno = e
                .downcast_ref::<std::io::Error>()
                .and_then(|io| io.raw_os_error());
            seen.push(errno);
            // Only a pressure errno earns another go; a genuinely wrong
            // classification must still fail the test rather than be retried away.
            //
            // `errno.is_none()` belongs in that set for the same reason, and it
            // cannot hide the thing under test: an error with no errno is one the
            // *handshake* produced, which means the connect succeeded — the failure
            // being asserted (`ECONNREFUSED` ⇒ stale) is not even reachable through
            // it. Observed as "holder closed before greeting" while the rest of
            // this binary was forking real shells, i.e. the kernel answering for a
            // listener that has been closed and whose descriptor something else is
            // briefly keeping alive. Machine, not code.
            let pressure = matches!(
                errno,
                Some(libc::EMFILE | libc::ENFILE | libc::EINTR | libc::EAGAIN)
            ) || errno.is_none();
            assert!(
                pressure,
                "dead socket misclassified: errno={errno:?} err={e}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            classified,
            "never got a decidable answer for a dead socket; saw {seen:?}"
        );

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
        let origin_of = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::ORIGIN, v.parse().unwrap());
            h
        };

        // Absent Origin fails closed — the whole point of the gate.
        assert!(!origin_allowed(&HeaderMap::new()));
        // Substring and suffix tricks must not pass an exact-match check.
        assert!(!origin_allowed(&origin_of(
            "https://veld.localhost.evil.com"
        )));
        assert!(!origin_allowed(&origin_of("https://evil.veld.localhost")));
        assert!(!origin_allowed(&origin_of("null")));
        // A port no mode serves, in either scheme.
        assert!(!origin_allowed(&origin_of("https://veld.localhost:8443")));
        assert!(!origin_allowed(&origin_of("http://veld.localhost:8080")));

        // **Every origin the dashboard is actually served at.** Asked for through
        // `management_origins` rather than written out, because the answer depends
        // on this machine's helper and `~/.veld/setup.json` — a test that hardcoded
        // 443 would pass on CI and fail on a no-sudo install. Which ports those are
        // is pinned in `the_port_fallback_follows_the_setup_mode`, and which schemes
        // in `the_plaintext_origin_is_only_trusted_on_port_80`.
        let host = veld_core::instance::MANAGEMENT_HOST;
        let served = management_origins(host);
        assert!(!served.is_empty());
        for origin in &served {
            assert!(
                origin_allowed(&origin_of(origin)),
                "the dashboard's own origin {origin} must be allowed — over plaintext \
                 too where that is a served surface (see `management_ports`), whose \
                 refusal cost a whole session's terminals and worktree arbitration"
            );
            assert!(allowed_origins().iter().any(|o| o == origin));
        }
        assert!(served.iter().any(|o| o.starts_with("https://")));
    }

    /// Which ports each setup mode puts in front.
    ///
    /// The mapping is the fallback for "the helper has not answered yet", and it is
    /// pinned separately from the formatting because it is the half that decides
    /// *what is trusted* — see `the_plaintext_origin_is_only_trusted_on_port_80`.
    #[test]
    fn the_port_fallback_follows_the_setup_mode() {
        let hi = veld_core::instance::UNPRIVILEGED_HTTPS_PORT;
        let lo = veld_core::instance::UNPRIVILEGED_HTTP_PORT;
        for mode in [Some("unprivileged"), Some("auto")] {
            assert_eq!(ports_for_mode(mode), (hi, lo), "the no-sudo pair");
        }
        // Privileged, unset, and anything unrecognised: the pair Caddy owns as
        // root. Every way of failing to read `setup.json` arrives here as `None`,
        // and this is the answer that trusts an origin nothing can squat.
        for mode in [Some("privileged"), None, Some("something-new")] {
            assert_eq!(ports_for_mode(mode), (443, 80), "mode {mode:?}");
        }
    }

    /// The plaintext origin is trusted **only** on a root-only port.
    ///
    /// Listing the unprivileged pair's HTTP port was the first version of this fix
    /// and it opens a window: Caddy is not bound to 18080 before the helper starts,
    /// during a `veld update`, or after a crash, and `veld.localhost` resolves to
    /// loopback — so any process could bind it, serve a page holding an allowlisted
    /// origin, and reverse-proxy `/api` to make `mint_ticket`'s header check
    /// same-origin. 443/80 cannot be taken that way without root — see
    /// `management_ports` for what that claim rests on. Two review angles found the
    /// unconditional version independently.
    #[test]
    fn the_plaintext_origin_is_only_trusted_on_port_80() {
        let host = veld_core::instance::MANAGEMENT_HOST;
        let hi = veld_core::instance::UNPRIVILEGED_HTTPS_PORT;
        let lo = veld_core::instance::UNPRIVILEGED_HTTP_PORT;

        // Privileged: both schemes, ports omitted the way a browser serialises
        // them. `http://veld.localhost` is the origin whose refusal cost a whole
        // session's terminals and worktree arbitration.
        assert_eq!(
            management_origins_for(host, &management_ports_from(443, 80)),
            vec![format!("https://{host}"), format!("http://{host}")]
        );

        // No-sudo: HTTPS only. Nothing advertises the plaintext high port, so this
        // costs a URL nobody arrives at by typing a hostname.
        assert_eq!(
            management_origins_for(host, &management_ports_from(hi, lo)),
            vec![format!("https://{host}:{hi}")]
        );

        // Exact pairs, never a cross product: nothing serves HTTPS on the HTTP port
        // or the other way round, so neither crossing may appear.
        let unprivileged = management_origins_for(host, &management_ports_from(hi, lo));
        assert!(!unprivileged.contains(&format!("http://{host}:{hi}")));
        assert!(!unprivileged.contains(&format!("https://{host}:{lo}")));
        assert!(!unprivileged.contains(&format!("http://{host}:{lo}")));
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
            // Same gate, the other source: a daemon started as a veld node
            // contributes its own URL and its proxying dev servers, and the
            // installed one must obtain none of that however its environment
            // is set. The gate itself is pinned in `veld_core::instance`.
            assert!(veld_core::instance::dev_trusted_origins().is_empty());
        }

        // Every origin the instance layer hands out has to actually reach the
        // allowlist. The wiring is the easy half to drop in a refactor, and its
        // failure mode is a 403 on a WebSocket handshake, whose reason a browser
        // cannot show the user.
        //
        // Fed explicitly rather than through `dev_trusted_origins()`, which is
        // empty in a test process: looping over that returned an empty
        // collection and passed whether or not the `extend` existed at all.
        let dev = "https://dev-daemon.somerun.veld.localhost".to_owned();
        let allowed = allowed_origins_with(vec![dev.clone()]);
        assert!(
            allowed.contains(&dev),
            "the dev origin never reached the list"
        );
        // …and the base list is still there beside it. Asked for by the same
        // function the runtime uses, because which port the dashboard is on depends
        // on this machine's helper and setup mode — see
        // `the_port_fallback_follows_the_setup_mode`.
        for origin in management_origins(veld_core::instance::MANAGEMENT_HOST) {
            assert!(allowed.contains(&origin));
        }
        // A value NOT handed in is still not trusted — the extension must be
        // exactly what it was given, not a widening.
        assert!(!allowed.contains(&"https://other.veld.localhost".to_owned()));
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
                pane: None,
                open_urls_in_app: true,
                intercept_system_open: true,
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
                pane: None,
                open_urls_in_app: true,
                intercept_system_open: true,
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
            (
                "POST",
                "/api/pty/sessions/a/open-url",
                r#"{"url":"https://example.com"}"#,
            ),
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

    #[tokio::test]
    async fn open_url_refuses_what_a_pane_could_never_show() {
        use axum::body::Body;
        use tower::ServiceExt;

        // Checked before the session is looked up and before the database is
        // touched, so these need neither. A caller sending one of these has a bug,
        // and answering "opened in the system browser" would hide it — the one place
        // a non-web argument is expected is `veld open-url` standing in for `open`,
        // which execs the real tool without asking the daemon at all.
        for body in [
            r#"{"url":"file:///etc/passwd"}"#,
            r#"{"url":"javascript:alert(1)"}"#,
            r#"{"url":"vscode://file/tmp/x"}"#,
            r#"{"url":"https://"}"#,
            r#"{"url":""}"#,
            // An unknown field is a client typo, not something to ignore.
            r#"{"url":"https://example.com","target":"pane"}"#,
        ] {
            let res = routes()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/api/pty/sessions/abc/open-url")
                        .header("content-type", "application/json")
                        .header("x-veld-request", "1")
                        .body(Body::from(body.to_owned()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                res.status().is_client_error(),
                "{body} must be a client error, got {}",
                res.status()
            );
        }

        // A well-formed URL for a session that does not exist is a 404 — again
        // without reaching the database, so this test cannot depend on one.
        let res = routes()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/pty/sessions/nosuchsession/open-url")
                    .header("content-type", "application/json")
                    .header("x-veld-request", "1")
                    .body(Body::from(r#"{"url":"https://example.com"}"#.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
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
                    pane: None,
                    open_urls_in_app: true,
                    intercept_system_open: true,
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
            handshake_error(req).await.0
        }

        /// Status **and body** from a handshake that is expected to fail.
        ///
        /// The body matters wherever two refusals share a status: 503 is both "too
        /// many terminal sessions" and "could not reach that terminal's holder", so
        /// asserting on the code alone would let a suite that accumulated sessions
        /// pass a test about identity.
        async fn handshake_error(req: http::Request<()>) -> (http::StatusCode, String) {
            match tokio_tungstenite::connect_async(req).await {
                Ok(_) => panic!("handshake unexpectedly succeeded"),
                Err(tokio_tungstenite::tungstenite::Error::Http(res)) => {
                    let status = res.status();
                    let body = res
                        .body()
                        .as_ref()
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                        .unwrap_or_default();
                    (status, body)
                }
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
                    pane: None,
                    open_urls_in_app: true,
                    intercept_system_open: true,
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

        /// Somebody probing the holder's socket must not cost the user a
        /// terminal.
        ///
        /// The daemon's half of `holder::tests::
        /// a_probe_connection_does_not_take_a_terminal_from_its_daemon`, and the
        /// level the bug was actually experienced at: `veld doctor` counts live
        /// holders by connecting to each socket in the directory and closing
        /// again. While any accepted connection displaced the attached daemon, one
        /// `veld doctor` disconnected every terminal on the machine — the daemon
        /// read EOF from the holder, published exit code 1 for shells that were
        /// still running, and the reaper hung those shells up 30 minutes later.
        /// The shape here is exactly doctor's: connect, drop, count.
        #[tokio::test]
        async fn a_probe_on_the_holder_socket_does_not_end_the_session() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            read_control(&mut ws, "ready").await;
            ws.send(WsMessage::Binary(
                b"VELD_MARK=probed; printf 'set%s\\n' '-ok'\n"
                    .to_vec()
                    .into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, "set-ok").await;

            // Three, because doctor probes every socket it finds and a resume
            // probes again on every retry.
            let socket = socket_for(&sid);
            for _ in 0..3 {
                let probe = std::os::unix::net::UnixStream::connect(&socket)
                    .expect("the holder must be listening");
                drop(probe);
            }

            // Same socket, same shell, same variable. `read_until` fails loudly on
            // an `exit` control frame, which is what the bug produced here.
            ws.send(WsMessage::Binary(
                b"printf 'mark=%s\\n' $VELD_MARK\n".to_vec().into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, "mark=probed").await;
            end_session(&sid, "test cleanup").await;
        }

        /// A daemon that has lost its record of a session, while the holder for it
        /// is still running, must adopt that holder rather than spawn a second one.
        ///
        /// The registry entry is removed by hand here because that is precisely the
        /// state `release_session` leaves behind, and the state a daemon is in
        /// whenever a holder started after its boot-time adoption sweep. What it
        /// used to do instead was spawn a holder onto a socket path with room for
        /// one: that holder fails to bind, and the session came back only by
        /// accident — through the doomed holder's own liveness probe and
        /// `await_holder`'s poll-connect, both of which then displaced the
        /// connection the daemon had just made. The user saw their shell reappear
        /// and drop again, several times a second.
        #[tokio::test]
        async fn a_lost_session_adopts_the_holder_that_is_still_serving_it() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            read_control(&mut ws, "ready").await;
            ws.send(WsMessage::Binary(
                b"VELD_MARK=readopted; printf 'set%s\\n' '-ok'\n"
                    .to_vec()
                    .into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, "set-ok").await;
            drop(ws);

            // The daemon forgets the session. The holder — and the shell in it —
            // knows nothing about that.
            SESSIONS.lock().await.remove(&sid);
            assert!(
                socket_for(&sid).exists(),
                "the holder must still be serving its socket"
            );

            let mut again = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            let ready = read_control(&mut again, "ready").await;
            assert_eq!(
                ready["resumed"], true,
                "adopting a running holder is a resume, not a fresh terminal"
            );
            again
                .send(WsMessage::Binary(
                    b"printf 'mark=%s\\n' $VELD_MARK\n".to_vec().into(),
                ))
                .await
                .unwrap();
            read_until(&mut again, "mark=readopted").await;
            end_session(&sid, "test cleanup").await;
        }

        /// Releasing a session must not tell the client its terminal was taken
        /// over, because the client's answer to that is destructive.
        ///
        /// `release_session` runs when this daemon has lost its link to a holder
        /// whose shell is still running. Signalling it through the attach epoch —
        /// the obvious way to make attached sockets leave — puts
        /// `ServerControl::TakenOver` on the wire, which
        /// `ui/src/panes/terminalHost.ts` renders as `ended` / "opened in another
        /// window". `PaneArea.tsx` offers Reconnect only for `error`, so that pane
        /// would present **Restart** as its one action — which deletes the session
        /// and hangs up the very shell this path exists to preserve. A close with
        /// no control frame is what the client already reads as a dropped
        /// connection, which is both true and the state whose button reattaches.
        #[tokio::test]
        async fn a_released_session_closes_its_socket_without_claiming_a_takeover() {
            use futures_util::StreamExt;

            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let mut ws = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            read_control(&mut ws, "ready").await;

            let session = SESSIONS
                .lock()
                .await
                .get(&sid)
                .cloned()
                .expect("the session must be registered");
            release_session(&session).await;

            let ended = tokio::time::timeout(STEP_TIMEOUT, async {
                while let Some(Ok(msg)) = ws.next().await {
                    // **No control frame at all**, which is the actual contract —
                    // not merely "not these two". A future `Released` variant is
                    // the natural thing to reach for (every other server→client
                    // signal is a typed frame) and would be a new claim the client
                    // has to interpret, when the whole point is that it already
                    // interprets a silent close correctly.
                    if let WsMessage::Text(text) = msg {
                        panic!("a release must close in silence, got a control frame: {text}");
                    }
                }
            })
            .await;
            assert!(ended.is_ok(), "the socket must close on a release");
            assert!(
                SESSIONS.lock().await.get(&sid).is_none(),
                "a released session leaves the registry"
            );

            // And the way back is the ordinary one: reattaching adopts the holder
            // that kept running. This doubles as the cleanup — with no registry
            // entry there is nothing `end_session` could hang up, and the shell
            // would sit in the holder until its orphan grace.
            let mut again = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            let ready = read_control(&mut again, "ready").await;
            assert_eq!(ready["resumed"], true, "the shell was still there");
            end_session(&sid, "test cleanup").await;
        }

        /// A socket that attaches to an already-released session closes without
        /// showing anything first.
        ///
        /// The release can land between `obtain_session` handing this socket the
        /// session and the WebSocket upgrade completing — a browser round trip
        /// wide. Checking for it *after* the scrollback replay and `Ready` would
        /// make this code produce the reported symptom exactly: the terminal comes
        /// back showing everything it was doing, and drops. So the check sits
        /// before the replay, and this pins that ordering rather than the fact of
        /// the close (which the test above covers).
        ///
        /// The session is put back in the registry by hand because that window is
        /// not otherwise reachable from a client: once released, the next attach
        /// builds a *new* session by adopting the holder.
        #[tokio::test]
        async fn attaching_to_an_already_released_session_shows_nothing() {
            use futures_util::StreamExt;

            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let mut ws = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            read_control(&mut ws, "ready").await;
            // Something in the scrollback, so a replay would be visible.
            ws.send(WsMessage::Binary(
                b"printf 'set%s\\n' '-ok'\n".to_vec().into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, "set-ok").await;

            let session = SESSIONS
                .lock()
                .await
                .get(&sid)
                .cloned()
                .expect("the session must be registered");
            release_session(&session).await;
            drop(ws);
            SESSIONS.lock().await.insert(sid.clone(), session);

            let mut late = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            let seen = tokio::time::timeout(STEP_TIMEOUT, async {
                let mut frames = 0usize;
                while let Some(Ok(msg)) = late.next().await {
                    if !matches!(msg, WsMessage::Close(_)) {
                        frames += 1;
                    }
                }
                frames
            })
            .await
            .expect("the socket must close");
            assert_eq!(
                seen, 0,
                "a released session must not replay its terminal and then drop"
            );

            // Cleanup: the manual re-insert left an entry whose holder is still
            // running, and `end_session` is what reaches it.
            end_session(&sid, "test cleanup").await;
        }

        /// A holder that answers for another session is never adopted — and never
        /// hung up either.
        ///
        /// The socket path is a digest of the session id, so what sits behind it
        /// is a claim, not a fact; `mint_ticket`'s cross-worktree guard reads the
        /// *registry*, so a session absent from it walks past that guard and this
        /// check is what stops there. It is also the reason the adopt path may not
        /// "clean up" what it refuses: that holder has somebody else's live shell
        /// in it.
        #[tokio::test]
        async fn a_holder_answering_for_another_session_is_refused_not_adopted() {
            use std::io::Write;

            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let ours = session_id();
            let theirs = session_id();

            // A real holder, at *our* socket path, announcing *their* id.
            let cfg = wire::HolderConfig {
                session_id: theirs.clone(),
                worktree_id: 1,
                label: "test".to_owned(),
                cwd: dir.path().to_path_buf(),
                cols: 80,
                rows: 24,
                socket: socket_for(&ours),
                orphan_grace_secs: 3600,
                argv: Some(vec!["cat".to_owned()]),
                env: std::collections::BTreeMap::new(),
                pane_label: None,
            };
            tokio::spawn(holder::run(cfg));
            let deadline = Instant::now() + STEP_TIMEOUT;
            while !socket_for(&ours).exists() {
                assert!(Instant::now() < deadline, "the fixture holder must bind");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            let ticket = plant_ticket(&ours, dir.path());
            let (status, body) = handshake_error(attach_request(
                addr,
                &format!("ticket={ticket}"),
                Some(&good_origin()),
            ))
            .await;
            assert_eq!(
                status,
                http::StatusCode::SERVICE_UNAVAILABLE,
                "a holder that answers for another session is not this session's"
            );
            // The body, because 503 is also what a full session table answers.
            assert!(
                body.contains("could not reach that terminal's holder"),
                "the refusal must be the identity one: {body}"
            );
            assert!(
                SESSIONS.lock().await.get(&ours).is_none(),
                "and nothing may be registered for it"
            );
            assert!(
                socket_for(&ours).exists(),
                "the refused holder must be left alone, not hung up"
            );

            // Cleanup: the five bytes any peer may write, since this holder is not
            // one this daemon owns a session for.
            let mut blunt = std::os::unix::net::UnixStream::connect(socket_for(&ours)).unwrap();
            let mut frame = vec![wire::HANGUP];
            frame.extend_from_slice(&0u32.to_be_bytes());
            blunt.write_all(&frame).unwrap();
            blunt.flush().unwrap();
        }

        /// A released session is a resume; one whose holder has since gone is not.
        ///
        /// `mint_ticket` reads this to decide whether an attach is a resume, and a
        /// resume is exempt from every rule that governs *starting* a shell — no
        /// new terminal in a trashed worktree, the directory must exist, capacity,
        /// resolving a pane's command. So the tombstone being wrong in the "still
        /// there" direction is not a stale entry, it is a fresh spawn let through
        /// the gates under a resume's rules, in a pane still labelled for a command
        /// nobody ran. The socket is checked as a necessary condition for exactly
        /// that reason, and this pins both halves.
        #[tokio::test]
        async fn a_released_session_stops_counting_as_one_when_its_holder_goes() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let mut ws = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            read_control(&mut ws, "ready").await;
            let session = SESSIONS
                .lock()
                .await
                .get(&sid)
                .cloned()
                .expect("the session must be registered");
            let worktree_id = session.worktree_id;
            release_session(&session).await;
            drop(ws);

            assert_eq!(
                released_worktree(&sid),
                Some(worktree_id),
                "a released session with a live holder is a resume, and belongs to \
                 the worktree it was started in"
            );

            // The other half, and it is written by hand for a reason: the state it
            // describes — a record whose holder has gone — is one this daemon
            // cannot produce on demand, because releasing is precisely giving up
            // the link that would tell it the shell ended. Ending the session
            // through the API instead would satisfy the assertion by removing the
            // record itself, and the test would pass with the prune deleted.
            let ghost = session_id();
            RELEASED
                .lock()
                .expect("released set poisoned")
                .insert(ghost.clone(), worktree_id);
            assert!(
                !socket_for(&ghost).exists(),
                "the fixture needs an id with no holder"
            );
            // Asserted before the read, because a concurrent test's
            // `release_session` sweeps records whose socket is gone — and this one
            // qualifies. Without this, losing that race would satisfy the
            // assertions below for the wrong reason, which is the failure mode
            // this whole test exists to have caught once already.
            assert!(
                RELEASED
                    .lock()
                    .expect("released set poisoned")
                    .contains_key(&ghost),
                "the record must still be there for the read to be the thing under test"
            );
            assert_eq!(
                released_worktree(&ghost),
                None,
                "a record whose holder is gone must not make the next attach a resume"
            );
            assert!(
                !RELEASED
                    .lock()
                    .expect("released set poisoned")
                    .contains_key(&ghost),
                "and reading it must drop it, since nothing else will"
            );

            end_session(&sid, "test cleanup").await;
        }

        /// Closing a pane must reach the shell even when this daemon has given the
        /// session up.
        ///
        /// `end_session` works off the registry, and `release_session` is a way for
        /// a *live* shell to be absent from it — so `DELETE /api/pty/sessions/{id}`
        /// answered "closed" having done nothing, and whatever was running in that
        /// terminal (an agent, a dev server) kept running until the holder's orphan
        /// grace, half an hour after the user asked it to stop.
        #[tokio::test]
        async fn closing_a_released_session_still_ends_its_shell() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let mut ws = open(addr, &sid, dir.path(), "&cols=90&rows=30").await;
            read_control(&mut ws, "ready").await;
            let socket = socket_for(&sid);
            assert!(socket.exists(), "the holder must be serving");

            let session = SESSIONS
                .lock()
                .await
                .get(&sid)
                .cloned()
                .expect("the session must be registered");
            release_session(&session).await;
            drop(ws);

            assert!(
                end_session(&sid, "closed by the client").await,
                "closing a released session must reach its holder"
            );
            // The holder removes its own socket on the way out, so its
            // disappearance is the shell's death observed from outside.
            let deadline = Instant::now() + STEP_TIMEOUT;
            while socket.exists() {
                assert!(
                    Instant::now() < deadline,
                    "the holder must hang up and exit: {socket:?} is still there"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        /// The daemon's environment additions actually reach the shell.
        ///
        /// `VELD_PTY_SESSION` rather than `$BROWSER`, because it is the one that is
        /// always set: the shim directory needs a `veld` binary beside the running
        /// executable, and a test binary lives in `target/debug/deps`. What this pins
        /// is the wiring — `HolderConfig.env` being applied to the spawned shell —
        /// which is the part a reader would otherwise have to take on trust.
        #[tokio::test]
        async fn the_session_id_reaches_the_shell_environment() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;
            ws.send(WsMessage::Binary(
                b"printf 'sess=%s\n' \"$VELD_PTY_SESSION\"
"
                .to_vec()
                .into(),
            ))
            .await
            .unwrap();
            read_until(&mut ws, &format!("sess={sid}")).await;
            end_session(&sid, "test cleanup").await;
        }

        /// A routed URL reaches the attached socket, and "nobody is attached" is
        /// observable.
        ///
        /// Drives the control channel directly rather than through
        /// `POST /open-url`: the handler's guards are unit-tested above without a
        /// database, and the part with real risk is here — the `select!` arm that
        /// forwards the frame, and the receiver count the handler uses to decide
        /// whether a pane is even reachable. That count is what turns a closed window
        /// into "opened in the system browser" instead of a URL dropped on the floor.
        #[tokio::test]
        async fn a_routed_url_reaches_the_attached_socket() {
            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();

            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;

            let session = SESSIONS.lock().await.get(&sid).cloned().expect("session");
            // The socket has subscribed by the time it reports `ready`, which is what
            // the endpoint's "is anyone looking?" check relies on.
            assert_eq!(session.control.receiver_count(), 1);
            session
                .control
                .send(ServerControl::OpenUrl {
                    // Built through the parser, because the type has no other
                    // constructor — which is the point of it.
                    url: veld_core::ide::parse_web_url("https://web.dev.app.localhost/x?y=1")
                        .expect("a web url")
                        .canonical,
                })
                .expect("a subscribed socket");

            let frame = read_control(&mut ws, "open_url").await;
            assert_eq!(frame["url"], "https://web.dev.app.localhost/x?y=1");

            // With the page gone there is nothing to route to, and the endpoint can
            // tell — otherwise it would answer "opened in a pane" and drop the URL.
            drop(ws);
            let deadline = Instant::now() + Duration::from_secs(10);
            while session.control.receiver_count() != 0 && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                session.control.receiver_count(),
                0,
                "a detached session must report no listeners"
            );
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

        /// The confirmation signal: idle at the prompt, busy while a foreground
        /// job runs, idle again once it finishes. Runs through the real holder
        /// (in-process under test) and the real `/api/pty/sessions/{id}/busy`
        /// route, so it pins the whole chain from the endpoint to `tcgetpgrp`.
        #[tokio::test]
        async fn busy_tracks_a_foreground_job() {
            use axum::body::Body;
            use tower::ServiceExt;

            let addr = serve().await;
            let dir = tempfile::tempdir().unwrap();
            let sid = session_id();
            let mut ws = open(addr, &sid, dir.path(), "").await;
            read_control(&mut ws, "ready").await;

            async fn busy(uri: &str) -> bool {
                let res = routes()
                    .oneshot(
                        axum::http::Request::builder()
                            .method("GET")
                            .uri(uri)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    res.status(),
                    StatusCode::OK,
                    "busy check must be a safe GET"
                );
                let bytes = axum::body::to_bytes(res.into_body(), 64).await.unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["busy"]
                    .as_bool()
                    .expect("busy is a bool")
            }

            // Idle at the prompt: nothing to warn about.
            assert!(
                !busy(&format!("/api/pty/sessions/{sid}/busy")).await,
                "a prompt must not read as busy"
            );

            // A foreground `sleep` must read as busy while it runs. The shell
            // does not switch the foreground pgrp instantly, so poll until it
            // does rather than race it.
            ws.send(WsMessage::Binary(
                b"sleep 5; printf 'veld%s\\n' '-idle-again'\n"
                    .to_vec()
                    .into(),
            ))
            .await
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut saw_busy = false;
            while Instant::now() < deadline {
                if busy(&format!("/api/pty/sessions/{sid}/busy")).await {
                    saw_busy = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(saw_busy, "a foreground job must read as busy");

            // Once the job finishes the prompt is idle again — the signal must
            // not stick.
            read_until(&mut ws, "veld-idle-again").await;
            assert!(
                !busy(&format!("/api/pty/sessions/{sid}/busy")).await,
                "an idle prompt must not read as busy"
            );
            end_session(&sid, "test cleanup").await;
        }
    }
}
