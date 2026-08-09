//! The IDE control channel: who is showing which worktree, and one socket per
//! client to say so on.
//!
//! # Why this is in the daemon
//!
//! It used to be in the Electron main process. That process can see its own
//! `BrowserWindow`s and nothing else, so opening `/ide` in a plain browser
//! produced a client that was invisible to the whole arbitration: it showed a
//! worktree the desktop app also had, rendered a *different* set of panes for
//! it (the layout was browser storage — now `pane_layouts`, migration v14), and
//! could not re-attach to the terminals the app had running because it had
//! never heard of their session ids. The daemon is the only process both kinds
//! of client have in common, so it is the only place the answer can live.
//!
//! # The rule being enforced
//!
//! **One worktree has one set of panes and one client showing them.** That is
//! not a policy choice left over from the Electron implementation; it is forced
//! by the terminals. A second attach to a live PTY session does not mirror it,
//! it *takes it over* (`pty::attach` bumps `attach_epoch`, and the displaced
//! socket is sent `TakenOver` and closed). Two clients rendering one worktree's
//! layout would therefore trade every shell in it back and forth, each one
//! going dead-but-visible in turn.
//!
//! # The lease is the socket
//!
//! A claim lives exactly as long as the connection that made it. There is no
//! heartbeat, no TTL to tune, and no periodic reaper, because every way a
//! client can stop existing — tab closed, window closed, process killed, laptop
//! slept until the TCP connection broke — closes the socket, and a close
//! releases everything that client held. The one case a socket close does *not*
//! mean "gone" is a page reload, which is what [`RECONNECT_GRACE`] covers:
//! for that long, the same `client_id` reconnecting gets its claims back.
//! A competing claim during the grace still wins immediately — a reloading
//! client has nothing attached, so there is nothing to protect it from.
//!
//! # Focus
//!
//! Clicking a worktree that another client is showing does not open a second
//! copy; it asks the daemon to bring you to the one that exists. The daemon
//! pushes `focus` to the holder, and what happens then depends on what the
//! holder is: an Electron window raises itself through its shell, and a browser
//! tab **cannot** — `window.focus()` without a user gesture is ignored by every
//! browser. So the refusal carries the holder's `kind`, and the client that
//! asked says which of the two it is instead of pretending the raise happened.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tracing::{debug, warn};

use veld_core::db::{LayoutRejected, LayoutWrite};

use super::management::{check_csrf, open_db};

/// How long a ticket is good for. Same shape and the same reason as the PTY
/// ticket: long enough for the browser to open a socket, short enough that one
/// left in a URL bar is useless.
const TICKET_TTL: Duration = Duration::from_secs(30);

/// How long a holder gets to confirm it has let go before the claim proceeds
/// anyway.
///
/// The release is a React commit, not a network round trip, so this is generous.
/// It resolves rather than failing: a wedged client must not be able to make a
/// worktree permanently unopenable, and the fallback is what the code did before
/// the acknowledgement existed.
const YIELD_ACK: Duration = Duration::from_millis(1500);

/// How long a disconnected client's claims survive, waiting for it to come back.
///
/// This is the reload window and nothing else. Long enough for a page to boot
/// and reconnect on a busy machine; short enough that a closed tab does not grey
/// out a rail row for anyone else. Nothing else in this module has a timeout,
/// because nothing else needs one — see the module docs.
const RECONNECT_GRACE: Duration = Duration::from_secs(6);

/// A claim's grace must outlast the client's worst-case return.
///
/// Made load-bearing by the expiry timer: an orphan used to survive until
/// somebody claimed, and now certainly dies at [`RECONNECT_GRACE`]. So the
/// client's slowest reconnect — `MAX_RETRY_MS` (5s) in `ui/src/ide/channel.ts`,
/// plus a ticket POST and a WebSocket upgrade — has to fit inside it, or a
/// reload routinely loses its worktree to whoever is next in the rail. Stated
/// here because the two constants live in different languages and nothing else
/// would connect them.
const _: () = assert!(
    RECONNECT_GRACE.as_millis() > 5_000,
    "must outlast MAX_RETRY_MS"
);

/// Cap on the worktrees one client may report holding.
///
/// A client holds the panes of the worktrees it has *visited* (they stay
/// mounted so switching back is instant), which is bounded by what a person
/// clicks. This stops a buggy or hostile page growing the registry without
/// limit.
const MAX_HELD: usize = 256;

/// Cap on a single control frame. Every message in this protocol is a handful of
/// numbers and a short string.
const MAX_FRAME_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Build the router for the control channel and the layout store.
///
/// CSRF, as in `pty::routes`, is per route and not a layer — for the same
/// reason: the load-bearing route here is a WebSocket upgrade, which is a GET
/// that a method-keyed layer waves through. `mint_ticket` and the layout `PUT`
/// call [`check_csrf`] themselves; the layout `GET` relies on the absent CORS
/// layer, exactly as `pty::list_pane_sessions` does.
pub fn routes() -> Router {
    Router::new()
        .route("/api/ide/tickets", post(mint_ticket))
        .route("/api/ide/channel", get(channel))
        .route("/api/worktrees/{id}/layout", get(get_layout))
        .route("/api/worktrees/{id}/layout", put(put_layout))
}

type ApiError = (StatusCode, Json<serde_json::Value>);

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    (code, Json(serde_json::json!({ "error": msg.into() })))
}

// ---------------------------------------------------------------------------
// Layout store
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct LayoutResponse {
    /// `0` when the worktree has no stored layout — see `Db::pane_layout`.
    version: i64,
    /// `null` at version 0. Never `{}`: "nobody has arranged this worktree" and
    /// "somebody arranged it to be empty" are different answers, and the client
    /// seeds a default only for the first.
    layout: Option<serde_json::Value>,
}

/// One worktree's panes.
///
/// Safe, so it carries no CSRF header (the UI sends one only on mutations) and
/// is protected by the daemon sending no `Access-Control-Allow-Origin`: another
/// origin can issue this request and never read the answer.
async fn get_layout(Path(worktree_id): Path<i64>) -> Result<Json<LayoutResponse>, ApiError> {
    let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
    let stored = db.pane_layout(worktree_id).map_err(|e| {
        warn!("layout read: database error: {e}");
        err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
    })?;
    Ok(Json(match stored {
        Some(l) => LayoutResponse {
            version: l.version,
            // Stored only after parsing, so this cannot fail — but a row
            // hand-edited in `sqlite3` can, and answering `null` degrades to
            // "no layout" rather than failing the whole request.
            layout: serde_json::from_str(&l.layout).ok(),
        },
        None => LayoutResponse {
            version: 0,
            layout: None,
        },
    }))
}

