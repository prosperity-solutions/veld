//! In-memory manager for active shares and joins, plus the iroh endpoints the
//! daemon uses for P2P traffic — one endpoint per relay policy, bound on demand,
//! so shares on different relays run concurrently.
//!
//! State is intentionally ephemeral: if the daemon stops, shares and joins stop
//! with it (fail-closed; a consumer then gets a clean connection error). The
//! persistent node keypair backs the public endpoint (stable identity);
//! custom-relay endpoints get a fresh per-run identity.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, OnceCell, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use veld_core::helper::HelperClient;
use veld_core::share::{
    ApprovalMode, Capability, GatewayPublicUrl, JoinResponse, PendingInfo, ShareInfo,
    ShareManifest, ShareTicket, SharesList,
};
use veld_core::state::GlobalRegistry;

use super::gateway::GatewayClient;

/// Timeout a manual approval waits before auto-denying.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the reaper scans for expired shares.
const REAPER_INTERVAL: Duration = Duration::from_secs(60);
/// Consecutive heartbeat acks that must fail to confirm a web share's access
/// policy before the daemon fails closed and unshares. >1 so a single
/// anomalous ack (or a lone stale replica during a rolling gateway upgrade)
/// can't strand a healthy password share, while a genuine rollback still
/// closes within a few beats.
const ACK_FAILURE_LIMIT: u32 = 3;
/// Max time to wait for an inbound peer to send its control frame before the
/// per-connection task gives up (pre-auth, so a leaked link can't pile up
/// stalled tasks holding a connection).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Max time a consumer's `join` waits to dial the host (hole-punch + approval)
/// before returning a clean error instead of hanging the HTTP request.
const DIAL_TIMEOUT: Duration = Duration::from_secs(75);
/// How long the join side watches the endpoint's home-relay status to tell a
/// token-gated relay's auth denial apart from a slow/unreachable relay. Kept
/// short: a denial surfaces fast, and on timeout we proceed and let the dial
/// (with its own `DIAL_TIMEOUT`) report a real connectivity failure.
const RELAY_AUTH_TIMEOUT: Duration = Duration::from_secs(8);

use super::endpoint::{
    RelayAuth, RelayChoice, bind_endpoint, relay_auth_status, resolve_embedded_tokens,
};
use super::host::{self, HostShare};
use super::{join, token_store};

/// The supplied tokens worth caching after a successful join: only those keyed
/// by a relay the ticket actually advertises. Filters out any spurious key a
/// client tacked onto `supplied_tokens` so a token is never cached for a relay
/// this join never used. Keys are canonical `RelayUrl` strings on both sides.
fn tokens_to_cache<'a>(
    ticket_relay_urls: &[String],
    supplied: &'a BTreeMap<String, String>,
) -> Vec<(&'a str, &'a str)> {
    supplied
        .iter()
        .filter(|(url, _)| ticket_relay_urls.iter().any(|t| t == *url))
        .map(|(u, t)| (u.as_str(), t.as_str()))
        .collect()
}

/// A share this daemon is hosting.
struct ShareEntry {
    id: String,
    manifest: ShareManifest,
    host_share: Arc<HostShare>,
    approve_mode: ApprovalMode,
    /// The encoded `veldshare_…` token, so the dashboard can build a join link.
    ticket: String,
    /// Unix seconds after which the reaper removes this share.
    expires_at: i64,
    /// The relay policy (endpoint) this share is served on. An inbound
    /// connection is matched to this share only if it arrived on the *same*
    /// endpoint — so a custom-relay share is never served over the public
    /// endpoint, keeping relay confinement airtight.
    relay: RelayChoice,
    /// Set for web shares: the gateway registration this share lives behind.
    web: Option<WebRegistration>,
}

/// A web share's registration on the public gateway: the daemon heartbeats it
/// for the share's lifetime and unregisters on unshare (best-effort — the
/// gateway's lease expiry covers a lost DELETE).
struct WebRegistration {
    client: GatewayClient,
    reg_id: String,
    public_urls: Vec<GatewayPublicUrl>,
    /// Viewer access policy sent with the registration; heartbeats re-send it
    /// so a restarted gateway re-learns the password with the lease (§6.1).
    /// Also the source for re-displaying the password in `veld shares`.
    access: Option<veld_core::share::GatewayAccessPolicy>,
    /// The heartbeat loop; aborted on unshare. It also self-terminates when
    /// the share disappears from the map (belt and braces).
    heartbeat: JoinHandle<()>,
}

/// A join parked awaiting the host's manual approval.
struct PendingRequest {
    id: String,
    share_id: String,
    label: String,
    node_id: String,
    /// Resolved by `approve_request`/`deny_request`.
    decision: oneshot::Sender<bool>,
}

/// A share this daemon has joined; holds everything needed to tear it down.
struct JoinEntry {
    id: String,
    nodes: Vec<String>,
    urls: Vec<String>,
    /// (hostname, route_id) pairs registered with the helper.
    routes: Vec<(String, String)>,
    /// Local listener tasks; aborted on leave to drop the listeners.
    tasks: Vec<JoinHandle<()>>,
    /// The QUIC connection to the host; closed on leave to stop the tunnel.
    conn: Connection,
    /// Capability of the joined share, so repeat opens of the same link are
    /// idempotent instead of creating duplicate joins.
    capability: Capability,
    /// The relay policy this join's endpoint is bound on. Recorded so
    /// `evict_endpoint` never tears down an endpoint a live join still uses —
    /// e.g. after mid-session token rotation, a *new* denied join can share this
    /// join's `RelayChoice` while this join survives on a direct path.
    relay: RelayChoice,
    /// Non-fatal notes from the join (e.g. skipped nodes), preserved so a repeat
    /// open of the same link reports them instead of an empty list.
    warnings: Vec<String>,
}

