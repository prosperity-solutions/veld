//! Consumer side of a join: dial the host, complete the control handshake, then
//! forward each locally-accepted TCP connection over its own data stream.

use anyhow::{Result, bail};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use tokio::net::TcpStream;
use veld_core::share::{Capability, ShareManifest};

use super::endpoint::ALPN;
use super::{forward, proto};

/// Dial the host and complete the control handshake. On approval the host sends
/// the manifest (which URLs/ports to materialise), returned alongside the live
/// connection. Errors if the host denies or is unreachable.
pub async fn dial(
    endpoint: &Endpoint,
    addr: impl Into<EndpointAddr>,
    capability: &Capability,
    label: &str,
) -> Result<(Connection, ShareManifest)> {
    let conn = endpoint.connect(addr, ALPN).await?;

    let (mut send, mut recv) = conn.open_bi().await?;
    proto::write_json(
        &mut send,
        &proto::ControlRequest {
            capability: capability.clone(),
            label: label.to_string(),
        },
    )
    .await?;

    let resp: proto::ControlResponse = proto::read_json(&mut recv).await?;
    if !resp.approved {
        match resp.reason {
            Some(reason) => bail!("join denied: {reason}"),
            None => bail!("join denied"),
        }
    }

    let manifest = resp
        .manifest
        .ok_or_else(|| anyhow::anyhow!("host approved but sent no manifest"))?;
    Ok((conn, manifest))
}

/// Forward one locally-accepted TCP connection to `hostname` on the host over a
/// fresh data stream.
pub async fn forward_local(conn: &Connection, hostname: &str, tcp: TcpStream) -> Result<()> {
    let (mut send, recv) = conn.open_bi().await?;
    proto::write_json(
        &mut send,
        &proto::OpenStream {
            hostname: hostname.to_string(),
        },
    )
    .await?;
    forward::splice(tcp, send, recv).await?;
    Ok(())
}
