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
    // seeds a default instead of restoring an empty screen. Unversioned on
    // purpose: the only caller is a client that has just closed the last pane,
    // and it holds the worktree to be able to do that.
    let Some(layout) = body.layout else {
        db.delete_pane_layout(worktree_id).map_err(|e| {
            warn!("layout delete: database error: {e}");
            err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
        })?;
        broadcast_layout(worktree_id, 0, body.client_id.as_deref());
        return Ok(Json(LayoutResponse {
            version: 0,
            layout: None,
        }));
    };

    let text = serde_json::to_string(&layout)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "layout is not serializable"))?;
    let outcome = db
        .put_pane_layout(worktree_id, body.version, &text)
        .map_err(|e| {
            // The foreign key refuses a layout for a worktree that does not
            // exist, which is a client racing a deletion rather than a fault.
            debug!("layout write: {e}");
            err(StatusCode::NOT_FOUND, "worktree not found")
        })?;

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
        Ok(LayoutWrite::Conflict(cur)) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "layout version is stale",
                "version": cur.version,
                "layout": serde_json::from_str::<serde_json::Value>(&cur.layout).ok(),
            })),
        )),
    }
}

// ---------------------------------------------------------------------------
// Tickets
// ---------------------------------------------------------------------------

struct Ticket {
    expires_at: Instant,
}

static TICKETS: LazyLock<Mutex<HashMap<String, Ticket>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Serialize)]
struct TicketResponse {
    ticket: String,
    expires_in_ms: u64,
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
    let now = Instant::now();
    {
        let mut store = TICKETS.lock().expect("ide ticket store poisoned");
        // A ticket minted and never redeemed (the tab closed mid-connect) would
        // otherwise sit here for the life of the daemon.
        store.retain(|_, t| t.expires_at > now);
        store.insert(
            ticket.clone(),
            Ticket {
                expires_at: now + TICKET_TTL,
            },
        );
    }
    Ok(Json(TicketResponse {
        ticket,
        expires_in_ms: TICKET_TTL.as_millis() as u64,
    }))
}

/// Consume a ticket. `false` if it is unknown or expired.
fn redeem(ticket: &str) -> bool {
    let mut store = TICKETS.lock().expect("ide ticket store poisoned");
    match store.remove(ticket) {
        Some(t) => t.expires_at > Instant::now(),
        None => false,
    }
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
    /// Stable across reloads of one tab or window, so a reload gets its claims
    /// back within [`RECONNECT_GRACE`]. Chosen by the client; it names nothing
    /// but itself, and a client that forges another's id can only take over
    /// claims it could have made anyway by asking.
    client_id: String,
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
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub client_id: String,
    pub kind: ClientKind,
    pub label: String,
}

/// Daemon → client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    /// The handshake landed. `epoch` changes when the daemon restarts, which is
    /// how a reconnecting client knows to re-read everything rather than assume
    /// its picture survived.
    Ready { epoch: String },
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

/// What a granted claim leaves for [`claim`]'s async half: the sequence number
/// that decides whether it is still current, and the yields to wait out.
type GrantedClaim = (u64, Vec<PendingYield>);

/// One entry of the claims table, as clients see it.
#[derive(Debug, Clone, Serialize)]
struct ClaimView {
    worktree_id: i64,
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
    /// Drop orphaned claims whose owner is not coming back.
    ///
    /// Called on the paths that read `claims`, rather than from a timer: the
    /// only thing an expired orphan affects is the answer to a claim or the
    /// contents of a broadcast, so evaluating it there is both sufficient and
    /// free. A timer would be a second thing to keep alive for no gain.
    ///
    /// The claim is removed only if it is *still* the orphan's. A worktree
    /// claimed by somebody else during the grace has already had its orphan
    /// entry dropped, but checking the owner here as well means this can never
    /// be the thing that revokes a live claim.
    fn expire_orphans(&mut self, now: Instant) {
        let expired: Vec<(i64, String)> = self
            .orphaned
            .iter()
            .filter(|(_, o)| now.duration_since(o.since) >= RECONNECT_GRACE)
            .map(|(worktree_id, o)| (*worktree_id, o.info.client_id.clone()))
            .collect();
        for (worktree_id, owner) in expired {
            self.orphaned.remove(&worktree_id);
            if self.claims.get(&worktree_id) == Some(&owner) {
                self.claims.remove(&worktree_id);
            }
        }
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
            .filter(|(_, o)| o.info.client_id == client_id)
            .map(|(worktree_id, _)| *worktree_id)
            .collect();
        for worktree_id in mine {
            self.orphaned.remove(&worktree_id);
            self.claims.insert(worktree_id, client_id.to_owned());
        }
    }