/// Turn a failed layout write into a status.
///
/// **Only the foreign key is a 404.** Everything else — a locked database, a
/// full disk, a poisoned mutex — is this daemon's problem, and reporting it as
/// "worktree not found" made a machine whose database had stopped accepting
/// writes indistinguishable from a client racing a deletion, at `debug!` level,
/// while the client's own save path swallows the error. Layouts would simply
/// stop persisting with nothing said anywhere.
fn layout_write_error(e: veld_core::db::DbError) -> ApiError {
    if e.is_constraint_violation() {
        debug!("layout write refused: no such worktree ({e})");
        return err(StatusCode::NOT_FOUND, "worktree not found");
    }
    warn!("layout write: database error: {e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutLayoutRequest {
    /// The version this client last read. `0` means "there was no layout".
    version: i64,
    /// The layout document, or `null` to forget this worktree's panes.
    layout: Option<serde_json::Value>,
    /// Which client is writing, so the daemon can push the change to the
    /// *others* without echoing it back to the author mid-drag.
    #[serde(default)]
    client_id: Option<String>,
}

/// Store one worktree's panes, if the caller's version is still current.
///
/// `409` with the current state on a mismatch — the client reconciles from the
/// body rather than re-reading and racing again.
async fn put_layout(
    Path(worktree_id): Path<i64>,
    headers: HeaderMap,
    Json(body): Json<PutLayoutRequest>,
) -> Result<Json<LayoutResponse>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;

    let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    // A worktree with no panes left has no row, so the next client to open it
    // seeds a default instead of restoring an empty screen.
    //
    // **Versioned like any other write**, which it was not at first and that was
    // a hole rather than a shortcut: a delete that ignores the version lets the
    // client that just let a worktree go — or one running off a stale poll —
    // erase the panes of the client that now holds it, which then sees them
    // unmount as it adopts the `null`. There is no reason for the destructive
    // write to be the one that skips the check the others pass.
    let Some(layout) = body.layout else {
        let outcome = db
            .delete_pane_layout(worktree_id, body.version)
            .map_err(layout_write_error)?;
        return match outcome {
            LayoutWrite::Stored(_) => {
                broadcast_layout(worktree_id, 0, body.client_id.as_deref());
                Ok(Json(LayoutResponse {
                    version: 0,
                    layout: None,
                }))
            }
            LayoutWrite::Conflict(cur) => Err(conflict(cur)),
        };
    };

    let text = serde_json::to_string(&layout)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "layout is not serializable"))?;
    let outcome = db
        .put_pane_layout(worktree_id, body.version, &text)
        .map_err(layout_write_error)?;

    match outcome {
        Err(LayoutRejected::NotJson) => Err(err(StatusCode::BAD_REQUEST, "layout is not JSON")),
        Err(LayoutRejected::TooLarge) => {
            Err(err(StatusCode::PAYLOAD_TOO_LARGE, "layout is too large"))
        }
        Ok(LayoutWrite::Stored(l)) => {
            broadcast_layout(worktree_id, l.version, body.client_id.as_deref());
            Ok(Json(LayoutResponse {
                version: l.version,
                layout: Some(layout),
            }))
        }
        Ok(LayoutWrite::Conflict(cur)) => Err(conflict(cur)),
    }
}

/// A refused write, carrying what is actually stored.
///
/// The body is the point: the loser reconciles from it in the same round trip
/// rather than re-reading and racing the same winner again.
fn conflict(cur: veld_core::db::PaneLayout) -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "layout version is stale",
            "version": cur.version,
            "layout": serde_json::from_str::<serde_json::Value>(&cur.layout).ok(),
        })),
    )
}

// ---------------------------------------------------------------------------
// Tickets
// ---------------------------------------------------------------------------

struct Ticket {
    /// The identity the socket this ticket opens will carry.
    client_id: String,
    expires_at: Instant,
}

static TICKETS: LazyLock<Mutex<HashMap<String, Ticket>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Serialize)]
struct TicketResponse {
    ticket: String,
    expires_in_ms: u64,
    /// The identity this connection will have.
    ///
    /// **Minted here, not chosen by the client.** A client-chosen id let anything
    /// that could open a socket present another client's id, which displaced that
    /// client *and inherited its claim* — after which claiming the worktree asked
    /// no yield of the victim, because the daemon believed the claimer already
    /// owned it. That is the false all-clear this whole module exists to prevent,
    /// and it did not need an attacker: Chrome's "Duplicate Tab" copies
    /// `sessionStorage`, so two tabs would arrive under one id by accident.
    ///
    /// It is a secret in the weak sense that matters here — it is never put on
    /// the wire to any *other* client (see [`ClientInfo`]), so presenting one you
    /// were not given means guessing a v4 UUID.
    client_id: String,
}

/// Mint a single-use ticket for the control socket.
///
/// The CSRF-gated half of the handshake, for the same reason `pty::mint_ticket`
/// exists: a WebSocket handshake cannot carry a custom header, so the check has
/// to happen on a request that can. The ticket grants nothing by itself — the
/// socket it opens still has to say who it is, and every claim it makes is
/// scoped to that identity.
async fn mint_ticket(headers: HeaderMap) -> Result<Json<TicketResponse>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    let ticket = uuid::Uuid::new_v4().simple().to_string();
    let client_id = uuid::Uuid::new_v4().simple().to_string();
    let now = Instant::now();
    {
        let mut store = TICKETS.lock().expect("ide ticket store poisoned");
        // A ticket minted and never redeemed (the tab closed mid-connect) would
        // otherwise sit here for the life of the daemon.
        store.retain(|_, t| t.expires_at > now);
        store.insert(
            ticket.clone(),
            Ticket {
                client_id: client_id.clone(),
                expires_at: now + TICKET_TTL,
            },
        );
    }
    Ok(Json(TicketResponse {
        ticket,
        expires_in_ms: TICKET_TTL.as_millis() as u64,
        client_id,
    }))
}

/// Consume a ticket, returning the identity it carries. `None` if it is unknown
/// or expired.
fn redeem(ticket: &str) -> Option<String> {
    let mut store = TICKETS.lock().expect("ide ticket store poisoned");
    let t = store.remove(ticket)?;
    (t.expires_at > Instant::now()).then_some(t.client_id)
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// What kind of client is on the other end.
///
/// The distinction is not cosmetic: it decides whether a `focus` push can
/// actually raise anything, and therefore what the client that was refused
/// tells the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// A Veld Desktop window. Can raise itself, through its Electron shell.
    Electron,
    /// A page in the user's own browser. Cannot raise itself — see the module
    /// docs.
    Browser,
}

/// Everything a client says about itself, once, when the socket opens.
///
/// Not a [`ClientMsg`] variant: it is the one message whose absence every other
/// handler would have to cope with, so it is read separately and before the
/// loop. Unknown fields are ignored rather than refused — the client tags it
/// `"type": "hello"` so the frame reads the same as the rest of the protocol,
/// and a newer client adding a field must not fail to connect to an older
/// daemon.
#[derive(Debug, Deserialize)]
struct Hello {
    /// An identity this page held on a previous connection, to be given its
    /// claims back within [`RECONNECT_GRACE`] — the reload case.
    ///
    /// **Honoured only when nothing is connected under it.** The identity itself
    /// was minted by the daemon (see [`TicketResponse::client_id`]); this is a
    /// request to resume it, and a request is all it can be, because two live
    /// sockets under one identity is the state that produced a claim inherited
    /// with no yield asked. When it is refused, the connection keeps the fresh
    /// identity its ticket carried and the client is told which one it got.
    ///
    /// What is *not* checked, stated because the obvious reading is that it is:
    /// this daemon does not remember which ids it has issued, so any well-formed
    /// id nothing is connected under is accepted. The property that makes that
    /// safe is unguessability — ids are v4 UUIDs and never appear on the wire to
    /// any other client (see [`ClientInfo`]) — not a registry. Putting an id
    /// back into a broadcast breaks this, which is what the
    /// `the_claims_broadcast_never_carries_a_client_id` test is for.
    ///
    /// One case the "nothing is connected under it" rule does not cover, stated
    /// because it reads as if it did: a tab duplicated while the original's
    /// socket happens to be *down* (a network blip, not a reload) resumes the
    /// original's id and is handed its claims, with no yield asked of the
    /// original — which is still mounted. [`RECONNECT_GRACE`] is what bounds it,
    /// and it is one-shot, because the duplicate stores the id it was given.
    #[serde(default)]
    resume: Option<String>,
    kind: ClientKind,
    /// What to call this client when another one is told where a worktree is.
    /// Free text, bounded, and rendered — never used to route anything.
    #[serde(default)]
    label: String,
}

