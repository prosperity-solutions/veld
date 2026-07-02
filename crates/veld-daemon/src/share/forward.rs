//! Bidirectional copy between a local TCP connection and an iroh QUIC stream.
//!
//! This is the core of the tunnel: on the host side we dial the real local
//! service and splice it to the QUIC stream; on the consumer side we accept a
//! local TCP connection and splice it to a QUIC stream dialed to the host.

use iroh::endpoint::{RecvStream, SendStream};
use tokio::io::{AsyncWriteExt, copy};
use tokio::net::TcpStream;

/// Splice `tcp` to the QUIC bi-stream (`send`/`recv`) until either direction
/// closes, then tear the other direction down cleanly so websocket-style
/// long-lived connections don't hang half-open.
pub async fn splice(tcp: TcpStream, mut send: SendStream, mut recv: RecvStream) -> std::io::Result<()> {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    let upstream = async {
        copy(&mut tcp_read, &mut send).await?;
        // Signal EOF to the peer; ignore if already closed.
        let _ = send.finish();
        Ok::<(), std::io::Error>(())
    };

    let downstream = async {
        copy(&mut recv, &mut tcp_write).await?;
        let _ = tcp_write.shutdown().await;
        Ok::<(), std::io::Error>(())
    };

    tokio::try_join!(upstream, downstream)?;
    Ok(())
}