/// Owns the iroh endpoints and all live shares/joins.
pub struct ShareManager {
    secret_key: SecretKey,
    /// One iroh endpoint per relay policy, bound on demand. The daemon can host
    /// concurrent shares on different relays (e.g. public + a self-hosted relay)
    /// by routing each share/join to the endpoint matching its policy. The
    /// public endpoint reuses the daemon's persistent identity; custom-relay
    /// endpoints get a fresh per-run identity (shares are ephemeral, so their
    /// node id need not survive a restart).
    endpoints: Mutex<HashMap<RelayChoice, Endpoint>>,
    /// The reaper scans all shares regardless of endpoint, so it runs once —
    /// started on the first endpoint bind.
    reaper: OnceCell<()>,
    shares: Mutex<HashMap<String, ShareEntry>>,
    joins: Mutex<HashMap<String, JoinEntry>>,
    /// Join requests awaiting manual approval, keyed by request id.
    pending: Mutex<HashMap<String, PendingRequest>>,
    /// For `first` mode: the node id that claimed each share, keyed by share id.
    claims: Mutex<HashMap<String, EndpointId>>,
    /// Live inbound connections per hosted share (keyed by connection stable id),
    /// so `unshare` can close them and the dashboard can count joiners.
    conns: Mutex<HashMap<String, HashMap<usize, Connection>>>,
}

impl ShareManager {
    pub fn new(secret_key: SecretKey) -> Self {
        Self {
            secret_key,
            endpoints: Mutex::new(HashMap::new()),
            reaper: OnceCell::new(),
            shares: Mutex::new(HashMap::new()),
            joins: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            claims: Mutex::new(HashMap::new()),
            conns: Mutex::new(HashMap::new()),
        }
    }

    /// The secret key (node identity) for an endpoint on `choice`. The public
    /// endpoint reuses the daemon's persistent key; each custom-relay endpoint
    /// gets its own fresh key so no two live endpoints share a node id (iroh
    /// requires one identity per endpoint).
    fn key_for(&self, choice: &RelayChoice) -> SecretKey {
        match choice {
            RelayChoice::Public => self.secret_key.clone(),
            RelayChoice::Custom(_) => SecretKey::generate(),
        }
    }

    /// Get (or bind on demand) the endpoint for `requested`, starting its accept
    /// loop, and the global reaper on the first bind of any endpoint.
    async fn get_or_bind(self: &Arc<Self>, requested: &RelayChoice) -> Result<Endpoint> {
        // Hold the map lock across bind so two callers racing on the same policy
        // can't bind two endpoints for it (binds are infrequent, and each policy
        // binds at most once for the daemon's life). `bind_endpoint` resolves any
        // relay auth tokens here — including a `command`/`file` source — so this
        // is held across that work; I/O-bound resolution is time-bounded
        // (`TOKEN_RESOLVE_TIMEOUT`) so a hung secret source can't wedge the lock
        // indefinitely. Trade-off (accepted, given binds are rare): a slow token
        // source stalls binds for *other* policies too for up to that bound;
        // resolving before the lock (double-checked insert) would remove that but
        // isn't worth the added complexity here.
        let mut endpoints = self.endpoints.lock().await;
        if let Some(ep) = endpoints.get(requested) {
            return Ok(ep.clone());
        }
        let ep = bind_endpoint(self.key_for(requested), requested).await?;
        info!(node_id = %ep.id(), relays = %requested, "iroh share endpoint bound");
        self.clone()
            .spawn_accept_loop(ep.clone(), requested.clone());
        endpoints.insert(requested.clone(), ep.clone());
        drop(endpoints);

        if self.reaper.set(()).is_ok() {
            self.clone().spawn_reaper();
        }
        Ok(ep)
    }

    /// Remove and close an endpoint that was bound only to probe relay auth (a
    /// denied join). Closing ends its accept loop (`accept()` returns `None`)
    /// and drops the permanently-denied relay connection.
    ///
    /// Skipped if any live share OR join is on the same relay policy: an
    /// endpoint is shared by `RelayChoice`, and `close()` tears down *all* its
    /// connections. A denied probe can share a live join's choice after the
    /// relay rotates its token (the live join survives on a direct path, a new
    /// join arrives with the now-stale token → same key → same endpoint), so
    /// closing here would kill the healthy join. When shared, leave it — it
    /// isn't leaked (the other party uses it); only an *unshared* probe endpoint
    /// is the leak this evicts.
    async fn evict_endpoint(&self, choice: &RelayChoice) {
        if self
            .shares
            .lock()
            .await
            .values()
            .any(|s| &s.relay == choice)
        {
            return;
        }
        if self.joins.lock().await.values().any(|j| &j.relay == choice) {
            return;
        }
        let ep = self.endpoints.lock().await.remove(choice);
        if let Some(ep) = ep {
            ep.close().await;
        }
    }