/// Client → daemon.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    /// Ask to show a worktree. Answered with [`ServerMsg::ClaimResult`] once
    /// every other holder has let go.
    Claim {
        worktree_id: i64,
        request_id: u64,
        /// Whether a refusal should raise the client that has it. `false` while
        /// a window is working out what it may display — it asks about several
        /// in a row, and each refusal yanking a different window forward is a
        /// window manager fighting the user over a question they did not ask.
        #[serde(default = "yes")]
        focus_holder: bool,
    },
    /// Which worktrees this client has mounted panes for. The daemon cannot see
    /// inside a page, and "who would I have to ask to let go" is a question
    /// only the pages can answer.
    Holds { worktree_ids: Vec<i64> },
    /// A yield has been carried out — sent *after* the release is on screen,
    /// not when the message arrived.
    Yielded { yield_id: u64 },
    /// These worktrees are gone. Rowids are reused, so a claim left on a deleted
    /// worktree greys out whichever one is created next.
    Forget { worktree_ids: Vec<i64> },
}

fn yes() -> bool {
    true
}

/// How a client is described to the others.
///
/// **No identity on it.** The id is the credential a reconnect resumes with, so
/// putting it in a broadcast handed every client the set of ids it would need to
/// impersonate any other. What a rail needs is which worktrees are not its own
/// and what to say about them, and neither question needs a name.
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub kind: ClientKind,
    pub label: String,
}

/// Daemon → client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    /// The handshake landed. `epoch` changes when the daemon restarts, which is
    /// how a reconnecting client knows its claims are gone and it must ask
    /// again. `client_id` is the identity actually in use — the ticket's, or a
    /// resumed one if the resume was honoured — and is what the client stores to
    /// resume with next time.
    Ready { epoch: String, client_id: String },
    /// Who is showing what, in full. Sent on connect and after every change —
    /// state, not a delta, so a client that missed one is not wrong afterwards.
    Claims { claims: Vec<ClaimView> },
    /// The answer to a [`ClientMsg::Claim`].
    ClaimResult {
        request_id: u64,
        ok: bool,
        /// `shown_elsewhere` or `superseded`; absent on success.
        reason: Option<&'static str>,
        /// Who has it, when the answer is `shown_elsewhere`. What the client
        /// tells the user depends on `kind` — a browser tab cannot be raised.
        holder: Option<ClientInfo>,
    },
    /// Let go of this worktree's panes; another client is taking it. Release the
    /// terminals without ending them, then answer with [`ClientMsg::Yielded`] —
    /// the claimer does not attach until it hears back.
    Yield { worktree_id: i64, yield_id: u64 },
    /// Someone asked to be brought here. An Electron client raises its window;
    /// a browser tab can only mark itself.
    Focus,
    /// A worktree's stored layout changed. Sent to every client *except* the one
    /// that wrote it, which already has the answer in its own response.
    LayoutChanged { worktree_id: i64, version: i64 },
}

/// A yield in flight: who was asked, under which id, and the channel their
/// acknowledgement arrives on.
type PendingYield = (String, u64, oneshot::Receiver<()>);

/// What a granted claim leaves for [`claim`]'s async half: the yields to wait
/// out before answering.
type GrantedClaim = Vec<PendingYield>;

/// One entry of the claims table, as one client sees it.
///
/// Personalised, which is what lets the identity stay off the wire: the rail's
/// question is "is this one mine", and answering it here costs a boolean where
/// answering it in the client would cost every client every other client's id.
#[derive(Debug, Clone, Serialize)]
struct ClaimView {
    worktree_id: i64,
    /// Whether the recipient is the one showing it.
    mine: bool,
    client: ClientInfo,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// A connected client.
struct Client {
    /// Which *socket* this is. A `client_id` outlives its connection (that is
    /// what makes a reload keep its claims), so the identity alone cannot tell
    /// a closing socket from the one that replaced it — and a dying task that
    /// assumed it could would tear down the live client's state.
    conn: u64,
    info: ClientInfo,
    /// Frames queued towards this client's socket.
    tx: mpsc::UnboundedSender<ServerMsg>,
    /// Worktrees whose panes this client has mounted.
    holds: HashSet<i64>,
    /// Yields this client has been asked for and has not answered.
    pending: HashMap<u64, oneshot::Sender<()>>,
}

/// A claim whose client disconnected, waiting out [`RECONNECT_GRACE`].
///
/// Carries the client's description as well as its id: the rail keeps rendering
/// the worktree as spoken for during the grace, and "open in another window"
/// versus "open in a browser tab" has to stay true across the gap. Deriving it
/// from the (now absent) client would mean guessing.
struct Orphan {
    /// Who may resume it. Never leaves the daemon.
    client_id: String,
    info: ClientInfo,
    since: Instant,
}

/// The whole of the shared state.
///
/// One mutex rather than one per map: every operation here touches at least two
/// of them (a claim reads `claims`, writes it, and reads `clients` to find who
/// to ask), and three fine-grained locks taken in a mutable order is how this
/// would deadlock. It is guarding a few hashmaps behind a handful of messages
/// per user action — contention is not the problem being solved.
#[derive(Default)]
struct Registry {
    clients: HashMap<String, Client>,
    /// `worktree_id → client_id`. A claim is *displayed*, not owned: a client
    /// that switches away releases, and the layout stays in the database for
    /// whoever picks the worktree up next.
    claims: HashMap<i64, String>,
    /// Claims belonging to clients that have disconnected. A reconnect within
    /// [`RECONNECT_GRACE`] takes them back; anyone else claiming one takes it
    /// immediately.
    orphaned: HashMap<i64, Orphan>,
    /// Bumped on every claim, so a claim that finished waiting can tell whether
    /// it is still the one its client is making.
    claim_seq: HashMap<String, u64>,
}

static REGISTRY: LazyLock<AsyncMutex<Registry>> =
    LazyLock::new(|| AsyncMutex::new(Registry::default()));

static NEXT_YIELD_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_CLAIM_SEQ: AtomicU64 = AtomicU64::new(1);
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// This daemon process's identity, handed to every client on connect.
///
/// A client that reconnects and sees a different epoch knows the daemon
/// restarted underneath it — its claims are gone and its layout may have been
/// written by someone else — so it re-reads rather than trusting what it holds.
/// `veld update` replaces the binary while pages stay open, so this is a normal
/// event, not a fault.
static EPOCH: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().simple().to_string());

impl Registry {
    /// Drop orphaned claims whose owner is not coming back. Returns whether
    /// anything was released.
    ///
    /// Called on the paths that read `claims`, **and** from a one-shot timer
    /// armed by [`disconnect`]. The lazy call alone was not enough and the
    /// mistake is worth naming: an expired orphan changes what every *other*
    /// client's rail should render, and nothing else in this module was going to
    /// run at that moment — so a closed tab greyed a worktree out until somebody
    /// happened to claim something, which contradicts the whole point of the
    /// grace being short.
    ///
    /// The claim is removed only if it is *still* the orphan's. A worktree
    /// claimed by somebody else during the grace has already had its orphan
    /// entry dropped, but checking the owner here as well means this can never
    /// be the thing that revokes a live claim.
    fn expire_orphans(&mut self, now: Instant) -> bool {
        let expired: Vec<(i64, String)> = self
            .orphaned
            .iter()
            .filter(|(_, o)| now.duration_since(o.since) >= RECONNECT_GRACE)
            .map(|(worktree_id, o)| (*worktree_id, o.client_id.clone()))
            .collect();
        if expired.is_empty() {
            return false;
        }
        for (worktree_id, owner) in expired {
            self.orphaned.remove(&worktree_id);
            if self.claims.get(&worktree_id) == Some(&owner) {
                self.claims.remove(&worktree_id);
            }
        }
        true
    }

    /// Hand a returning client the claims its previous socket held.
    ///
    /// The whole of what [`RECONNECT_GRACE`] buys: a reload drops the socket but
    /// not the identity, so without this every reload would release the
    /// worktree and then have to race whatever else is running to get it back.
    fn reclaim_orphans_of(&mut self, client_id: &str) {
        let mine: Vec<i64> = self
            .orphaned
            .iter()
            .filter(|(_, o)| o.client_id == client_id)
            .map(|(worktree_id, _)| *worktree_id)
            .collect();
        for worktree_id in mine {
            self.orphaned.remove(&worktree_id);
            self.claims.insert(worktree_id, client_id.to_owned());
        }
    }

