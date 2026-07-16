//! HTTP/1.1 client connections over iroh tunnel streams.
//!
//! The host side of a share splices each accepted bi-stream to the shared
//! service's local TCP port (see `veld_share::host`), so from this end a
//! bi-stream *is* a byte-for-byte TCP connection to the origin service. This
//! module opens such a stream (sending the `OpenStream` routing frame first)
//! and hands it to hyper as an HTTP/1.1 client connection.

use anyhow::{Context, Result};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use iroh::endpoint::Connection;
use veld_share::proto;

/// An HTTP/1.1 request sender bound to one freshly-opened tunnel stream.
pub type Sender = http1::SendRequest<axum::body::Body>;

/// Open a new bi-stream to `hostname` on the share behind `conn` and perform
/// an HTTP/1.1 client handshake over it. The returned sender carries exactly
/// one logical connection; the driver task runs until the stream closes and
/// supports protocol upgrades (WebSockets).
pub async fn connect(conn: &Connection, hostname: &str) -> Result<Sender> {
    let (mut send, recv) = conn
        .open_bi()
        .await
        .context("opening tunnel stream to the host")?;
    proto::write_json(
        &mut send,
        &proto::OpenStream {
            hostname: hostname.to_string(),
        },
    )
    .await
    .context("sending open-stream frame")?;

    let io = TokioIo::new(tokio::io::join(recv, send));
    let (sender, connection) = http1::Builder::new()
        .handshake::<_, axum::body::Body>(io)
        .await
        .context("HTTP/1.1 handshake over tunnel stream")?;

    // Drive the connection until it completes. `with_upgrades` keeps the
    // stream alive past a 101 so WebSocket splicing works.
    tokio::spawn(async move {
        if let Err(e) = connection.with_upgrades().await {
            tracing::debug!(error = %e, "tunnel http connection ended with error");
        }
    });

    Ok(sender)
}
