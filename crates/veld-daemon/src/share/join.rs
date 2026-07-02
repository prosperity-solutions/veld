//! Consumer side of a join: dial the host, complete the control handshake, then
//! forward each locally-accepted TCP connection over its own data stream.

use anyhow::{Result, bail};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use tokio::net::TcpStream;
use veld_core::share::Capability;

use super::endpoint::ALPN;
use super::{forward, proto};

/// Dial the host and complete the control handshake. Returns the live
/// connection on approval; errors if the host denies or is unreachable.
pub async fn dial(
    endpoint: &Endpoint,
    addr: impl Into<EndpointAddr>,
    capability: &Capability,
    label: &str,
) -> Result<Connection> {
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

    Ok(conn)
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