    /// Accept inbound connections and dispatch each to the share whose
    /// capability the peer presents.
    fn spawn_accept_loop(self: Arc<Self>, endpoint: Endpoint, relay: RelayChoice) {
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let mgr = Arc::clone(&self);
                let relay = relay.clone();
                tokio::spawn(async move {
                    // Bounded handshake: a peer that completes the QUIC connect
                    // but never sends its control frame (e.g. a leaked link)
                    // must not park this task forever holding a connection.
                    let handshake = async {
                        let conn = incoming.await.ok()?;
                        let (req, send, recv) = host::read_control(&conn).await.ok()?;
                        Some((conn, req, send, recv))
                    };
                    let Ok(Some((conn, req, send, recv))) =
                        tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await
                    else {
                        debug!("inbound handshake failed or timed out");
                        return;
                    };
                    drop(recv);

                    // Match the capability only against shares served on THIS
                    // endpoint's relay policy. A share minted on a custom relay
                    // must never be served over the public endpoint (and vice
                    // versa), or relay confinement would leak.
                    let matched = {
                        let shares = mgr.shares.lock().await;
                        shares
                            .values()
                            .find(|s| {
                                s.relay == relay && s.host_share.capability.ct_eq(&req.capability)
                            })
                            .map(|s| (s.id.clone(), s.approve_mode, Arc::clone(&s.host_share)))
                    };

                    let Some((share_id, mode, host_share)) = matched else {
                        host::deny(send, "unknown or expired share").await;
                        return;
                    };

                    let node_id = conn.remote_id();
                    let approved = mgr
                        .resolve_approval(&share_id, mode, node_id, &req.label)
                        .await;

                    if !approved {
                        host::deny(send, "join denied").await;
                        return;
                    }

                    // Register the connection FIRST, then re-check the share
                    // still exists. This closes the race with a concurrent
                    // unshare (which was parked awaiting manual approval): unshare
                    // either sees this conn in the map and force-closes it, or we
                    // observe the share gone here and tear down. Registering after
                    // the re-check would let a conn slip past unshare's close.
                    let sid = conn.stable_id();
                    mgr.register_conn(&share_id, conn.clone()).await;
                    if !mgr.shares.lock().await.contains_key(&share_id) {
                        mgr.unregister_conn(&share_id, sid).await;
                        mgr.clear_claim(&share_id, node_id).await;
                        host::deny(send, "share stopped").await;
                        return;
                    }

                    debug!(label = %req.label, share = %share_id, "join approved");
                    let _ = host::accept_and_serve(conn, send, host_share).await;
                    mgr.unregister_conn(&share_id, sid).await;
                    // First-mode: release the claim when the pinned peer's session
                    // ends so a different colleague can join next.
                    mgr.clear_claim(&share_id, node_id).await;
                });
            }
        });
    }

    /// Periodically remove shares past their TTL, closing them fail-closed.
    fn spawn_reaper(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAPER_INTERVAL).await;
                let now = chrono::Utc::now().timestamp();
                let expired: Vec<String> = {
                    let shares = self.shares.lock().await;
                    shares
                        .values()
                        .filter(|s| s.expires_at <= now)
                        .map(|s| s.id.clone())
                        .collect()
                };
                for id in expired {
                    let _ = self.unshare(&id).await;
                    info!(share_id = %id, "share expired");
                }
            }
        });
    }

    /// Track a live inbound connection so `unshare` can force it closed and the
    /// dashboard can count joiners.
    async fn register_conn(&self, share_id: &str, conn: Connection) {
        let sid = conn.stable_id();
        self.conns
            .lock()
            .await
            .entry(share_id.to_string())
            .or_default()
            .insert(sid, conn);
    }

    /// Drop a connection from the live set when its session ends.
    async fn unregister_conn(&self, share_id: &str, stable_id: usize) {
        if let Some(m) = self.conns.lock().await.get_mut(share_id) {
            m.remove(&stable_id);
        }
    }

    /// Register a share for `manifest`, minting a ticket. The capability gates
    /// all inbound connections to this share.
    pub async fn start_share(
        self: &Arc<Self>,
        manifest: ShareManifest,
        capability: Capability,
        approve_mode: ApprovalMode,
        relay: RelayChoice,
        embed_relay_tokens: bool,
    ) -> Result<(String, ShareTicket)> {
        let choice = relay;
        let endpoint = self.get_or_bind(&choice).await?;

        // Wait (bounded) for the endpoint to learn its addresses/relay so the
        // ticket is dialable; proceed with whatever we have on timeout.
        let _ = tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await;

        let addr = endpoint.addr();

        // Fail closed for a custom-relay policy: the ticket MUST advertise the
        // relay, or the consumer's `RelayChoice::for_join` sees no relay in the
        // ticket and silently falls back to n0's public relays — breaking the
        // custom-relay compliance guarantee. This happens when the configured
        // relay is unreachable at mint time (the `online()` wait above times out
        // without a home relay). Refuse to mint a relay-less ticket instead —
        // consistent with `bind_endpoint`'s refusal to fall back to public. (An
        // endpoint bound `RelayMode::Custom` only ever advertises the configured
        // relays, so a non-empty `relay_urls()` here is one of them.)
        if matches!(choice, RelayChoice::Custom(_)) && addr.relay_urls().next().is_none() {
            bail!(
                "relay not ready: the share endpoint has no relay address to put \
                 in the ticket (the configured relay may be unreachable). Refusing \
                 to mint a ticket that would let joiners fall back to public relays."
            );
        }

        // DANGER opt-in: embed each advertised relay's resolved auth token in the
        // ticket so joiners need no out-of-band config. This ships the relay
        // secret inside the shareable link — only reached when the host set
        // `sharing.dangerouslyEmbedRelayTokensInTicket`.
        let relay_tokens = if embed_relay_tokens {
            resolve_embedded_tokens(&choice, addr.relay_urls())
                .await
                .context("resolving relay tokens to embed in the ticket")?
        } else {
            std::collections::BTreeMap::new()
        };

        let iroh_ticket = EndpointTicket::new(addr).to_string();

        let upstreams: HashMap<String, u16> = manifest
            .nodes
            .iter()
            .map(|n| (n.hostname.clone(), n.upstream_port))
            .collect();

        let host_share = Arc::new(HostShare {
            capability: capability.clone(),
            upstreams,
            manifest: manifest.clone(),
        });

        let ticket = ShareTicket {
            iroh_ticket,
            capability,
            relay_tokens,
        };
        let token = ticket.encode().context("encoding ticket")?;

        let id = gen_id("shr");
        let expires_at = manifest.expires_at;
        self.shares.lock().await.insert(
            id.clone(),
            ShareEntry {
                id: id.clone(),
                manifest,
                host_share,
                approve_mode,
                ticket: token,
                expires_at,
                relay: choice,
                web: None,
            },
        );
        info!(share_id = %id, ?approve_mode, "share started");
        Ok((id, ticket))
    }

    /// Attach a gateway registration to a (web) share and start its heartbeat
    /// loop. Errors if the share vanished in the meantime (the caller then
    /// unregisters from the gateway).
    pub async fn attach_web_registration(
        self: &Arc<Self>,
        share_id: &str,
        client: GatewayClient,
        reg_id: String,
        lease_secs: u64,
        public_urls: Vec<GatewayPublicUrl>,
        access: Option<veld_core::share::GatewayAccessPolicy>,
    ) -> Result<()> {
        // Heartbeat well inside the lease window; floor guards a tiny lease.
        let interval = Duration::from_secs((lease_secs / 3).max(5));
        let hb_manager = Arc::clone(self);
        let hb_share_id = share_id.to_string();
        let hb_client = client.clone();
        let hb_access = access.clone();

        let mut shares = self.shares.lock().await;
        let Some(entry) = shares.get_mut(share_id) else {
            bail!("share {share_id} ended before the gateway registration completed");
        };
        let ticket = entry.ticket.clone();
        let heartbeat = tokio::spawn(async move {
            // Consecutive heartbeats whose ack didn't confirm the access
            // policy. We only fail closed after several in a row so one
            // anomalous ack (or a single stale replica seen during a rolling
            // gateway upgrade) can't strand a healthy password share — while a
            // genuine rollback still closes within a few beats.
            let mut bad_acks = 0u32;
            loop {
                tokio::time::sleep(interval).await;
                // Self-terminate once the share is gone (unshare also aborts
                // this task; this covers any path that missed it).
                if !hb_manager.shares.lock().await.contains_key(&hb_share_id) {
                    break;
                }
                // The access policy rides every heartbeat: a restarted
                // gateway holds no state, so the beat that re-establishes the
                // share must re-establish its password too.
                match hb_client.register(&ticket, hb_access.as_ref()).await {
                    Ok(resp) => {
                        // Re-verify the ack on EVERY beat, not just at share
                        // start: a gateway rollback to a pre-access-layer (or
                        // non-enforcing) build would re-register the same
                        // deterministic slugs served wide open. Fail closed —
                        // kill the share rather than leave it exposed.
                        if let Some(access) = &hb_access {
                            match super::api::verify_access_ack(access, resp.access.as_ref()) {
                                Ok(()) => bad_acks = 0,
                                Err(msg) => {
                                    bad_acks += 1;
                                    warn!(share_id = %hb_share_id, bad_acks, %msg,
                                        "gateway heartbeat ack did not confirm the access policy");
                                    if bad_acks >= ACK_FAILURE_LIMIT {
                                        error!(share_id = %hb_share_id,
                                            "gateway stopped enforcing this share's access \
                                             policy across {ACK_FAILURE_LIMIT} beats (rolled \
                                             back?); unsharing to fail closed");
                                        // Detached: unshare() aborts THIS task,
                                        // so run it off-task to guarantee the
                                        // teardown completes rather than
                                        // cancelling itself mid-way.
                                        let m = Arc::clone(&hb_manager);
                                        let id = hb_share_id.clone();
                                        tokio::spawn(async move {
                                            let _ = m.unshare(&id).await;
                                        });
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Transient gateway failures self-heal on a later beat
                        // (a restarted gateway re-joins and mints the same
                        // URLs). A network failure is not evidence the policy
                        // stopped being enforced, so it does not count toward
                        // the fail-closed threshold.
                        warn!(share_id = %hb_share_id, error = %format!("{e:#}"),
                            "gateway heartbeat failed; will retry");
                    }
                }
            }
        });
        entry.web = Some(WebRegistration {
            client,
            reg_id,
            public_urls,
            access,
            heartbeat,
        });
        Ok(())
    }

    /// Join a shared environment: dial the host, then materialise each shared
    /// URL locally as a Caddy route tunnelled over the connection.
    ///
    /// `supplied_tokens` are relay auth tokens the caller is providing this
    /// attempt (relay URL → token), typically from an interactive prompt after a
    /// prior `needs_relay_token` response. When `remember` is set they are
    /// cached locally on success. If the ticket's relay is token-gated and no
    /// valid token is available, returns a `JoinResponse` with `needs_relay_token`
    /// set (no join performed) so the caller can prompt and retry.
    pub async fn join(
        self: &Arc<Self>,
        ticket_str: &str,
        label: &str,
        supplied_tokens: &BTreeMap<String, String>,
        remember: bool,
    ) -> Result<JoinResponse> {
        let ticket = ShareTicket::decode(ticket_str).context("decoding ticket")?;

        // Idempotent for a *successful* join: opening the same link again returns
        // the existing live join (with its original warnings) rather than dialing
        // twice. A prior all-skipped join (no URLs materialised) is torn down so
        // this attempt can retry; a left join is already gone, so it re-joins.
        {
            let existing = {
                let joins = self.joins.lock().await;
                joins
                    .values()
                    .find(|j| j.capability.ct_eq(&ticket.capability))
                    .map(|j| (j.id.clone(), j.urls.clone(), j.warnings.clone()))
            };
            if let Some((id, urls, warnings)) = existing {
                if !urls.is_empty() {
                    return Ok(JoinResponse {
                        join_id: id,
                        urls,
                        warnings,
                        needs_relay_token: None,
                    });
                }
                let _ = self.leave(&id).await;
            }
        }

        let addr: EndpointAddr = EndpointTicket::from_str(&ticket.iroh_ticket)
            .context("parsing iroh ticket")?
            .endpoint_addr()
            .clone();

        // Bind the join endpoint on the SAME relay(s) the host advertised in
        // the ticket. A share minted on a custom relay must be joined over that
        // relay — never silently over n0's public relays. Only a relay-less
        // ticket (a direct-address-only host) resolves to the public endpoint;
        // a custom-relay host refuses to mint such a ticket (see `start_share`).
        //
        // Relay auth tokens (if the relay is token-gated) are resolved by
        // priority — local cache < env < ticket-embedded < just-supplied —
        // attached only to the matching relay so nothing leaks.
        let stored = token_store::load();
        let tokens = RelayChoice::resolve_join_tokens_from_env(
            addr.relay_urls(),
            &ticket.relay_tokens,
            &stored,
            supplied_tokens,
        );
        // The canonical URLs of the relays this ticket actually advertises —
        // used below to cache only tokens for these relays (never a bogus key a
        // client might have tacked onto `supplied_tokens`).
        let ticket_relay_urls: Vec<String> = addr.relay_urls().map(|u| u.to_string()).collect();
        let choice = RelayChoice::for_join(addr.relay_urls(), &tokens);
        let endpoint = self.get_or_bind(&choice).await?;

        // If the ticket's relay is token-gated, the endpoint's home-relay
        // connection is denied ("not authorized") when the token is missing or
        // wrong. Detect that up front and ask the caller for a token rather than
        // letting the dial time out with a vague "unreachable". A public-relay
        // join is also `Custom` here (the ticket advertises the relay URL), but
        // an open relay just connects → `OkOrUnknown`, so this only prompts when
        // a relay genuinely rejects auth.
        if matches!(choice, RelayChoice::Custom(_)) {
            if let RelayAuth::Denied(relay_url) =
                relay_auth_status(&endpoint, RELAY_AUTH_TIMEOUT).await
            {
                // Evict this endpoint: it was bound only to detect the denial
                // and will never be reused — a retry supplies a token, which is a
                // *different* `RelayChoice` (the token is part of the key) and
                // binds a fresh endpoint. Leaving it in the map would leak an
                // endpoint + a permanently-denied relay connection for the
                // daemon's life, once per token-gated relay on the default path.
                drop(endpoint);
                self.evict_endpoint(&choice).await;
                return Ok(JoinResponse {
                    join_id: String::new(),
                    urls: Vec::new(),
                    warnings: Vec::new(),
                    needs_relay_token: Some(relay_url),
                });
            }
        }

        let label = if label.is_empty() { "veld" } else { label };
        // The host sends the manifest over the tunnel after approving — the
        // ticket itself carries none, keeping it short. Bounded so a browser
        // join doesn't hang forever on an unreachable host.
        let (conn, manifest) = match tokio::time::timeout(
            DIAL_TIMEOUT,
            join::dial(&endpoint, addr, &ticket.capability, label),
        )
        .await
        {
            Ok(res) => res?,
            Err(_) => bail!("timed out connecting to the host (unreachable, or no relay path)"),
        };

        let helper = HelperClient::connect()
            .await
            .context("connecting to veld-helper")?;

        let join_id = gen_id("join");
        let mut urls = Vec::new();
        let mut nodes = Vec::new();
        let mut routes = Vec::new();
        let mut tasks = Vec::new();
        let mut warnings = Vec::new();

        for node in &manifest.nodes {
            // Local URL wins: never clobber a hostname this machine already
            // serves — from one of its own runs, or from another active join.
            if hostname_in_use_locally(&node.hostname)
                || self.hostname_in_active_join(&node.hostname).await
            {
                warnings.push(format!(
                    "skipped '{}': {} is already in use locally (local URL wins)",
                    node.node, node.hostname
                ));
                continue;
            }

            // Per-node setup is non-fatal: on any failure we clean up what we
            // started for this node, warn, and move on. Bind an OS-assigned port
            // directly (we own the listener) — no allocator handoff, so no
            // leak/TOCTOU.
            let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
                Ok(l) => l,
                Err(e) => {
                    warnings.push(format!(
                        "skipped '{}': could not bind a local port ({e})",
                        node.node
                    ));
                    continue;
                }
            };
            let local_port = match listener.local_addr() {
                Ok(a) => a.port(),
                Err(e) => {
                    warnings.push(format!("skipped '{}': no local address ({e})", node.node));
                    continue;
                }
            };

            let conn_for_task = conn.clone();
            let hostname_for_task = node.hostname.clone();
            let handle = tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let conn = conn_for_task.clone();
                    let hostname = hostname_for_task.clone();
                    tokio::spawn(async move {
                        if let Err(e) = join::forward_local(&conn, &hostname, tcp).await {
                            debug!(error = %e, "forwarded stream ended with error");
                        }
                    });
                }
            });

            // Register DNS + Caddy route pointing at our local listener.
            let route_id = format!("veld-join-{join_id}-{}", node.node);
            if let Err(e) = helper.add_host(&node.hostname, "127.0.0.1").await {
                // add_host is a no-op for `.localhost` (RFC 6761); for custom
                // apex domains a failure means the URL won't resolve — warn.
                warnings.push(format!(
                    "'{}': DNS entry for {} may be incomplete ({e})",
                    node.node, node.hostname
                ));
            }
            let route = serde_json::json!({
                "route_id": route_id,
                "hostname": node.hostname,
                "upstream": format!("localhost:{local_port}"),
            });
            if let Err(e) = helper.add_route(route).await {
                // Undo everything we did for this node so nothing leaks.
                handle.abort();
                let _ = helper.remove_host(&node.hostname).await;
                warnings.push(format!(
                    "skipped '{}': route registration failed ({e})",
                    node.node
                ));
                continue;
            }

            tasks.push(handle);
            routes.push((node.hostname.clone(), route_id));
            urls.push(node.url.clone());
            nodes.push(node.node.clone());
        }

        let _ = helper.reload_dns().await;

        // Insert the join BEFORE spawning the drop-watcher: if the tunnel drops
        // in between, the watcher's leave() must find the entry to clean it up
        // (otherwise an orphan with stale routes is left behind).
        self.joins.lock().await.insert(
            join_id.clone(),
            JoinEntry {
                id: join_id.clone(),
                nodes,
                urls: urls.clone(),
                routes,
                tasks,
                conn: conn.clone(),
                capability: ticket.capability.clone(),
                relay: choice.clone(),
                warnings: warnings.clone(),
            },
        );

        // Self-heal: if the tunnel drops (host unshared, stopped, or crashed),
        // tear this join down locally so routes/listeners don't go stale.
        let watcher = Arc::clone(self);
        let watcher_id = join_id.clone();
        tokio::spawn(async move {
            conn.closed().await;
            let _ = watcher.leave(&watcher_id).await;
        });

        // The join authenticated to the relay, so any token the caller supplied
        // this attempt is valid — cache it (per relay URL) if asked, so future
        // joins to the same relay don't re-prompt. Best-effort: a cache write
        // failure must not fail the join.
        if remember {
            for (url, token) in tokens_to_cache(&ticket_relay_urls, supplied_tokens) {
                if let Err(e) = token_store::save(url, token) {
                    warn!(error = %e, relay = %url, "failed to cache relay token");
                }
            }
        }

        info!(join_id = %join_id, count = urls.len(), "joined share");
        Ok(JoinResponse {
            join_id,
            urls,
            warnings,
            needs_relay_token: None,
        })
    }

    /// List active shares and joins.
    pub async fn list(&self) -> SharesList {
        // Snapshot joiner counts first (separate lock) to avoid nested locking.
        let counts: HashMap<String, usize> = self
            .conns
            .lock()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect();
        let base = join_base();
        let shares = self
            .shares
            .lock()
            .await
            .values()
            .map(|s| ShareInfo {
                id: s.id.clone(),
                run: s.manifest.run.clone(),
                approve: Some(s.approve_mode),
                nodes: s.manifest.nodes.iter().map(|n| n.node.clone()).collect(),
                urls: s.manifest.nodes.iter().map(|n| n.url.clone()).collect(),
                // A web share's ticket is a secret held between this daemon
                // and the gateway — never surfaced for copy/paste.
                ticket: s.web.is_none().then(|| s.ticket.clone()),
                join_url: s.web.is_none().then(|| format!("{base}/join#{}", s.ticket)),
                joiners: counts.get(&s.id).copied().unwrap_or(0),
                public_urls: s
                    .web
                    .as_ref()
                    .map(|w| w.public_urls.clone())
                    .unwrap_or_default(),
                web_password: s
                    .web
                    .as_ref()
                    .and_then(|w| w.access.as_ref())
                    .and_then(|a| a.password.clone()),
            })
            .collect();
        let joins = self
            .joins
            .lock()
            .await
            .values()
            .map(|j| ShareInfo {
                id: j.id.clone(),
                run: String::new(),
                approve: None,
                nodes: j.nodes.clone(),
                urls: j.urls.clone(),
                ticket: None,
                join_url: None,
                joiners: 0,
                public_urls: Vec::new(),
                web_password: None,
            })
            .collect();
        let pending = self
            .pending
            .lock()
            .await
            .values()
            .map(|p| PendingInfo {
                id: p.id.clone(),
                share_id: p.share_id.clone(),
                label: p.label.clone(),
                node_id: p.node_id.clone(),
            })
            .collect();
        SharesList {
            shares,
            joins,
            pending,
        }
    }

    /// Resolve whether a join is approved, per the share's approval mode.
    async fn resolve_approval(
        self: &Arc<Self>,
        share_id: &str,
        mode: ApprovalMode,
        node_id: EndpointId,
        label: &str,
    ) -> bool {
        match mode {
            ApprovalMode::Auto => true,
            ApprovalMode::First => {
                let mut claims = self.claims.lock().await;
                match claims.get(share_id) {
                    None => {
                        claims.insert(share_id.to_string(), node_id);
                        info!(share = %share_id, node = %node_id, "first joiner claimed share");
                        true
                    }
                    // Re-connections from the same pinned peer are allowed.
                    Some(existing) => *existing == node_id,
                }
            }
            ApprovalMode::Manual => {
                let (tx, rx) = oneshot::channel();
                let req_id = gen_id("req");
                // Only pop the browser if this share has no request already
                // pending — avoids a new tab per retry / per concurrent joiner.
                let already_pending = {
                    let mut pending = self.pending.lock().await;
                    let had = pending.values().any(|p| p.share_id == share_id);
                    pending.insert(
                        req_id.clone(),
                        PendingRequest {
                            id: req_id.clone(),
                            share_id: share_id.to_string(),
                            label: label.to_string(),
                            node_id: node_id.to_string(),
                            decision: tx,
                        },
                    );
                    had
                };
                info!(req = %req_id, share = %share_id, label, "join awaiting approval");
                if !already_pending {
                    open_dashboard();
                }

                let approved = matches!(
                    tokio::time::timeout(APPROVAL_TIMEOUT, rx).await,
                    Ok(Ok(true))
                );
                self.pending.lock().await.remove(&req_id);
                approved
            }
        }
    }

    /// Release a `first`-mode claim held by `node_id` on `share_id` (no-op if a
    /// different node holds it or none does).
    async fn clear_claim(&self, share_id: &str, node_id: EndpointId) {
        let mut claims = self.claims.lock().await;
        if claims.get(share_id) == Some(&node_id) {
            claims.remove(share_id);
        }
    }

    /// True if a currently-joined share already materialises `hostname` locally
    /// (so a second join for the same hostname is skipped — local URL wins).
    async fn hostname_in_active_join(&self, hostname: &str) -> bool {
        self.joins
            .lock()
            .await
            .values()
            .any(|j| j.routes.iter().any(|(h, _)| h == hostname))
    }

    /// Approve a parked join request.
    pub async fn approve_request(&self, req_id: &str) -> Result<()> {
        let entry = self
            .pending
            .lock()
            .await
            .remove(req_id)
            .ok_or_else(|| anyhow::anyhow!("no such request: {req_id}"))?;
        let _ = entry.decision.send(true);
        info!(req = %req_id, "approved");
        Ok(())
    }

    /// Deny a parked join request.
    pub async fn deny_request(&self, req_id: &str) -> Result<()> {
        let entry = self
            .pending
            .lock()
            .await
            .remove(req_id)
            .ok_or_else(|| anyhow::anyhow!("no such request: {req_id}"))?;
        let _ = entry.decision.send(false);
        info!(req = %req_id, "denied");
        Ok(())
    }

    /// Stop hosting a share. In-flight connections end when their peers
    /// disconnect; no new connection will match the removed capability.
    pub async fn unshare(&self, id: &str) -> Result<()> {
        let Some(entry) = self.shares.lock().await.remove(id) else {
            bail!("no such share: {id}");
        };
        // Web share: stop heartbeating and unregister from the gateway so its
        // public URLs die now, not at lease expiry. Best-effort — a lost
        // DELETE is covered by the lease (and by the tunnel closing below).
        if let Some(web) = entry.web {
            web.heartbeat.abort();
            tokio::spawn(async move {
                if let Err(e) = web.client.unregister(&web.reg_id).await {
                    debug!(error = %format!("{e:#}"), "gateway unregister failed (lease will expire it)");
                }
            });
        }
        self.claims.lock().await.remove(id);
        // Revoke any requests parked awaiting approval for this share so a
        // pending approval can't admit a joiner after the share is gone.
        {
            let mut pending = self.pending.lock().await;
            let stale: Vec<String> = pending
                .iter()
                .filter(|(_, p)| p.share_id == id)
                .map(|(rid, _)| rid.clone())
                .collect();
            for rid in stale {
                if let Some(req) = pending.remove(&rid) {
                    let _ = req.decision.send(false);
                }
            }
        }
        // Close live tunnels so consumers stop being able to reach the host.
        if let Some(conns) = self.conns.lock().await.remove(id) {
            for conn in conns.into_values() {
                conn.close(0u32.into(), b"share stopped");
            }
        }
        info!(share_id = %id, "share stopped");
        Ok(())
    }

    /// Stop every share minted from a given run — called when the run is
    /// stopped, so shares don't outlive the environment they expose.
    pub async fn unshare_run(&self, run_id: Uuid) -> usize {
        let ids: Vec<String> = {
            let shares = self.shares.lock().await;
            shares
                .values()
                .filter(|s| s.manifest.run_id == run_id)
                .map(|s| s.id.clone())
                .collect()
        };
        let mut stopped = 0;
        for id in ids {
            if self.unshare(&id).await.is_ok() {
                stopped += 1;
            }
        }
        stopped
    }

    /// Change a hosted share's approval mode (the dashboard's auto-accept toggle).
    /// Applies to subsequent join attempts.
    pub async fn set_approve_mode(&self, id: &str, mode: ApprovalMode) -> Result<()> {
        let mut shares = self.shares.lock().await;
        let entry = shares
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("no such share: {id}"))?;
        entry.approve_mode = mode;
        info!(share_id = %id, ?mode, "approval mode changed");
        Ok(())
    }

    /// Leave a joined share: remove routes/DNS, drop listeners, close the tunnel.
    pub async fn leave(&self, id: &str) -> Result<()> {
        let entry = self
            .joins
            .lock()
            .await
            .remove(id)
            .ok_or_else(|| anyhow::anyhow!("no such join: {id}"))?;

        for task in &entry.tasks {
            task.abort();
        }
        // Close the tunnel (also wakes the drop-watcher so it doesn't dangle).
        entry.conn.close(0u32.into(), b"left");

        if let Ok(helper) = HelperClient::connect().await {
            for (hostname, route_id) in &entry.routes {
                let _ = helper.remove_route(route_id).await;
                let _ = helper.remove_host(hostname).await;
            }
            let _ = helper.reload_dns().await;
        } else {
            warn!("could not reach helper to remove routes on leave");
        }

        info!(join_id = %id, "left share");
        Ok(())
    }

    /// The ids of every live **web** share minted from `run_id`. Snapshotted
    /// so a repeat `veld share --web` can register its replacement first and
    /// only then tear these down — a failed re-share must not destroy the
    /// share it was replacing.
    pub async fn web_share_ids_for_run(&self, run_id: Uuid) -> Vec<String> {
        self.shares
            .lock()
            .await
            .values()
            .filter(|s| s.web.is_some() && s.manifest.run_id == run_id)
            .map(|s| s.id.clone())
            .collect()
    }

    /// Stop specific shares by id (their gateway registrations are unregistered
    /// by `unshare`). Returns how many stopped. Used to replace prior web
    /// shares once a fresh one is live.
    pub async fn unshare_ids(&self, ids: &[String]) -> usize {
        let mut stopped = 0;
        for id in ids {
            if self.unshare(id).await.is_ok() {
                stopped += 1;
            }
        }
        stopped
    }
}