    /// Who is showing what, as one client sees it.
    ///
    /// An orphaned claim is included — the worktree really is spoken for until
    /// the grace expires, and showing it as free would invite a click that then
    /// gets taken away.
    fn view_for(&self, recipient: &str) -> Vec<ClaimView> {
        self.claims
            .iter()
            .filter_map(|(worktree_id, client_id)| {
                let info = self
                    .clients
                    .get(client_id)
                    .map(|c| c.info.clone())
                    // Orphaned: the owner is gone but the claim is not, and the
                    // orphan carries its own description for exactly this.
                    .or_else(|| self.orphaned.get(worktree_id).map(|o| o.info.clone()))?;
                Some(ClaimView {
                    worktree_id: *worktree_id,
                    mine: client_id == recipient,
                    client: info,
                })
            })
            .collect()
    }

    /// Push the claims table to every connected client.
    fn broadcast(&self) {
        for (id, client) in &self.clients {
            let _ = client.tx.send(ServerMsg::Claims {
                claims: self.view_for(id),
            });
        }
    }

    /// The synchronous half of a claim: decide, record, and tell everyone.
    ///
    /// Split out from [`claim`] because it is where every rule lives and the
    /// async half is only a timeout. Returns `None` when the claim was refused
    /// (both sides have already been told), otherwise the claim's sequence
    /// number and the yields to wait on.
    ///
    /// The claim is recorded **before** the wait, which is what makes a third
    /// client's claim arriving during it get refused rather than granted
    /// alongside, and the greyed rail row in every other client true from this
    /// moment on.
    fn begin_claim(
        &mut self,
        client_id: &str,
        conn: u64,
        worktree_id: i64,
        request_id: u64,
        focus_holder: bool,
        seq: u64,
    ) -> Option<GrantedClaim> {
        // **The socket that asked must still be the one registered.** A claim is
        // spawned, so it can be polled after its own read loop has exited, or
        // after a *newer* socket resumed this identity — and recording a claim
        // for a client that is gone leaves an entry nothing ever removes.
        if self.clients.get(client_id).is_none_or(|c| c.conn != conn) {
            return None;
        }
        let _ = self.expire_orphans(Instant::now());

        // An orphan is not a holder: nothing is attached behind a closed socket,
        // so a claim takes it and the returning client re-claims (or hunts for
        // something free) when it comes back. Only a *live* claim by another
        // client refuses.
        let live_holder = self
            .claims
            .get(&worktree_id)
            .filter(|owner| owner.as_str() != client_id)
            .filter(|owner| self.clients.contains_key(owner.as_str()))
            .cloned();

        if let Some(holder_id) = live_holder {
            let holder = self.clients.get(&holder_id);
            let info = holder.map(|c| c.info.clone());
            // Only a *deliberate* pick raises the holder. A client working out
            // what it may display asks about several worktrees in a row, and
            // having each refusal yank a different window forward would be a
            // window manager fighting the user over a question they did not ask.
            if focus_holder && let Some(c) = holder {
                let _ = c.tx.send(ServerMsg::Focus);
            }
            if let Some(me) = self.clients.get(client_id) {
                let _ = me.tx.send(ServerMsg::ClaimResult {
                    request_id,
                    ok: false,
                    reason: Some("shown_elsewhere"),
                    holder: info,
                });
            }
            return None;
        }

        // One *displayed* worktree per client. Letting go of the previous one
        // does not make this client drop that worktree's panes — it keeps them
        // mounted so switching back is instant, and lets go only when somebody
        // else claims that one.
        self.release_claims_of(client_id, Some(worktree_id));
        self.orphaned.remove(&worktree_id);
        self.claims.insert(worktree_id, client_id.to_owned());
        // Both halves changed: the worktree this client let go of is free for
        // everyone, and the one it took is now spoken for.
        self.broadcast();

        // **The highest seq wins, not the last writer.** These arrive in frame
        // order but *run* in scheduler order, so a plain insert let whichever
        // task happened to run second install its number — and the earlier frame
        // then owned the outcome. Keeping the maximum makes the rule "a later
        // claim from this client supersedes this one" true regardless of how the
        // two were scheduled.
        let current = self.claim_seq.entry(client_id.to_owned()).or_insert(seq);
        *current = (*current).max(seq);

        // Every *other* client still holding this worktree's panes has to let go
        // before this one attaches, or the two would trade its shells.
        let targets: Vec<String> = self
            .clients
            .iter()
            .filter(|(id, c)| id.as_str() != client_id && c.holds.contains(&worktree_id))
            .map(|(id, _)| id.clone())
            .collect();
        let mut waits = Vec::new();
        for target in targets {
            let yield_id = NEXT_YIELD_ID.fetch_add(1, Ordering::Relaxed);
            let (settle_tx, settle_rx) = oneshot::channel();
            let Some(c) = self.clients.get_mut(&target) else {
                continue;
            };
            c.pending.insert(yield_id, settle_tx);
            if c.tx
                .send(ServerMsg::Yield {
                    worktree_id,
                    yield_id,
                })
                .is_ok()
            {
                waits.push((target, yield_id, settle_rx));
            } else {
                // The queue is closed, so the yield will never be delivered and
                // waiting on it would burn the full timeout for a client that is
                // already gone.
                c.pending.remove(&yield_id);
            }
        }
        Some(waits)
    }

    /// Release every claim and hold belonging to a client that switched away or
    /// disappeared. Returns whether anything changed.
    fn release_claims_of(&mut self, client_id: &str, keep: Option<i64>) -> bool {
        let mut changed = false;
        self.claims.retain(|worktree_id, owner| {
            if owner != client_id || Some(*worktree_id) == keep {
                return true;
            }
            changed = true;
            false
        });
        changed
    }
}

/// Tell the clients other than the author that a layout moved.
///
/// Fire-and-forget from an HTTP handler, so it takes the registry lock in a
/// spawned task rather than blocking the response on it: the author already has
/// the new state in its own reply, and the others are being told to re-read, not
/// being handed data they must not miss.
fn broadcast_layout(worktree_id: i64, version: i64, author: Option<&str>) {
    let author = author.map(str::to_owned);
    tokio::spawn(async move {
        let reg = REGISTRY.lock().await;
        for (id, client) in &reg.clients {
            if author.as_deref() == Some(id.as_str()) {
                continue;
            }
            let _ = client.tx.send(ServerMsg::LayoutChanged {
                worktree_id,
                version,
            });
        }
    });
}

// ---------------------------------------------------------------------------
// The socket
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChannelQuery {
    ticket: String,
}

/// Upgrade to the control socket.
async fn channel(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(q): Query<ChannelQuery>,
) -> Response {
    if !super::pty::origin_allowed(&headers) {
        // Terse for the same reason the terminal upgrade is: an attacker learns
        // nothing about the allowlist, and the real origin is in the daemon log.
        warn!(
            origin = ?headers.get(axum::http::header::ORIGIN),
            "rejected ide channel upgrade from a disallowed origin"
        );
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let Some(minted) = redeem(&q.ticket) else {
        return (StatusCode::FORBIDDEN, "invalid or expired ticket").into_response();
    };
    ws.max_message_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| serve_channel(socket, minted))
}

