//! In-memory manager for active shares and joins, plus the single iroh endpoint
//! the daemon uses for all P2P traffic.
//!
//! State is intentionally ephemeral: if the daemon stops, shares and joins stop
//! with it (fail-closed; a consumer then gets a clean connection error). Only
//! the node keypair persists, giving the daemon a stable identity.

use std::collections::HashMap;
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
use tracing::{debug, info, warn};
use uuid::Uuid;
use veld_core::helper::HelperClient;
use veld_core::share::{
    ApprovalMode, Capability, JoinResponse, PendingInfo, ShareInfo, ShareManifest, ShareTicket,
    SharesList,
};
use veld_core::state::GlobalRegistry;

/// Timeout a manual approval waits before auto-denying.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the reaper scans for expired shares.
const REAPER_INTERVAL: Duration = Duration::from_secs(60);
/// Max time to wait for an inbound peer to send its control frame before the
/// per-connection task gives up (pre-auth, so a leaked link can't pile up
/// stalled tasks holding a connection).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Max time a consumer's `join` waits to dial the host (hole-punch + approval)
/// before returning a clean error instead of hanging the HTTP request.
const DIAL_TIMEOUT: Duration = Duration::from_secs(75);

use super::endpoint::bind_endpoint;
use super::host::{self, HostShare};
use super::join;

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
    /// Non-fatal notes from the join (e.g. skipped nodes), preserved so a repeat
    /// open of the same link reports them instead of an empty list.
    warnings: Vec<String>,
}

/// Owns the iroh endpoint and all live shares/joins.
pub struct ShareManager {
    secret_key: SecretKey,
    endpoint: OnceCell<Endpoint>,
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
            endpoint: OnceCell::new(),
            shares: Mutex::new(HashMap::new()),
            joins: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            claims: Mutex::new(HashMap::new()),
            conns: Mutex::new(HashMap::new()),
        }
    }

    /// Lazily bind the endpoint on first use and start the accept loop that
    /// routes inbound connections to the matching share by capability.
    async fn endpoint(self: &Arc<Self>) -> Result<Endpoint> {
        let ep = self
            .endpoint
            .get_or_try_init(|| async {
                let ep = bind_endpoint(self.secret_key.clone()).await?;
                info!(node_id = %ep.id(), "iroh share endpoint bound");
                self.clone().spawn_accept_loop(ep.clone());
                self.clone().spawn_reaper();
                Ok::<_, anyhow::Error>(ep)
            })
            .await?;
        Ok(ep.clone())
    }

    /// Accept inbound connections and dispatch each to the share whose
    /// capability the peer presents.
    fn spawn_accept_loop(self: Arc<Self>, endpoint: Endpoint) {
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let mgr = Arc::clone(&self);
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

                    let matched = {
                        let shares = mgr.shares.lock().await;
                        shares
                            .values()
                            .find(|s| s.host_share.capability.ct_eq(&req.capability))
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

                    // The share may have been stopped (unshare/expiry) while this
                    // request was parked awaiting manual approval — re-check
                    // before serving so a stopped share can't keep admitting.
                    if !mgr.shares.lock().await.contains_key(&share_id) {
                        host::deny(send, "share stopped").await;
                        mgr.clear_claim(&share_id, node_id).await;
                        return;
                    }

                    debug!(label = %req.label, share = %share_id, "join approved");
                    let sid = conn.stable_id();
                    mgr.register_conn(&share_id, conn.clone()).await;
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
    ) -> Result<(String, ShareTicket)> {
        let endpoint = self.endpoint().await?;

        // Wait (bounded) for the endpoint to learn its addresses/relay so the
        // ticket is dialable; proceed with whatever we have on timeout.
        let _ = tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await;

        let iroh_ticket = EndpointTicket::new(endpoint.addr()).to_string();

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
            },
        );
        info!(share_id = %id, ?approve_mode, "share started");
        Ok((id, ticket))
    }

    /// Join a shared environment: dial the host, then materialise each shared
    /// URL locally as a Caddy route tunnelled over the connection.
    pub async fn join(self: &Arc<Self>, ticket_str: &str, label: &str) -> Result<JoinResponse> {
        let endpoint = self.endpoint().await?;
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
                    });
                }
                let _ = self.leave(&id).await;
            }
        }

        let addr: EndpointAddr = EndpointTicket::from_str(&ticket.iroh_ticket)
            .context("parsing iroh ticket")?
            .endpoint_addr()
            .clone();

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

        info!(join_id = %join_id, count = urls.len(), "joined share");
        Ok(JoinResponse {
            join_id,
            urls,
            warnings,
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
                ticket: Some(s.ticket.clone()),
                join_url: Some(format!("{base}/join#{}", s.ticket)),
                joiners: counts.get(&s.id).copied().unwrap_or(0),
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
        if self.shares.lock().await.remove(id).is_none() {
            bail!("no such share: {id}");
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
