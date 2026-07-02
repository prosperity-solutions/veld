//! Host side of a share: accept an iroh connection, gate on the capability,
//! then forward each data stream to the requested local service.
//!
//! Approval is auto-grant on a valid capability here (Phase 1b). The
//! `manual`/`first` approval flow (parking the connection until the host
//! approves) is layered on in Phase 3 by resolving approval *before* the
//! [`ControlResponse::approved`] reply is sent.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, bail};
use iroh::endpoint::Connection;
use tokio::net::TcpStream;
use tracing::{debug, warn};
use veld_core::share::Capability;

use super::{forward, proto};

/// What a host is willing to serve on one share: the capability that gates it
/// and the map of shared hostname → local upstream port.
pub struct HostShare {
    pub capability: Capability,
    pub upstreams: HashMap<String, u16>,
}

/// Serve a single accepted connection: control handshake, then a data-stream
/// loop until the peer disconnects.
pub async fn serve_connection(conn: Connection, share: Arc<HostShare>) -> Result<()> {
    // --- Control handshake ---
    let (mut send, mut recv) = conn.accept_bi().await?;
    let req: proto::ControlRequest = proto::read_json(&mut recv).await?;

    if !req.capability.ct_eq(&share.capability) {
        let _ = proto::write_json(&mut send, &proto::ControlResponse::denied("invalid token")).await;
        bail!("rejected join from {:?}: invalid capability", req.label);
    }

    proto::write_json(&mut send, &proto::ControlResponse::approved()).await?;
    debug!(label = %req.label, "join approved");

    // --- Data streams: one bi-stream per proxied TCP connection ---
    loop {
        let (send, mut recv) = match conn.accept_bi().await {
            Ok(streams) => streams,
            // Peer closed the connection: clean end of the share session.
            Err(_) => break,
        };

        let share = Arc::clone(&share);
        tokio::spawn(async move {
            let open: proto::OpenStream = match proto::read_json(&mut recv).await {
                Ok(open) => open,
                Err(e) => {
                    debug!(error = %e, "malformed open-stream frame; dropping");
                    return;
                }
            };

            let Some(&port) = share.upstreams.get(&open.hostname) else {
                warn!(hostname = %open.hostname, "requested hostname not in shared scope; dropping");
                return;
            };

            match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(tcp) => {
                    if let Err(e) = forward::splice(tcp, send, recv).await {
                        debug!(error = %e, hostname = %open.hostname, "tunnel stream ended with error");
                    }
                }
                Err(e) => warn!(error = %e, port, "failed to dial local upstream"),
            }
        });
    }

    Ok(())
}
