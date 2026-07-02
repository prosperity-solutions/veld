//! Host side of a share: accept an iroh connection, gate on the capability,
//! then forward each data stream to the requested local service.
//!
//! Approval is auto-grant on a valid capability here (Phase 1b/2). The
//! `manual`/`first` approval flow (parking the connection until the host
//! approves) is layered on in Phase 3 by resolving approval *before*
//! [`accept_and_serve`] sends its reply.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use iroh::endpoint::{Connection, RecvStream, SendStream};
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

/// Read the control request that opens every connection. The caller inspects
/// the capability, then either [`accept_and_serve`] or [`deny`].
pub async fn read_control(
    conn: &Connection,
) -> Result<(proto::ControlRequest, SendStream, RecvStream)> {
    let (send, mut recv) = conn.accept_bi().await?;
    let req: proto::ControlRequest = proto::read_json(&mut recv).await?;
    Ok((req, send, recv))
}

/// Reply "denied" on the control stream.
pub async fn deny(mut send: SendStream, reason: &str) {
    let _ = proto::write_json(&mut send, &proto::ControlResponse::denied(reason)).await;
}

/// Reply "approved", then run the data-stream loop until the peer disconnects.
pub async fn accept_and_serve(
    conn: Connection,
    mut send: SendStream,
    share: Arc<HostShare>,
) -> Result<()> {
    proto::write_json(&mut send, &proto::ControlResponse::approved()).await?;
    run_data_loop(conn, share).await
}

/// Forward each incoming data stream to its requested shared upstream.
async fn run_data_loop(conn: Connection, share: Arc<HostShare>) -> Result<()> {
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

/// Convenience for a single-share context (tests): handshake against one share's
/// capability, then serve.
#[cfg(test)]
pub async fn serve_connection(conn: Connection, share: Arc<HostShare>) -> Result<()> {
    let (req, send, recv) = read_control(&conn).await?;
    if req.capability.ct_eq(&share.capability) {
        drop(recv);
        accept_and_serve(conn, send, share).await
    } else {
        deny(send, "invalid token").await;
        Ok(())
    }
}