fn gen_id(prefix: &str) -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &uuid[..8])
}

/// Base for browser join URLs, from the setup mode. Uses `veld.localhost` (via
/// Caddy) — never the daemon's raw `127.0.0.1:19899` — so a copied link works
/// on the recipient's machine.
pub(crate) fn join_base() -> String {
    let mode = dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".veld").join("setup.json")).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("mode").and_then(|m| m.as_str()).map(String::from));
    match mode.as_deref() {
        Some("unprivileged") => "https://veld.localhost:18443".to_string(),
        _ => "https://veld.localhost".to_string(),
    }
}

/// True if any of this machine's own runs already serves `hostname`.
fn hostname_in_use_locally(hostname: &str) -> bool {
    match GlobalRegistry::load() {
        Ok(reg) => reg.projects.values().any(|entry| {
            entry.runs.values().any(|run| {
                run.urls
                    .values()
                    .any(|u| super::api::hostname_of(u) == hostname)
            })
        }),
        Err(_) => false,
    }
}

/// Open the local dashboard in the default browser so the host can approve a
/// pending join. Best-effort and OS-agnostic; a no-op where there is no opener.
fn open_dashboard() {
    // Open the Caddy-fronted dashboard (veld.localhost), not the daemon's raw
    // 127.0.0.1:19899 port.
    let url = format!("{}/#shares", join_base());
    #[cfg(target_os = "macos")]
    let program: Option<&str> = Some("open");
    #[cfg(target_os = "linux")]
    let program: Option<&str> = Some("xdg-open");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let program: Option<&str> = None;

    if let Some(program) = program {
        if let Err(e) = std::process::Command::new(program).arg(&url).spawn() {
            debug!(error = %e, "could not open dashboard for approval");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::share::SharedNode;

    #[test]
    fn tokens_to_cache_keeps_only_ticket_relays() {
        let ticket = vec!["https://relay.example/".to_string()];
        let mut supplied = BTreeMap::new();
        supplied.insert("https://relay.example/".to_string(), "good".to_string());
        // A key the ticket never advertised (spurious / client-tacked-on).
        supplied.insert("https://attacker.example/".to_string(), "leak".to_string());

        let cached = tokens_to_cache(&ticket, &supplied);
        assert_eq!(cached, vec![("https://relay.example/", "good")]);
        // The non-ticket relay's token is never persisted.
        assert!(
            !cached
                .iter()
                .any(|(u, _)| *u == "https://attacker.example/")
        );
    }

    fn sample_manifest() -> ShareManifest {
        ShareManifest {
            run_id: Uuid::new_v4(),
            run: "demo".to_string(),
            project: "p".to_string(),
            nodes: vec![SharedNode {
                node: "app".to_string(),
                variant: "local".to_string(),
                hostname: "app.demo.p.localhost".to_string(),
                url: "https://app.demo.p.localhost".to_string(),
                upstream_port: 19001,
            }],
            created_at: 0,
            expires_at: i64::MAX,
        }
    }

    // A share stopped while a join is parked awaiting approval must revoke that
    // request (deny it) rather than leave it to admit a joiner post-unshare.
    #[tokio::test]
    async fn unshare_revokes_parked_pending() {
        let mgr = std::sync::Arc::new(ShareManager::new(SecretKey::generate()));
        let manifest = sample_manifest();
        let host_share = Arc::new(HostShare {
            capability: Capability::generate(),
            upstreams: HashMap::new(),
            manifest: manifest.clone(),
        });
        mgr.shares.lock().await.insert(
            "shr_1".to_string(),
            ShareEntry {
                id: "shr_1".to_string(),
                manifest,
                host_share,
                approve_mode: ApprovalMode::Manual,
                ticket: "veldshare_x".to_string(),
                expires_at: i64::MAX,
                relay: RelayChoice::Public,
                web: None,
            },
        );
        let (tx, rx) = oneshot::channel();
        mgr.pending.lock().await.insert(
            "req_1".to_string(),
            PendingRequest {
                id: "req_1".to_string(),
                share_id: "shr_1".to_string(),
                label: "bob".to_string(),
                node_id: "n".to_string(),
                decision: tx,
            },
        );

        mgr.unshare("shr_1").await.expect("unshare");

        assert!(mgr.shares.lock().await.is_empty(), "share removed");
        assert!(mgr.pending.lock().await.is_empty(), "pending drained");
        assert_eq!(rx.await, Ok(false), "parked request denied");
    }

    // A web share's ticket embeds the capability that IS the public-URL bearer
    // secret — `list()` must never surface it (no ticket, no join_url), and it
    // must surface the public URLs instead. Guards against a refactor of
    // `list()` re-leaking the web capability to any local `veld shares` caller.
    #[tokio::test]
    async fn list_never_surfaces_a_web_share_ticket() {
        let mgr = std::sync::Arc::new(ShareManager::new(SecretKey::generate()));
        let manifest = sample_manifest();
        let host_share = Arc::new(HostShare {
            capability: Capability::generate(),
            upstreams: HashMap::new(),
            manifest: manifest.clone(),
        });
        mgr.shares.lock().await.insert(
            "shr_web".to_string(),
            ShareEntry {
                id: "shr_web".to_string(),
                manifest,
                host_share,
                approve_mode: ApprovalMode::Auto,
                ticket: "veldshare_SECRET".to_string(),
                expires_at: i64::MAX,
                relay: RelayChoice::Public,
                web: Some(WebRegistration {
                    client: super::super::gateway::GatewayClient::for_test(),
                    reg_id: "reg_1".to_string(),
                    public_urls: vec![GatewayPublicUrl {
                        node: "app".to_string(),
                        hostname: "app.demo.p.localhost".to_string(),
                        public_url: "https://slug.share.example".to_string(),
                        access: Some(veld_core::config::WebAccessMode::Password),
                    }],
                    access: Some(veld_core::share::GatewayAccessPolicy {
                        password: Some("k7dm-q2xp-9fzt".to_string()),
                        nodes: Default::default(),
                    }),
                    heartbeat: tokio::spawn(async {}),
                }),
            },
        );

        let list = mgr.list().await;
        let info = &list.shares[0];
        assert_eq!(info.ticket, None, "web ticket must not be surfaced");
        assert_eq!(info.join_url, None, "web join_url must not be surfaced");
        assert_eq!(info.public_urls.len(), 1, "public URLs surfaced instead");
        assert_eq!(info.public_urls[0].public_url, "https://slug.share.example");
        // The raw ticket string appears nowhere in the serialized listing.
        let json = serde_json::to_string(&list).unwrap();
        assert!(
            !json.contains("veldshare_SECRET"),
            "ticket leaked into list: {json}"
        );
    }
}