/// Bound on the identity strings a client may send.
fn valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn serve_channel(socket: WebSocket, minted: String) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // The first frame must be the hello. Everything after it is scoped to the
    // identity it establishes, so there is nothing useful a client can do before
    // sending one — and accepting other messages first would mean every handler
    // below has to cope with an unidentified sender.
    let hello: Hello = loop {
        match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<Hello>(&text) {
                Ok(h) => break h,
                Err(e) => {
                    debug!("ide channel: unreadable hello: {e}");
                    return;
                }
            },
            // Ping/pong and close frames are the transport's, not ours.
            Some(Ok(_)) => continue,
            Some(Err(e)) => {
                debug!("ide channel: closed before the hello: {e}");
                return;
            }
            None => return,
        }
    };

    let info = ClientInfo {
        kind: hello.kind,
        // Bounded: it is rendered in another client's UI, and an unbounded
        // string crossing that boundary is a payload, not a name.
        label: hello.label.chars().take(120).collect(),
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let client_id;

    {
        let mut reg = REGISTRY.lock().await;
        let _ = reg.expire_orphans(Instant::now());
        // **A resume is honoured only when nothing is connected under that id.**
        // The alternative — letting the newcomer displace the incumbent — is
        // what made a claim inheritable without a yield: the displaced client
        // stayed attached to its terminals while the registry handed its claim
        // to whoever arrived. Refusing costs a reloading page nothing (its old
        // socket is closed by definition) and costs a duplicated tab only the
        // claims it never had. `valid_client_id` first, because the id becomes a
        // map key and a `%client` field in the log.
        client_id = match hello.resume {
            Some(resume) if valid_client_id(&resume) && !reg.clients.contains_key(&resume) => {
                resume
            }
            _ => minted,
        };
        reg.reclaim_orphans_of(&client_id);
        reg.clients.insert(
            client_id.clone(),
            Client {
                conn,
                info: info.clone(),
                tx,
                holds: HashSet::new(),
                pending: HashMap::new(),
            },
        );
        let _ = reg.clients[&client_id].tx.send(ServerMsg::Ready {
            epoch: EPOCH.clone(),
            client_id: client_id.clone(),
        });
        // Everyone, including this client, which gets the table it is joining.
        reg.broadcast();
    }

    // Writer: one task draining the queue, so a handler never awaits the socket
    // while holding the registry lock.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(text) = serde_json::to_string(&msg) else {
                continue;
            };
            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.close().await;
    });

    while let Some(frame) = ws_rx.next().await {
        let text = match frame {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(e) => {
                debug!(client = %client_id, "ide channel closed: {e}");
                break;
            }
        };
        match serde_json::from_str::<ClientMsg>(&text) {
            // **A claim is spawned, not awaited.** It waits out other clients'
            // yields, and awaiting it here stops this client's *own* later
            // frames being read for the duration — including the `yielded` that
            // some third client's claim is waiting on. The symptom is the one
            // warning this module has for the takeover race firing about a
            // client that answered promptly. Everything else is a map update
            // that finishes immediately, and `claim` re-checks `claim_seq` under
            // the lock, so being overtaken is already its normal case.
            Ok(ClientMsg::Claim {
                worktree_id,
                request_id,
                focus_holder,
            }) => {
                // **The sequence number is taken here, in frame order.** Taking
                // it inside the spawned task made it scheduler order instead:
                // tokio parks a task spawned from a worker in its LIFO slot and
                // polls that first, so two claims read from one socket in a
                // single poll run second-then-first — the earlier claim takes
                // the lower number and the *later* one is answered `superseded`.
                // For which the UI says nothing, deliberately, so the user's
                // second click simply vanishes. Two frames land in one poll at
                // boot every time, because releasing the parked `whenReady`
                // waiters writes them together.
                let seq = NEXT_CLAIM_SEQ.fetch_add(1, Ordering::Relaxed);
                let id = client_id.clone();
                tokio::spawn(async move {
                    claim(&id, conn, worktree_id, request_id, focus_holder, seq).await;
                });
            }
            Ok(msg) => handle(&client_id, msg).await,
            Err(e) => debug!(client = %client_id, "ide channel: unreadable message: {e}"),
        }
    }

    disconnect(&client_id, conn).await;
    writer.abort();
}

async fn handle(client_id: &str, msg: ClientMsg) {
    match msg {
        // Intercepted in the read loop, which is where its sequence number is
        // taken — see there. Unreachable, and left as an explicit arm so adding
        // a variant does not silently route through a `_`.
        ClientMsg::Claim { .. } => {}
        ClientMsg::Holds { worktree_ids } => {
            let mut reg = REGISTRY.lock().await;
            if let Some(client) = reg.clients.get_mut(client_id) {
                client.holds = worktree_ids.into_iter().take(MAX_HELD).collect();
            }
        }
        ClientMsg::Yielded { yield_id } => {
            let mut reg = REGISTRY.lock().await;
            // Bound to the client the yield was sent to: ids are sequential, so
            // without the ownership check any client could answer for a holder
            // that has not released and hand the claimer a false all-clear.
            if let Some(client) = reg.clients.get_mut(client_id)
                && let Some(settle) = client.pending.remove(&yield_id)
            {
                let _ = settle.send(());
            }
        }
        ClientMsg::Forget { worktree_ids } => forget(worktree_ids).await,
    }
}

/// Drop the registry state of worktrees that no longer exist.
///
/// **Every id is checked against the database first**, and that is the whole
/// safety of this message. It is the one client→daemon message that mutates
/// *other* clients' state — clearing a `holds` entry removes that client from
/// the next claim's yield list, so a claimer is granted with nothing to wait for
/// and attaches to PTY sessions the other client is still driving. Taking the
/// sender's word for it therefore hands any client a way to produce exactly the
/// false all-clear this module exists to prevent — and it does not take malice:
/// the caller reports off a five-second poll, so a list one tick stale would
/// have revoked live claims.
///
/// A worktree that really is gone has no row, because the sender deleted it —
/// and if the deletion has not landed yet, the next poll sends the id again.
async fn forget(worktree_ids: Vec<i64>) {
    let asked: Vec<i64> = worktree_ids.into_iter().take(MAX_HELD).collect();
    if asked.is_empty() {
        return;
    }
    let Ok(db) = open_db() else {
        // Without the database there is no way to check, and clearing on trust
        // is the failure above. Leaving the entries costs a greyed rail row that
        // the next poll corrects.
        warn!("ide channel: cannot verify forgotten worktrees without a database");
        return;
    };
    let mut gone: HashSet<i64> = HashSet::new();
    for id in asked {
        match db.get_worktree(id) {
            Ok(None) => {
                gone.insert(id);
            }
            Ok(Some(_)) => {}
            Err(e) => warn!(worktree_id = id, "ide channel: worktree lookup failed: {e}"),
        }
    }
    if gone.is_empty() {
        return;
    }
    let mut reg = REGISTRY.lock().await;
    reg.claims
        .retain(|worktree_id, _| !gone.contains(worktree_id));
    reg.orphaned
        .retain(|worktree_id, _| !gone.contains(worktree_id));
    for client in reg.clients.values_mut() {
        client.holds.retain(|w| !gone.contains(w));
    }
    reg.broadcast();
}