    /// Who is showing what, as clients see it.
    ///
    /// Includes a client's own claim: the rail needs "somewhere else" and can
    /// subtract itself, and sending each client a personalised list would mean
    /// recomputing the whole table per recipient to save one comparison.
    /// An orphaned claim is included too — the worktree really is spoken for
    /// until the grace expires, and showing it as free would invite a click
    /// that then gets taken away.
    fn view(&self) -> Vec<ClaimView> {
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
                    client: info,
                })
            })
            .collect()
    }

    /// Push the claims table to every connected client.
    fn broadcast(&self) {
        let msg = ServerMsg::Claims {
            claims: self.view(),
        };
        for client in self.clients.values() {
            let _ = client.tx.send(msg.clone());
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
        worktree_id: i64,
        request_id: u64,
        focus_holder: bool,
    ) -> Option<GrantedClaim> {
        self.expire_orphans(Instant::now());

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

        let seq = NEXT_CLAIM_SEQ.fetch_add(1, Ordering::Relaxed);
        self.claim_seq.insert(client_id.to_owned(), seq);

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
        Some((seq, waits))
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
    if !redeem(&q.ticket) {
        return (StatusCode::FORBIDDEN, "invalid or expired ticket").into_response();
    }
    ws.max_message_size(MAX_FRAME_BYTES)
        .on_upgrade(serve_channel)
}

/// Bound on the identity strings a client may send.
fn valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn serve_channel(socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // The first frame must be the hello. Everything after it is scoped to the
    // identity it establishes, so there is nothing useful a client can do before
    // sending one — and accepting other messages first would mean every handler
    // below has to cope with an unidentified sender.
    let hello: Hello = loop {
        match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<Hello>(&text) {
                Ok(h) if valid_client_id(&h.client_id) => break h,
                Ok(_) => {
                    warn!("ide channel: rejected an invalid client id");
                    return;
                }
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

    let client_id = hello.client_id.clone();
    let info = ClientInfo {
        client_id: client_id.clone(),
        kind: hello.kind,
        // Bounded: it is rendered in another client's UI, and an unbounded
        // string crossing that boundary is a payload, not a name.
        label: hello.label.chars().take(120).collect(),
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);

    {
        let mut reg = REGISTRY.lock().await;
        reg.expire_orphans(Instant::now());
        // **A reconnecting client takes its claims back** — see
        // `reclaim_orphans_of`.
        reg.reclaim_orphans_of(&client_id);
        // A second socket for one `client_id` (a reload whose old socket has not
        // been noticed yet, or two tabs with a copied id) displaces the first
        // rather than running beside it: two sockets under one identity would
        // both be sent that identity's yields, and each would think the other
        // had answered. Dropping the old record drops its queue, which ends its
        // writer task; its reader task then finds `conn` changed and leaves the
        // registry alone.
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
            Ok(msg) => handle(&client_id, msg).await,
            Err(e) => debug!(client = %client_id, "ide channel: unreadable message: {e}"),
        }
    }

    disconnect(&client_id, conn).await;
    writer.abort();
}

async fn handle(client_id: &str, msg: ClientMsg) {
    match msg {
        ClientMsg::Claim {
            worktree_id,
            request_id,
            focus_holder,
        } => claim(client_id, worktree_id, request_id, focus_holder).await,
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
        ClientMsg::Forget { worktree_ids } => {
            let mut reg = REGISTRY.lock().await;
            let gone: HashSet<i64> = worktree_ids.into_iter().take(MAX_HELD).collect();
            if gone.is_empty() {
                return;
            }
            reg.claims
                .retain(|worktree_id, _| !gone.contains(worktree_id));
            reg.orphaned
                .retain(|worktree_id, _| !gone.contains(worktree_id));
            for client in reg.clients.values_mut() {
                client.holds.retain(|w| !gone.contains(w));
            }
            reg.broadcast();
        }
    }
}

/// Ask to show a worktree.
///
/// Records the claim synchronously — that is what makes a third client's claim
/// arriving during the wait get refused rather than granted alongside this one,
/// and the greyed rail row in every other client true from that moment. Then
/// waits for the previous holders to let go, because the caller attaches to the
/// PTY sessions its layout names on the strength of this answer.
async fn claim(client_id: &str, worktree_id: i64, request_id: u64, focus_holder: bool) {
    let Some((answer, waits)) = ({
        let mut reg = REGISTRY.lock().await;
        reg.begin_claim(client_id, worktree_id, request_id, focus_holder)
    }) else {
        // Refused; `begin_claim` has already told both sides.
        return;
    };

    for (target, yield_id, settle) in waits {
        if tokio::time::timeout(YIELD_ACK, settle).await.is_err() {
            // Proceeding anyway is the documented fallback, and it is also the
            // one path that can still reinstate the takeover race — so it says
            // so. The condition (a wedged or very slow client) is exactly the
            // kind only ever reported second-hand.
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
    }

    let reg = REGISTRY.lock().await;
    // **A claim from the same client during the wait supersedes this one.** The
    // later claim released this reservation on its way in, so answering `ok`
    // here would tell the page to display a worktree the daemon no longer
    // records it as showing — and another client asking for that one would then
    // be granted it. Answers do not even come back in call order: a claim with a
    // silent holder waits out `YIELD_ACK` while one with no holder returns at
    // once.
    let superseded = reg.claim_seq.get(client_id) != Some(&answer);
    if let Some(me) = reg.clients.get(client_id) {
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
                    info: info.clone(),
                    since: now,
                },
            );
        }
    }
    reg.expire_orphans(now);
    reg.broadcast();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client in the registry, plus the queue its socket would be draining.
    struct Fake {
        id: String,
        rx: mpsc::UnboundedReceiver<ServerMsg>,
    }

    fn connect(reg: &mut Registry, id: &str, kind: ClientKind) -> Fake {
        let (tx, rx) = mpsc::unbounded_channel();
        reg.clients.insert(
            id.to_owned(),
            Client {
                conn: NEXT_CONN.fetch_add(1, Ordering::Relaxed),
                info: ClientInfo {
                    client_id: id.to_owned(),
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
            rx,
        }
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
        let granted = reg.begin_claim(&a.id, 7, 1, true);
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
        reg.begin_claim(&a.id, 7, 1, true).unwrap();

        assert!(reg.begin_claim(&b.id, 7, 2, true).is_none());
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
        reg.begin_claim(&a.id, 7, 1, true).unwrap();
        reg.begin_claim(&b.id, 7, 2, true);
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
        reg.begin_claim(&a.id, 7, 1, true).unwrap();
        let _ = drain(&mut a);

        reg.begin_claim(&b.id, 7, 2, false);
        assert!(
            !drain(&mut a).iter().any(|m| matches!(m, ServerMsg::Focus)),
            "a client surveying what it may display must not yank windows forward"
        );

        reg.begin_claim(&b.id, 7, 3, true);
        assert!(drain(&mut a).iter().any(|m| matches!(m, ServerMsg::Focus)));
    }

    /// Re-selecting the worktree you are already showing is not a fight with
    /// yourself.
    #[test]
    fn reclaiming_your_own_worktree_is_granted() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        reg.begin_claim(&a.id, 7, 1, true).unwrap();
        assert!(reg.begin_claim(&a.id, 7, 2, true).is_some());
    }

    /// One *displayed* worktree per client: switching away frees the old one for
    /// everybody, which is what makes "close that window and it comes back over
    /// here" work with no hand-off protocol.
    #[test]
    fn switching_worktrees_releases_the_previous_claim() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        let b = connect(&mut reg, "b", ClientKind::Electron);
        reg.begin_claim(&a.id, 7, 1, true).unwrap();
        reg.begin_claim(&a.id, 8, 2, true).unwrap();
        assert!(!reg.claims.contains_key(&7));
        assert!(reg.begin_claim(&b.id, 7, 3, true).is_some());
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

        let (_, waits) = reg.begin_claim(&a.id, 7, 1, true).unwrap();
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
        let (_, waits) = reg.begin_claim(&a.id, 7, 1, true).unwrap();
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
        let (_, waits) = reg.begin_claim(&a.id, 7, 1, true).unwrap();
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
        reg.begin_claim(&a.id, 7, 1, true).unwrap();

        reg.clients.remove("a");
        reg.orphaned.insert(
            7,
            Orphan {
                info: ClientInfo {
                    client_id: "a".to_owned(),
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
                info: ClientInfo {
                    client_id: "a".to_owned(),
                    kind: ClientKind::Electron,
                    label: "a".to_owned(),
                },
                since: Instant::now(),
            },
        );

        assert!(reg.begin_claim(&b.id, 7, 1, true).is_some());
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
                info: ClientInfo {
                    client_id: "a".to_owned(),
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
                info: ClientInfo {
                    client_id: "a".to_owned(),
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
                info: ClientInfo {
                    client_id: "a".to_owned(),
                    kind: ClientKind::Browser,
                    label: "Safari".to_owned(),
                },
                since: Instant::now(),
            },
        );
        let view = reg.view();
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].client.kind, ClientKind::Browser);
        assert_eq!(view[0].client.label, "Safari");
    }

    /// Worktree rowids are reused, so a claim left on a deleted worktree would
    /// grey out whichever one is created next.
    #[test]
    fn forgetting_a_worktree_drops_its_claim_its_orphan_and_every_hold() {
        let mut reg = Registry::default();
        let a = connect(&mut reg, "a", ClientKind::Electron);
        reg.begin_claim(&a.id, 7, 1, true).unwrap();
        holds(&mut reg, "a", &[7, 9]);
        reg.orphaned.insert(
            9,
            Orphan {
                info: ClientInfo {
                    client_id: "z".to_owned(),
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
            r#"{{"type":"hello","client_id":"a","kind":"browser","label":"{}"}}"#,
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
        let hello: Hello = serde_json::from_str(
            r#"{"type":"hello","client_id":"a","kind":"electron","label":"w","future":1}"#,
        )
        .unwrap();
        assert_eq!(hello.kind, ClientKind::Electron);
    }

    #[test]
    fn a_ticket_is_single_use_and_an_unknown_one_is_refused() {
        let ticket = uuid::Uuid::new_v4().simple().to_string();
        TICKETS.lock().unwrap().insert(
            ticket.clone(),
            Ticket {
                expires_at: Instant::now() + TICKET_TTL,
            },
        );
        assert!(redeem(&ticket));
        assert!(!redeem(&ticket), "a redeemed ticket must not work twice");
        assert!(!redeem("never-minted"));
    }

    #[test]
    fn an_expired_ticket_is_refused() {
        let ticket = uuid::Uuid::new_v4().simple().to_string();
        TICKETS.lock().unwrap().insert(
            ticket.clone(),
            Ticket {
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        assert!(!redeem(&ticket));
    }
}
