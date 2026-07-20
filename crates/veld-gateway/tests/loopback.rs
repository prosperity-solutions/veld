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

/// Minimal WebSocket-ish origin: completes the HTTP/1.1 upgrade handshake
/// (101), then echoes every raw byte back. We are testing the proxy plumbing
/// (upgrade detection → tunnel → splice), not the WebSocket protocol, so no
/// real frame parsing or Sec-WebSocket-Accept hashing is needed — the test
/// client doesn't validate it.
async fn spawn_ws_echo_origin() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                // Read the request head.
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    head.extend_from_slice(&buf[..n]);
                }
                let req = String::from_utf8_lossy(&head).to_ascii_lowercase();
                if !(req.contains("upgrade: websocket") && req.contains("connection: upgrade")) {
                    let _ = sock
                        .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
                        .await;
                    return;
                }
                // Mimic Next's dev-origin gate, tightened to a strict
                // superset: destroy the socket on ANY Origin header without
                // an HTTP response. (Real Next 15+/16 kills only origins
                // outside localhost/allowedDevOrigins — rejecting all origins
                // here pins the gateway's chosen strategy, dropping the
                // header, rather than any rewrite.)
                if req.contains("\r\norigin:") {
                    return;
                }
                // The forged Set-Cookie mimics a hostile upstream trying to
                // (re)set the gateway's own session cookie on the 101 — the
                // splice path must strip it (it bypasses
                // rewrite_response_headers).
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 101 Switching Protocols\r\n\
                          upgrade: websocket\r\n\
                          connection: Upgrade\r\n\
                          set-cookie: __Host-veld_gw_sess=forged; Path=/\r\n\
                          set-cookie: app_ws=legit; Path=/\r\n\
                          sec-websocket-accept: test-not-validated\r\n\r\n",
                    )
                    .await;
                // Echo raw bytes until the peer closes.
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    port
}