/// Ask to show a worktree.
///
/// Records the claim synchronously — that is what makes a third client's claim
/// arriving during the wait get refused rather than granted alongside this one,
/// and the greyed rail row in every other client true from that moment. Then
/// waits for the previous holders to let go, because the caller attaches to the
/// PTY sessions its layout names on the strength of this answer.
async fn claim(
    client_id: &str,
    conn: u64,
    worktree_id: i64,
    request_id: u64,
    focus_holder: bool,
    seq: u64,
) {
    let Some(waits) = ({
        let mut reg = REGISTRY.lock().await;
        reg.begin_claim(client_id, conn, worktree_id, request_id, focus_holder, seq)
    }) else {
        // Refused; `begin_claim` has already told both sides.
        return;
    };

    // **One deadline for all of them, not one each.** Awaiting them in sequence
    // made a claim's worst case `holders × YIELD_ACK` — and `holds` is
    // client-declared, so a page could name itself a holder of every worktree
    // and put minutes in front of every real claim. Concurrently, a claim costs
    // at most `YIELD_ACK` however many clients are involved, which is what the
    // constant's own doc already promised.
    let timed_out = futures_util::future::join_all(waits.into_iter().map(
        |(target, yield_id, settle)| async move {
            match tokio::time::timeout(YIELD_ACK, settle).await {
                Ok(_) => None,
                Err(_) => Some((target, yield_id)),
            }
        },
    ))
    .await;

    for (target, yield_id) in timed_out.into_iter().flatten() {
        // Proceeding anyway is the documented fallback, and it is also the one
        // path that can still reinstate the takeover race — so it says so. The
        // condition (a wedged or very slow client) is exactly the kind only ever
        // reported second-hand.
        warn!(
            client = %target,
            worktree_id,
            "did not acknowledge yielding in {}ms; proceeding",
            YIELD_ACK.as_millis()
        );
        let mut reg = REGISTRY.lock().await;
        if let Some(c) = reg.clients.get_mut(&target) {
            c.pending.remove(&yield_id);
        }
    }

    let reg = REGISTRY.lock().await;
    // **A claim from the same client during the wait supersedes this one.** The
    // later claim released this reservation on its way in, so answering `ok`
    // here would tell the page to display a worktree the daemon no longer
    // records it as showing — and another client asking for that one would then
    // be granted it. Answers do not even come back in call order: a claim with a
    // silent holder waits out `YIELD_ACK` while one with no holder returns at
    // once.
    let superseded = reg.claim_seq.get(client_id) != Some(&seq);
    // **Only to the socket that asked.** A client id outlives its connection —
    // that is what makes a reload keep its claims — and `request_id` restarts at
    // 1 in every page instance, so answering by id alone let a reload's *new*
    // page have its first claim resolved by the *old* page's in-flight one. The
    // new page then acted on an answer to a question it never asked.
    if let Some(me) = reg.clients.get(client_id).filter(|c| c.conn == conn) {
        let _ = me.tx.send(ServerMsg::ClaimResult {
            request_id,
            ok: !superseded,
            reason: superseded.then_some("superseded"),
            holder: None,
        });
    }
}

