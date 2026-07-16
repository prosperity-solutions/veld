//! Cross-crate drift guard (SHARING_V2.md §5.2): the gateway joins a share
//! served by the *same host half the daemon uses* (`veld_share::host`) and
//! proxies a real HTTP request through the tunnel. If the gateway ever drifts
//! from the protocol the daemon speaks, this fails.
//!
//! Marked `#[ignore]` for the same reason as `veld-share`'s loopback test: it
//! needs UDP + (potentially) n0 relay reachability. Run manually with
//! `cargo test -p veld-gateway -- --ignored loopback`.

use std::collections::HashMap;
use std::sync::Arc;

use iroh::SecretKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use veld_core::share::{Capability, ShareManifest, ShareTicket, SharedNode};
use veld_share::endpoint::{RelayChoice, bind_endpoint};
use veld_share::host::{self, HostShare};

/// Minimal HTTP/1.1 origin standing in for a dev server: answers every
/// request with a body echoing the request's Host header.
async fn spawn_origin() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let host = req
                    .lines()
                    .find_map(|l| l.strip_prefix("host: ").or(l.strip_prefix("Host: ")))
                    .unwrap_or("?")
                    .to_owned();
                let body = format!("host={host}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

#[tokio::test]
#[ignore = "requires network; manual cross-crate drift check"]
async fn gateway_proxies_through_daemon_host_half() {
    let origin_port = spawn_origin().await;
    let hostname = "app.demo.p.localhost".to_string();

    // Host side — exactly what the daemon runs.
    let capability = Capability::generate();
    let manifest = ShareManifest {
        run_id: uuid::Uuid::new_v4(),
        run: "demo".into(),
        project: "p".into(),
        nodes: vec![SharedNode {
            node: "app".into(),
            variant: "local".into(),
            hostname: hostname.clone(),
            url: format!("https://{hostname}"),
            upstream_port: origin_port,
        }],
        created_at: 0,
        expires_at: i64::MAX,
    };
    let mut upstreams = HashMap::new();
    upstreams.insert(hostname.clone(), origin_port);
    let share = Arc::new(HostShare {
        capability: capability.clone(),
        upstreams,
        manifest,
    });

    let host_ep = bind_endpoint(SecretKey::generate(), &RelayChoice::Public)
        .await
        .unwrap();
    host_ep.online().await;
    let ticket = ShareTicket {
        iroh_ticket: iroh_tickets::endpoint::EndpointTicket::new(host_ep.addr()).to_string(),
        capability,
        relay_tokens: Default::default(),
    };

    let host_ep2 = host_ep.clone();
    tokio::spawn(async move {
        while let Some(incoming) = host_ep2.accept().await {
            let share = Arc::clone(&share);
            tokio::spawn(async move {
                if let Ok(conn) = incoming.await {
                    let Ok((req, send, recv)) = host::read_control(&conn).await else {
                        return;
                    };
                    drop(recv);
                    if req.capability.ct_eq(&share.capability) {
                        let _ = host::accept_and_serve(conn, send, share).await;
                    } else {
                        host::deny(send, "invalid token").await;
                    }
                }
            });
        }
    });

    // Gateway side: register (joins over iroh, mints deterministic slugs).
    let registry = veld_gateway::registry::Registry::new(
        "share.example".into(),
        std::time::Duration::from_secs(90),
        None,
        SecretKey::generate(),
    );
    let info = registry.register(&ticket).await.expect("register");
    assert_eq!(info.nodes.len(), 1);
    let slug = info.nodes[0].slug.clone();
    assert_eq!(
        info.nodes[0].public_url,
        format!("https://{slug}.share.example")
    );

    // Registering again (heartbeat) is idempotent: same slug, same URL.
    let again = registry.register(&ticket).await.expect("heartbeat");
    assert_eq!(again.nodes[0].slug, slug);

    // Proxy a real request through the tunnel via the slug route.
    let target = registry.lookup(&slug).await.expect("slug routes");
    let mut sender = veld_gateway::tunnel::connect(&target.registration.conn, &hostname)
        .await
        .expect("tunnel connect");
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/")
        .header("host", &hostname)
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = sender.send_request(req).await.expect("send");
    assert!(resp.status().is_success());
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("body")
        .to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    // The origin saw its own hostname (origin-Host rewrite policy sends the
    // origin hostname upstream).
    assert_eq!(body, format!("host={hostname}"));
}