/// Serve `share` (the daemon's host half) on `host_ep` until the test ends.
fn serve_host(host_ep: iroh::Endpoint, share: Arc<HostShare>) {
    tokio::spawn(async move {
        while let Some(incoming) = host_ep.accept().await {
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
            proxy: None,
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

    let host_ep = bind_endpoint(
        SecretKey::generate(),
        &RelayChoice::Public,
        veld_share::endpoint::IpFamilies::default(),
    )
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
        veld_gateway::registry::RelayAllowList::Unconfined,
        SecretKey::generate(),
        512,
        veld_share::endpoint::IpFamilies::default(),
    );
    // Register with a §6.1 access policy: the node is password-protected.
    let mut access_nodes = std::collections::BTreeMap::new();
    access_nodes.insert(hostname.clone(), veld_core::config::WebAccessMode::Password);
    let policy = veld_core::share::GatewayAccessPolicy {
        password: Some("k7dm-q2xp-9fzt".into()),
        nodes: access_nodes,
    };
    let info = registry
        .register(&ticket, Some(&policy))
        .await
        .expect("register");
    assert_eq!(info.nodes.len(), 1);
    assert!(info.password_protected, "ack must reflect the policy");
    assert_eq!(
        info.nodes[0].access,
        veld_core::config::WebAccessMode::Password
    );
    let slug = info.nodes[0].slug.clone();
    assert_eq!(
        info.nodes[0].public_url,
        format!("https://{slug}.share.example")
    );

    // Registering again (heartbeat) is idempotent: same slug, same URL, and
    // the ack still reports the enforced policy.
    let again = registry
        .register(&ticket, Some(&policy))
        .await
        .expect("heartbeat");
    assert_eq!(again.nodes[0].slug, slug);
    assert!(again.password_protected);

    // The slug target carries the access mode + a session key that verifies
    // gateway-side tokens (the stateless session model end-to-end).
    let target = registry.lookup(&slug).await.expect("slug routes");
    assert_eq!(target.access, veld_core::config::WebAccessMode::Password);
    let key = target.registration.session_key();
    let token = veld_gateway::auth::mint_token(&key, &slug, i64::MAX - 1);
    assert!(veld_gateway::auth::verify_token(&key, &slug, 0, &token));

    // Proxy a real request through the tunnel via the slug route.
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
    // This test drives tunnel::connect/send_request directly (it sends
    // Host=hostname straight through), exercising the transport round-trip —
    // NOT proxy::handle's Host rewrite, which is covered by proxy.rs unit tests.
    assert_eq!(body, format!("host={hostname}"));
}

/// Full-stack WebSocket upgrade (Next.js HMR shape): raw client → real
/// gateway HTTP listener → dispatch → proxy::handle upgrade path → tunnel →
/// daemon host half → local origin, then bidirectional bytes over the spliced
/// connection. Guards the whole 101 plumbing end-to-end.
#[tokio::test]
#[ignore = "requires network; manual cross-crate drift check"]
async fn gateway_splices_websocket_upgrade_end_to_end() {
    let origin_port = spawn_ws_echo_origin().await;
    let hostname = "app.demo.p.localhost".to_string();

    // Host side — exactly what the daemon runs. This node opts into dropping the
    // Origin header (`proxy.request.remove: ["Origin"]`), the escape hatch for a
    // dev server that gates WS HMR on Origin and offers no allow-list. Without
    // it the gateway now rewrites Origin coherently (see proxy.rs), which the
    // strict echo origin below rejects — exercising the opt-in end-to-end.
    let capability = Capability::generate();
    let proxy = veld_core::config::ResolvedProxy {
        request: veld_core::config::HeaderRules {
            remove: vec!["Origin".into()],
            set: Default::default(),
        },
        response: Default::default(),
    };
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
            proxy: Some(proxy),
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

    let host_ep = bind_endpoint(
        SecretKey::generate(),
        &RelayChoice::Public,
        veld_share::endpoint::IpFamilies::default(),
    )
    .await
    .unwrap();
    host_ep.online().await;
    let ticket = ShareTicket {
        iroh_ticket: iroh_tickets::endpoint::EndpointTicket::new(host_ep.addr()).to_string(),
        capability,
        relay_tokens: Default::default(),
    };
    serve_host(host_ep.clone(), share);

    // Gateway side: registry + real HTTP listener serving the real router.
    let registry = veld_gateway::registry::Registry::new(
        "share.example".into(),
        std::time::Duration::from_secs(90),
        veld_gateway::registry::RelayAllowList::Unconfined,
        SecretKey::generate(),
        512,
        veld_share::endpoint::IpFamilies::default(),
    );
    // Link access: the slug itself is the credential — no password gate in
    // the way of the upgrade request.
    let mut access_nodes = std::collections::BTreeMap::new();
    access_nodes.insert(hostname.clone(), veld_core::config::WebAccessMode::Link);
    let policy = veld_core::share::GatewayAccessPolicy {
        password: None,
        nodes: access_nodes,
    };
    let info = registry
        .register(&ticket, Some(&policy))
        .await
        .expect("register");
    let slug = info.nodes[0].slug.clone();

    let config = veld_gateway::config::GatewayConfig {
        domain: "share.example".into(),
        listen: "127.0.0.1:0".parse().unwrap(),
        tls: None,
        auth_token: veld_core::config::SecretSource::Literal("test-token".into()),
        relays: None,
        lease: std::time::Duration::from_secs(90),
        state_dir: None,
        max_registrations: 512,
        trust_forwarded_headers: false,
        trust_forwarded_host: false,
        ip_families: veld_share::endpoint::IpFamilies::default(),
    };
    let state = veld_gateway::state::AppState {
        config: Arc::new(config),
        registry,
        auth_token: "test-token".into(),
        limiter: Arc::new(veld_gateway::auth::RateLimiter::default()),
    };
    let app = veld_gateway::server::router(state)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let handle = axum_server::Handle::new();
    let server = axum_server::bind("127.0.0.1:0".parse().unwrap()).handle(handle.clone());
    tokio::spawn(server.serve(app));
    let addr = handle.listening().await.expect("gateway bound");

    // Raw HTTP/1.1 WebSocket handshake, browser/Next-HMR shape — including
    // the Origin header a browser always sends. With this node's
    // `proxy.request.remove: ["Origin"]` opt-in the gateway must DROP it (the
    // strict echo origin above kills the socket if it sees one).
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let handshake = format!(
        "GET /_next/webpack-hmr?id=abc123 HTTP/1.1\r\n\
         Host: {slug}.share.example\r\n\
         Connection: keep-alive, Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: x3JJHMbDL1EzLkh9GBhXDw==\r\n\
         Origin: https://{slug}.share.example\r\n\r\n"
    );
    client.write_all(handshake.as_bytes()).await.unwrap();

    // Read the response head.
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tokio::time::timeout(std::time::Duration::from_secs(30), client.read(&mut buf))
            .await
            .expect("response head within 30s")
            .expect("read response head");
        assert!(n > 0, "connection closed before a response head");
        head.extend_from_slice(&buf[..n]);
    }
    let head_end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let head_str = String::from_utf8_lossy(&head[..head_end]).to_string();
    assert!(
        head_str.starts_with("HTTP/1.1 101"),
        "expected 101 Switching Protocols, got:\n{head_str}"
    );
    let lower = head_str.to_ascii_lowercase();
    assert!(lower.contains("upgrade: websocket"), "got:\n{head_str}");
    // The 101 path bypasses rewrite_response_headers, but must still strip an
    // upstream attempt to (re)set the gateway's own session cookie — while
    // passing the app's own cookies through.
    assert!(
        !lower.contains("__host-veld_gw_sess"),
        "gateway session cookie must not cross the splice path:\n{head_str}"
    );
    assert!(
        lower.contains("set-cookie: app_ws=legit"),
        "app cookies must pass through on the 101:\n{head_str}"
    );

    // Bidirectional bytes over the spliced connection (echo origin).
    let mut echoed: Vec<u8> = head[head_end..].to_vec();
    client.write_all(b"hmr-ping").await.unwrap();
    while echoed.len() < b"hmr-ping".len() {
        let n = tokio::time::timeout(std::time::Duration::from_secs(30), client.read(&mut buf))
            .await
            .expect("echo within 30s")
            .expect("read echo");
        assert!(n > 0, "connection closed before the echo");
        echoed.extend_from_slice(&buf[..n]);
    }
    assert_eq!(&echoed, b"hmr-ping");
    handle.shutdown();
}