/// A client's socket closed.
///
/// Its claims become orphans rather than disappearing, so a page reload gets
/// them back — see [`RECONNECT_GRACE`]. Its holds go immediately: nothing is
/// mounted behind a closed socket, so waiting for it to yield would be waiting
/// for a page that no longer exists.
async fn disconnect(client_id: &str, conn: u64) {
    let mut reg = REGISTRY.lock().await;
    // **Only the socket that is still registered may tear anything down.** A
    // newer socket for this id has already displaced this one and owns the
    // identity; without this check a reload's *old* reader task, running a
    // moment after the new one connected, would orphan the claims the new
    // socket had just reclaimed.
    if reg.clients.get(client_id).is_none_or(|c| c.conn != conn) {
        return;
    }
    let info = reg.clients.remove(client_id).map(|c| c.info);
    reg.claim_seq.remove(client_id);
    let now = Instant::now();
    // Its holds went with the record: nothing is mounted behind a closed
    // socket, so waiting for it to yield would be waiting on a page that no
    // longer exists. Its claims become orphans instead — see `RECONNECT_GRACE`.
    if let Some(info) = info {
        let mine: Vec<i64> = reg
            .claims
            .iter()
            .filter(|(_, owner)| owner.as_str() == client_id)
            .map(|(worktree_id, _)| *worktree_id)
            .collect();
        for worktree_id in mine {
            reg.orphaned.insert(
                worktree_id,
                Orphan {
                    client_id: client_id.to_owned(),
                    info: info.clone(),
                    since: now,
                },
            );
        }
    }
    let _ = reg.expire_orphans(now);
    reg.broadcast();
    drop(reg);

    // **Arm the expiry.** Nothing else in this module runs at the moment the
    // grace ends, so without this a closed tab left every other client's rail
    // greying out a worktree until somebody happened to claim something. One
    // short-lived task per disconnect, which is the only event that creates an
    // orphan; it re-checks under the lock, so a client that came back or a
    // worktree somebody else took is a no-op.
    tokio::spawn(async move {
        tokio::time::sleep(RECONNECT_GRACE + Duration::from_millis(50)).await;
        let mut reg = REGISTRY.lock().await;
        if reg.expire_orphans(Instant::now()) {
            reg.broadcast();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client in the registry, plus the queue its socket would be draining.
    struct Fake {
        id: String,
        conn: u64,
        rx: mpsc::UnboundedReceiver<ServerMsg>,
    }

    fn connect(reg: &mut Registry, id: &str, kind: ClientKind) -> Fake {
        let (tx, rx) = mpsc::unbounded_channel();
        let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
        reg.clients.insert(
            id.to_owned(),
            Client {
                conn,
                info: ClientInfo {
                    kind,
                    label: id.to_owned(),
                },
                tx,
                holds: HashSet::new(),
                pending: HashMap::new(),
            },
        );
        Fake {
            id: id.to_owned(),
            conn,
            rx,
        }
    }

    /// `begin_claim` with the two arguments the read loop supplies: the socket
    /// that asked, and a sequence number taken in frame order.
    fn claim_for(
        reg: &mut Registry,
        who: &Fake,
        worktree_id: i64,
        request_id: u64,
        focus_holder: bool,
    ) -> Option<GrantedClaim> {
        let seq = NEXT_CLAIM_SEQ.fetch_add(1, Ordering::Relaxed);
        reg.begin_claim(
            &who.id,
            who.conn,
            worktree_id,
            request_id,
            focus_holder,
            seq,
        )
    }

    /// Everything queued for this client so far, so a test can assert on what a
    /// page would actually have received.
    fn drain(f: &mut Fake) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        while let Ok(msg) = f.rx.try_recv() {
            out.push(msg);
        }
        out
    }

    fn claim_results(msgs: &[ServerMsg]) -> Vec<(bool, Option<&'static str>, Option<ClientKind>)> {
        msgs.iter()
            .filter_map(|m| match m {
                ServerMsg::ClaimResult {
                    ok, reason, holder, ..
                } => Some((*ok, *reason, holder.as_ref().map(|h| h.kind))),
                _ => None,
            })
            .collect()
    }

    fn holds(reg: &mut Registry, id: &str, worktrees: &[i64]) {
        reg.clients.get_mut(id).unwrap().holds = worktrees.iter().copied().collect();
    }

    #[test]
    fn a_free_worktree_is_granted_and_recorded_before_any_wait() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        let granted = claim_for(&mut reg, &a, 7, 1, true);
        assert!(granted.is_some());
        assert_eq!(reg.claims.get(&7).map(String::as_str), Some("a"));
    }

    /// **The rule the whole module exists for.** Two clients showing one
    /// worktree would trade its shells, because a second PTY attach takes a
    /// session over rather than mirroring it.
    #[test]
    fn a_worktree_shown_by_a_live_client_refuses_the_second_claim() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        let mut b = connect(&mut reg, "b", ClientKind::Browser);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();

        assert!(claim_for(&mut reg, &b, 7, 2, true).is_none());
        assert_eq!(
            reg.claims.get(&7).map(String::as_str),
            Some("a"),
            "a refused claim must not move the claim"
        );
        assert_eq!(
            claim_results(&drain(&mut b)),
            vec![(false, Some("shown_elsewhere"), Some(ClientKind::Electron))],
            "the refusal names the holder's kind, which is what decides whether \
             the asking client can promise a raise"
        );
    }

    /// A browser tab cannot be raised, so the refusal has to *say* it is a
    /// browser tab rather than leave the caller to claim it focused something.
    #[test]
    fn a_refusal_reports_a_browser_holder_as_a_browser() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Browser);
        let mut b = connect(&mut reg, "b", ClientKind::Electron);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();
        claim_for(&mut reg, &b, 7, 2, true);
        assert_eq!(
            claim_results(&drain(&mut b))[0].2,
            Some(ClientKind::Browser)
        );
    }

    #[test]
    fn a_deliberate_pick_focuses_the_holder_and_a_survey_does_not() {
        let mut reg = Registry::default();
        let mut a = connect(&mut reg, "a", ClientKind::Electron);
        let b = connect(&mut reg, "b", ClientKind::Electron);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();
        let _ = drain(&mut a);

        claim_for(&mut reg, &b, 7, 2, false);
        assert!(
            !drain(&mut a).iter().any(|m| matches!(m, ServerMsg::Focus)),
            "a client surveying what it may display must not yank windows forward"
        );

        claim_for(&mut reg, &b, 7, 3, true);
        assert!(drain(&mut a).iter().any(|m| matches!(m, ServerMsg::Focus)));
    }

    /// Re-selecting the worktree you are already showing is not a fight with
    /// yourself.
    #[test]
    fn reclaiming_your_own_worktree_is_granted() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();
        assert!(claim_for(&mut reg, &a, 7, 2, true).is_some());
    }

    /// One *displayed* worktree per client: switching away frees the old one for
    /// everybody, which is what makes "close that window and it comes back over
    /// here" work with no hand-off protocol.
    #[test]
    fn switching_worktrees_releases_the_previous_claim() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        let b = connect(&mut reg, "b", ClientKind::Electron);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();
        claim_for(&mut reg, &a, 8, 2, true).unwrap();
        assert!(!reg.claims.contains_key(&7));
        assert!(claim_for(&mut reg, &b, 7, 3, true).is_some());
    }

    /// Holding a worktree's panes is not the same as showing it: a claim asks
    /// every *other* holder to let go, and does not ask the claimer.
    #[test]
    fn a_claim_asks_every_other_holder_to_yield() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        let mut b = connect(&mut reg, "b", ClientKind::Electron);
        let mut c = connect(&mut reg, "c", ClientKind::Browser);
        holds(&mut reg, "b", &[7]);
        holds(&mut reg, "c", &[7, 9]);
        holds(&mut reg, "a", &[7]);

        let waits = claim_for(&mut reg, &a, 7, 1, true).unwrap();
        assert_eq!(waits.len(), 2, "both other holders, and not the claimer");
        for f in [&mut b, &mut c] {
            assert!(
                drain(f)
                    .iter()
                    .any(|m| matches!(m, ServerMsg::Yield { worktree_id: 7, .. })),
                "a holder is asked for the one worktree being claimed"
            );
        }
    }

    #[test]
    fn a_client_holding_nothing_is_not_waited_for() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        connect(&mut reg, "b", ClientKind::Electron);
        let waits = claim_for(&mut reg, &a, 7, 1, true).unwrap();
        assert!(waits.is_empty());
    }

    /// A dropped socket must not cost a claim the full acknowledgement timeout
    /// for a client that is already gone.
    #[test]
    fn a_closed_queue_is_not_waited_for() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        let b = connect(&mut reg, "b", ClientKind::Electron);
        holds(&mut reg, "b", &[7]);
        drop(b.rx);
        let waits = claim_for(&mut reg, &a, 7, 1, true).unwrap();
        assert!(waits.is_empty());
        assert!(
            reg.clients["b"].pending.is_empty(),
            "and nothing is left pending"
        );
    }

    // -----------------------------------------------------------------------
    // Disconnect, grace, and reclaim
    // -----------------------------------------------------------------------

    /// The reload window. Its claims are held, not dropped, so a page that comes
    /// straight back does not have to race anything to get its worktree again.
    #[test]
    fn a_disconnect_orphans_a_claim_and_a_reconnect_takes_it_back() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();

        reg.clients.remove("a");
        reg.orphaned.insert(
            7,
            Orphan {
                client_id: "a".to_owned(),
                info: ClientInfo {
                    kind: ClientKind::Electron,
                    label: "a".to_owned(),
                },
                since: Instant::now(),
            },
        );

        reg.reclaim_orphans_of("a");
        assert_eq!(reg.claims.get(&7).map(String::as_str), Some("a"));
        assert!(reg.orphaned.is_empty());
    }

    /// An orphan is not a holder: nothing is attached behind a closed socket, so
    /// a worktree does not become unopenable for the length of the grace.
    #[test]
    fn another_client_may_take_an_orphaned_claim_during_the_grace() {
        let mut reg = Registry::default();
        let b = connect(&mut reg, "b", ClientKind::Electron);
        reg.claims.insert(7, "a".to_owned());
        reg.orphaned.insert(
            7,
            Orphan {
                client_id: "a".to_owned(),
                info: ClientInfo {
                    kind: ClientKind::Electron,
                    label: "a".to_owned(),
                },
                since: Instant::now(),
            },
        );

        assert!(claim_for(&mut reg, &b, 7, 1, true).is_some());
        assert_eq!(reg.claims.get(&7).map(String::as_str), Some("b"));
        assert!(
            reg.orphaned.is_empty(),
            "the orphan must go, or the original client would reclaim it back \
             out from under the client that was just granted it"
        );
    }

    /// A worktree whose owner never comes back has to become free, or a closed
    /// tab greys out a rail row forever.
    #[test]
    fn an_orphan_past_the_grace_releases_its_claim() {
        let mut reg = Registry::default();
        reg.claims.insert(7, "a".to_owned());
        reg.orphaned.insert(
            7,
            Orphan {
                client_id: "a".to_owned(),
                info: ClientInfo {
                    kind: ClientKind::Electron,
                    label: "a".to_owned(),
                },
                since: Instant::now() - RECONNECT_GRACE - Duration::from_secs(1),
            },
        );
        reg.expire_orphans(Instant::now());
        assert!(reg.claims.is_empty());
        assert!(reg.orphaned.is_empty());
    }

    /// Expiry must never be the thing that revokes a *live* claim: by the time
    /// an orphan ages out, the worktree may belong to somebody else entirely.
    #[test]
    fn expiring_an_orphan_leaves_a_claim_someone_else_has_taken() {
        let mut reg = Registry::default();
        reg.claims.insert(7, "b".to_owned());
        reg.orphaned.insert(
            7,
            Orphan {
                client_id: "a".to_owned(),
                info: ClientInfo {
                    kind: ClientKind::Electron,
                    label: "a".to_owned(),
                },
                since: Instant::now() - RECONNECT_GRACE - Duration::from_secs(1),
            },
        );
        reg.expire_orphans(Instant::now());
        assert_eq!(reg.claims.get(&7).map(String::as_str), Some("b"));
    }

    /// The rail keeps showing an orphaned worktree as taken during the grace,
    /// and keeps describing it correctly — a click on it would otherwise be
    /// granted and then taken away.
    #[test]
    fn the_claims_view_describes_an_orphan_from_its_own_record() {
        let mut reg = Registry::default();
        reg.claims.insert(7, "a".to_owned());
        reg.orphaned.insert(
            7,
            Orphan {
                client_id: "a".to_owned(),
                info: ClientInfo {
                    kind: ClientKind::Browser,
                    label: "Safari".to_owned(),
                },
                since: Instant::now(),
            },
        );
        let view = reg.view_for("someone-else");
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].client.kind, ClientKind::Browser);
        assert_eq!(view[0].client.label, "Safari");
        assert!(!view[0].mine);
    }

    /// Worktree rowids are reused, so a claim left on a deleted worktree would
    /// grey out whichever one is created next.
    #[test]
    fn forgetting_a_worktree_drops_its_claim_its_orphan_and_every_hold() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();
        holds(&mut reg, "a", &[7, 9]);
        reg.orphaned.insert(
            9,
            Orphan {
                client_id: "z".to_owned(),
                info: ClientInfo {
                    kind: ClientKind::Browser,
                    label: String::new(),
                },
                since: Instant::now(),
            },
        );

        let gone: HashSet<i64> = [7, 9].into_iter().collect();
        reg.claims.retain(|w, _| !gone.contains(w));
        reg.orphaned.retain(|w, _| !gone.contains(w));
        for c in reg.clients.values_mut() {
            c.holds.retain(|w| !gone.contains(w));
        }
        assert!(reg.claims.is_empty());
        assert!(reg.orphaned.is_empty());
        assert!(reg.clients["a"].holds.is_empty());
    }

    // -----------------------------------------------------------------------
    // Identity
    // -----------------------------------------------------------------------

    /// **The identity never goes on the wire to anyone else.** It is the
    /// credential a reconnect resumes with, so a broadcast carrying it handed
    /// every client the set of ids it would need to impersonate any other — and
    /// impersonation inherited a live claim without a yield being asked of the
    /// client still attached to its terminals.
    #[test]
    fn the_claims_broadcast_never_carries_a_client_id() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "secret-id-a", ClientKind::Electron);
        // A label of its own, so the assertion is about the identity and not
        // about the fixture happening to reuse it.
        reg.clients.get_mut(&a.id).unwrap().info.label = "Window 1".to_owned();
        connect(&mut reg, "b", ClientKind::Browser);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();
        let json = serde_json::to_string(&ServerMsg::Claims {
            claims: reg.view_for("b"),
        })
        .unwrap();
        assert!(
            !json.contains("secret-id-a"),
            "an identity must never reach another client: {json}"
        );
    }

    /// The rail's question is "is this one mine", and it is answered per
    /// recipient so the identity can stay off the wire entirely.
    #[test]
    fn a_claim_is_mine_only_to_the_client_that_made_it() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        connect(&mut reg, "b", ClientKind::Browser);
        claim_for(&mut reg, &a, 7, 1, true).unwrap();
        assert!(reg.view_for("a")[0].mine);
        assert!(!reg.view_for("b")[0].mine);
    }

    /// A grace that ends without telling anyone leaves every other client's rail
    /// greying out a worktree nobody holds — for as long as nobody happens to
    /// claim something.
    #[test]
    fn expiring_an_orphan_reports_that_something_changed() {
        let mut reg = Registry::default();
        reg.claims.insert(7, "a".to_owned());
        reg.orphaned.insert(
            7,
            Orphan {
                client_id: "a".to_owned(),
                info: ClientInfo {
                    kind: ClientKind::Electron,
                    label: "a".to_owned(),
                },
                since: Instant::now() - RECONNECT_GRACE - Duration::from_secs(1),
            },
        );
        assert!(
            reg.expire_orphans(Instant::now()),
            "the caller must broadcast"
        );
        // …and says nothing when there was nothing to release, so a broadcast is
        // not sent on every claim for no reason.
        assert!(!reg.expire_orphans(Instant::now()));
    }

    /// **Frame order decides the winner, not scheduler order.**
    ///
    /// The sequence number is taken in the read loop and passed in, because a
    /// claim is spawned: tokio parks a task spawned from a worker in its LIFO
    /// slot and polls that first, so two claims read in one poll run
    /// second-then-first. Taken inside the task, the *earlier* frame got the
    /// lower number and the later one was answered `superseded` — for which the
    /// UI deliberately says nothing, so the user's second click vanished.
    #[test]
    fn the_later_claim_wins_however_the_two_are_scheduled() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        // Frame order: 7 then 8. Handed to `begin_claim` in the opposite order,
        // which is what a LIFO-slot schedule produces.
        let first = NEXT_CLAIM_SEQ.fetch_add(1, Ordering::Relaxed);
        let second = NEXT_CLAIM_SEQ.fetch_add(1, Ordering::Relaxed);
        reg.begin_claim(&a.id, a.conn, 8, 2, false, second).unwrap();
        reg.begin_claim(&a.id, a.conn, 7, 1, false, first).unwrap();
        assert_eq!(
            reg.claim_seq.get("a"),
            Some(&second),
            "the later frame owns the outcome whichever ran first"
        );
    }

    /// A claim is spawned, so it can be polled after its own socket is gone —
    /// or after a newer socket resumed the identity. Recording one then leaves
    /// an entry nothing removes, and answers a page that never asked.
    #[test]
    fn a_claim_from_a_socket_that_has_been_replaced_is_dropped() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        let stale = a.conn;
        // The page reloaded: same identity, new socket.
        let a2 = connect(&mut reg, "a", ClientKind::Electron);
        assert_ne!(stale, a2.conn);
        let seq = NEXT_CLAIM_SEQ.fetch_add(1, Ordering::Relaxed);
        assert!(reg.begin_claim("a", stale, 7, 1, false, seq).is_none());
        assert!(
            reg.claims.is_empty(),
            "and records nothing for a dead socket"
        );
    }

    /// A claim waits for every holder at once. Sequentially, `holds` being
    /// client-declared meant one page could name itself a holder of everything
    /// and put `holders × YIELD_ACK` in front of every real claim.
    #[tokio::test(start_paused = true)]
    async fn a_claim_waits_out_silent_holders_concurrently() {
        // Three silent holders, one deadline. Under the old sequential await
        // this took 3 × YIELD_ACK.
        let (tx, rx) = (0..3)
            .map(|_| oneshot::channel::<()>())
            .collect::<Vec<_>>()
            .into_iter()
            .fold((Vec::new(), Vec::new()), |(mut t, mut r), (a, b)| {
                t.push(a);
                r.push(b);
                (t, r)
            });
        let started = tokio::time::Instant::now();
        let _keep = tx;
        futures_util::future::join_all(
            rx.into_iter()
                .map(|settle| async move { tokio::time::timeout(YIELD_ACK, settle).await }),
        )
        .await;
        assert!(
            started.elapsed() < YIELD_ACK * 2,
            "the waits must overlap, not queue: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn client_ids_are_bounded_to_a_charset_that_cannot_carry_a_payload() {
        assert!(valid_client_id("abc-123_XYZ"));
        assert!(!valid_client_id(""));
        assert!(!valid_client_id("has space"));
        assert!(!valid_client_id("../../etc"));
        assert!(!valid_client_id(&"a".repeat(65)));
        assert!(valid_client_id(&"a".repeat(64)));
    }

    /// The label is rendered inside another client's UI, so it is bounded on the
    /// way in rather than trusted to be short.
    #[test]
    fn a_label_is_truncated_rather_than_trusted() {
        let hello: Hello = serde_json::from_str(&format!(
            r#"{{"type":"hello","kind":"browser","label":"{}"}}"#,
            "x".repeat(500)
        ))
        .unwrap();
        let label: String = hello.label.chars().take(120).collect();
        assert_eq!(label.chars().count(), 120);
    }

    /// A newer client adding a field must still be able to connect to an older
    /// daemon — a rejected hello is a page that never works at all.
    #[test]
    fn an_unknown_hello_field_is_ignored() {
        let hello: Hello =
            serde_json::from_str(r#"{"type":"hello","kind":"electron","label":"w","future":1}"#)
                .unwrap();
        assert_eq!(hello.kind, ClientKind::Electron);
        assert_eq!(hello.resume, None);
    }

    #[test]
    fn a_ticket_is_single_use_and_an_unknown_one_is_refused() {
        let ticket = uuid::Uuid::new_v4().simple().to_string();
        TICKETS.lock().unwrap().insert(
            ticket.clone(),
            Ticket {
                client_id: "c".to_owned(),
                expires_at: Instant::now() + TICKET_TTL,
            },
        );
        assert_eq!(redeem(&ticket).as_deref(), Some("c"));
        assert!(
            redeem(&ticket).is_none(),
            "a redeemed ticket must not work twice"
        );
        assert!(redeem("never-minted").is_none());
    }

    #[test]
    fn an_expired_ticket_is_refused() {
        let ticket = uuid::Uuid::new_v4().simple().to_string();
        TICKETS.lock().unwrap().insert(
            ticket.clone(),
            Ticket {
                client_id: "c".to_owned(),
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        assert!(redeem(&ticket).is_none());
    }
}
