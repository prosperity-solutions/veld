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
use veld_core::port::PortAllocator;
use veld_core::share::{
    ApprovalMode, Capability, JoinResponse, PendingInfo, ShareInfo, ShareManifest, ShareTicket,
    SharesList,
};
use veld_core::state::GlobalRegistry;

/// Timeout a manual approval waits before auto-denying.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the reaper scans for expired shares.
const REAPER_INTERVAL: Duration = Duration::from_secs(60);

use super::endpoint::bind_endpoint;
use super::host::{self, HostShare};
use super::join;

/// A share this daemon is hosting.
struct ShareEntry {
    id: String,
    manifest: ShareManifest,
    host_share: Arc<HostShare>,
    approve_mode: ApprovalMode,
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
    /// Live inbound connections per hosted share, so `unshare` can close them.
    conns: Mutex<HashMap<String, Vec<Connection>>>,
    ports: PortAllocator,
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
            ports: PortAllocator::new(),
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
                    let conn = match incoming.await {
                        Ok(conn) => conn,
                        Err(e) => {
                            debug!(error = %e, "incoming connection failed");
                            return;
                        }
                    };
                    let (req, send, recv) = match host::read_control(&conn).await {
                        Ok(parts) => parts,
                        Err(e) => {
                            debug!(error = %e, "control handshake failed");
                            return;
                        }
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

                    if approved {
                        debug!(label = %req.label, share = %share_id, "join approved");
                        mgr.register_conn(&share_id, conn.clone()).await;
                        let _ = host::accept_and_serve(conn, send, host_share).await;
                    } else {
                        host::deny(send, "join denied").await;
                    }
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

    /// Track a live inbound connection so `unshare` can force it closed.
    async fn register_conn(&self, share_id: &str, conn: Connection) {
        self.conns
            .lock()
            .await
            .entry(share_id.to_string())
            .or_default()
            .push(conn);
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
        });

        let ticket = ShareTicket {
            iroh_ticket,
            manifest: manifest.clone(),
            capability,
        };

        let id = gen_id("shr");
        let expires_at = manifest.expires_at;
        self.shares.lock().await.insert(
            id.clone(),
            ShareEntry {
                id: id.clone(),
                manifest,
                host_share,
                approve_mode,
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
        let addr: EndpointAddr = EndpointTicket::from_str(&ticket.iroh_ticket)
            .context("parsing iroh ticket")?
            .endpoint_addr()
            .clone();

        let label = if label.is_empty() { "veld" } else { label };
        let conn = join::dial(&endpoint, addr, &ticket.capability, label).await?;

        let helper = HelperClient::connect()
            .await
            .context("connecting to veld-helper")?;

        let join_id = gen_id("join");
        let mut urls = Vec::new();
        let mut nodes = Vec::new();
        let mut routes = Vec::new();
        let mut tasks = Vec::new();
        let mut warnings = Vec::new();

        for node in &ticket.manifest.nodes {
            // Local URL wins: never clobber a hostname this machine already
            // serves from one of its own runs.
            if hostname_in_use_locally(&node.hostname) {
                warnings.push(format!(
                    "skipped '{}': {} is already in use locally (local URL wins)",
                    node.node, node.hostname
                ));
                continue;
            }

            // Per-node setup is non-fatal: on any failure we clean up what we
            // started for this node, warn, and move on — never leaving a leaked
            // listener, bound port, or half-registered route behind.
            let reservation = match self.ports.allocate() {
                Ok(r) => r,
                Err(e) => {
                    warnings.push(format!("skipped '{}': no local port ({e})", node.node));
                    continue;
                }
            };
            let local_port = reservation.port;
            reservation.release();

            let listener = match TcpListener::bind(("127.0.0.1", local_port)).await {
                Ok(l) => l,
                Err(e) => {
                    warnings.push(format!(
                        "skipped '{}': bind {local_port} failed ({e})",
                        node.node
                    ));
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
            let _ = helper.add_host(&node.hostname, "127.0.0.1").await;
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

        // Self-heal: if the tunnel drops (host unshared, stopped, or crashed),
        // tear this join down locally so routes/listeners don't go stale.
        let watcher = Arc::clone(self);
        let watcher_id = join_id.clone();
        let watch_conn = conn.clone();
        tokio::spawn(async move {
            watch_conn.closed().await;
            let _ = watcher.leave(&watcher_id).await;
        });

        self.joins.lock().await.insert(
            join_id.clone(),
            JoinEntry {
                id: join_id.clone(),
                nodes,
                urls: urls.clone(),
                routes,
                tasks,
                conn,
            },
        );
        info!(join_id = %join_id, count = urls.len(), "joined share");
        Ok(JoinResponse {
            join_id,
            urls,
            warnings,
        })
    }

    /// List active shares and joins.
    pub async fn list(&self) -> SharesList {
        let shares = self
            .shares
            .lock()
            .await
            .values()
            .map(|s| ShareInfo {
                id: s.id.clone(),
                nodes: s.manifest.nodes.iter().map(|n| n.node.clone()).collect(),
                urls: s.manifest.nodes.iter().map(|n| n.url.clone()).collect(),
            })
            .collect();
        let joins = self
            .joins
            .lock()
            .await
            .values()
            .map(|j| ShareInfo {
                id: j.id.clone(),
                nodes: j.nodes.clone(),
                urls: j.urls.clone(),
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
                self.pending.lock().await.insert(
                    req_id.clone(),
                    PendingRequest {
                        id: req_id.clone(),
                        share_id: share_id.to_string(),
                        label: label.to_string(),
                        node_id: node_id.to_string(),
                        decision: tx,
                    },
                );
                info!(req = %req_id, share = %share_id, label, "join awaiting approval");
                open_dashboard();

                let approved = matches!(
                    tokio::time::timeout(APPROVAL_TIMEOUT, rx).await,
                    Ok(Ok(true))
                );
                self.pending.lock().await.remove(&req_id);
                approved
            }
        }
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
        // Close live tunnels so consumers stop being able to reach the host.
        if let Some(conns) = self.conns.lock().await.remove(id) {
            for conn in conns {
                conn.close(0u32.into(), b"share stopped");
            }
        }
        info!(share_id = %id, "share stopped");
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
    let url = "http://127.0.0.1:19899/#shares";
    #[cfg(target_os = "macos")]
    let program: Option<&str> = Some("open");
    #[cfg(target_os = "linux")]
    let program: Option<&str> = Some("xdg-open");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let program: Option<&str> = None;

    if let Some(program) = program {
        if let Err(e) = std::process::Command::new(program).arg(url).spawn() {
            debug!(error = %e, "could not open dashboard for approval");
        }
    }
}
