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
use iroh::{Endpoint, EndpointAddr, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, OnceCell};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use veld_core::helper::HelperClient;
use veld_core::port::PortAllocator;
use veld_core::share::{
    Capability, JoinResponse, ShareInfo, ShareManifest, ShareTicket, SharesList,
};
use uuid::Uuid;

use super::endpoint::bind_endpoint;
use super::host::{self, HostShare};
use super::join;

/// A share this daemon is hosting.
struct ShareEntry {
    id: String,
    manifest: ShareManifest,
    host_share: Arc<HostShare>,
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
    /// Keeps the QUIC connection alive for the duration of the join.
    _conn: Connection,
}

/// Owns the iroh endpoint and all live shares/joins.
pub struct ShareManager {
    secret_key: SecretKey,
    endpoint: OnceCell<Endpoint>,
    shares: Mutex<HashMap<String, ShareEntry>>,
    joins: Mutex<HashMap<String, JoinEntry>>,
    ports: PortAllocator,
}

impl ShareManager {
    pub fn new(secret_key: SecretKey) -> Self {
        Self {
            secret_key,
            endpoint: OnceCell::new(),
            shares: Mutex::new(HashMap::new()),
            joins: Mutex::new(HashMap::new()),
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
                            .map(|s| Arc::clone(&s.host_share))
                    };

                    match matched {
                        Some(host_share) => {
                            debug!(label = %req.label, "accepted join");
                            let _ = host::accept_and_serve(conn, send, host_share).await;
                        }
                        None => host::deny(send, "unknown or expired share").await,
                    }
                });
            }
        });
    }

    /// Register a share for `manifest`, minting a ticket. The capability gates
    /// all inbound connections to this share.
    pub async fn start_share(
        self: &Arc<Self>,
        manifest: ShareManifest,
        capability: Capability,
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
        self.shares.lock().await.insert(
            id.clone(),
            ShareEntry {
                id: id.clone(),
                manifest,
                host_share,
            },
        );
        info!(share_id = %id, "share started");
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

        for node in &ticket.manifest.nodes {
            // Allocate a local port, then release the guard so we can bind our
            // own listener on it (mirrors the orchestrator's port handoff).
            let reservation = self.ports.allocate().context("allocating local port")?;
            let local_port = reservation.port;
            reservation.release();

            let listener = TcpListener::bind(("127.0.0.1", local_port))
                .await
                .with_context(|| format!("binding local listener on {local_port}"))?;

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
            tasks.push(handle);

            // Register DNS + Caddy route pointing at our local listener.
            let route_id = format!("veld-join-{join_id}-{}", node.node);
            let _ = helper.add_host(&node.hostname, "127.0.0.1").await;
            let route = serde_json::json!({
                "route_id": route_id,
                "hostname": node.hostname,
                "upstream": format!("localhost:{local_port}"),
            });
            helper
                .add_route(route)
                .await
                .with_context(|| format!("adding route for {}", node.hostname))?;

            routes.push((node.hostname.clone(), route_id));
            urls.push(node.url.clone());
            nodes.push(node.node.clone());
        }

        let _ = helper.reload_dns().await;

        self.joins.lock().await.insert(
            join_id.clone(),
            JoinEntry {
                id: join_id.clone(),
                nodes,
                urls: urls.clone(),
                routes,
                tasks,
                _conn: conn,
            },
        );
        info!(join_id = %join_id, count = urls.len(), "joined share");
        Ok(JoinResponse { join_id, urls })
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
        SharesList { shares, joins }
    }

    /// Stop hosting a share. In-flight connections end when their peers
    /// disconnect; no new connection will match the removed capability.
    pub async fn unshare(&self, id: &str) -> Result<()> {
        if self.shares.lock().await.remove(id).is_none() {
            bail!("no such share: {id}");
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
