//! Shared transport layer for Veld's peer-to-peer sharing over iroh.
//!
//! Both halves of the share protocol live here — the **host** side (serve a
//! share, gate on the capability, forward data streams to local upstreams) and
//! the **join** side (dial a ticket, complete the control handshake, forward
//! local TCP connections over the tunnel) — together with endpoint
//! construction, relay policy resolution, and the wire protocol itself.
//!
//! `veld-daemon` (the developer-machine host/consumer) and `veld-gateway` (the
//! headless public-web joiner) both compile against this crate, so the
//! protocol cannot drift between them: a change here is picked up by both
//! binaries or neither, and the versioned [`endpoint::ALPN`] is defined in
//! exactly one place.
//!
//! What does **not** live here, deliberately: share/join lifecycle management
//! (Caddy routes, approval UX, reapers, token caches) — that is each binary's
//! own habitat.

pub mod endpoint;
pub mod forward;
pub mod host;
pub mod join;
pub mod proto;
pub mod status;

pub use endpoint::ALPN;

#[cfg(test)]
mod tests {
    use super::endpoint::{ALPN, load_or_create_secret_key};

    #[test]
    fn alpn_is_versioned() {
        assert_eq!(ALPN, b"veld/share/1");
    }

    // Full loopback tunnel: host endpoint serves an echo service, consumer dials
    // over iroh and proxies a local TCP connection through. Marked `#[ignore]`
    // because it needs UDP + (potentially) n0 relay reachability; run manually
    // with `cargo test -p veld-share -- --ignored tunnel`.
    #[tokio::test]
    #[ignore = "requires network; manual transport check"]
    async fn full_tunnel_echoes_over_iroh() {
        use super::endpoint::{RelayChoice, bind_endpoint};
        use super::{host::HostShare, join};
        use iroh::SecretKey;
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};
        use veld_core::share::ShareManifest;

        // Local echo server standing in for the shared dev service.
        let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = sock.read(&mut buf).await {
                        if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        let host_ep = bind_endpoint(SecretKey::generate(), &RelayChoice::Public)
            .await
            .unwrap();
        let client_ep = bind_endpoint(SecretKey::generate(), &RelayChoice::Public)
            .await
            .unwrap();
        host_ep.online().await;
        let host_addr = host_ep.addr();

        let capability = veld_core::share::Capability::generate();
        let hostname = "app.demo.irohtest.localhost".to_string();
        let mut upstreams = HashMap::new();
        upstreams.insert(hostname.clone(), echo_port);
        let manifest = ShareManifest {
            run_id: uuid::Uuid::new_v4(),
            run: "demo".to_string(),
            project: "irohtest".to_string(),
            nodes: vec![],
            created_at: 0,
            expires_at: 0,
        };
        let share = Arc::new(HostShare {
            capability: capability.clone(),
            upstreams,
            manifest,
        });

        // Host accept loop.
        let host_ep2 = host_ep.clone();
        tokio::spawn(async move {
            if let Some(incoming) = host_ep2.accept().await {
                if let Ok(conn) = incoming.await {
                    let _ = super::host::serve_connection(conn, share).await;
                }
            }
        });

        // Consumer dials, opens a data stream, echoes bytes.
        let (conn, _manifest) = join::dial(&client_ep, host_addr, &capability, "test")
            .await
            .unwrap();

        // Local listener the consumer would register with Caddy.
        let local = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let local_port = local.local_addr().unwrap().port();
        let conn2 = conn.clone();
        let hostname2 = hostname.clone();
        tokio::spawn(async move {
            if let Ok((tcp, _)) = local.accept().await {
                let _ = join::forward_local(&conn2, &hostname2, tcp).await;
            }
        });

        let mut c = TcpStream::connect(("127.0.0.1", local_port)).await.unwrap();
        c.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        c.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[test]
    fn secret_key_persists_and_reloads() {
        let path = std::env::temp_dir().join(format!(
            "veld-node-key-test-{}-{}",
            std::process::id(),
            // vary per test invocation without needing rand
            line!()
        ));
        let _ = std::fs::remove_file(&path);

        let first = load_or_create_secret_key(&path).expect("create key");
        let second = load_or_create_secret_key(&path).expect("reload key");

        assert_eq!(
            first.public(),
            second.public(),
            "reloaded key must yield the same public identity"
        );

        let _ = std::fs::remove_file(&path);
    }
}
