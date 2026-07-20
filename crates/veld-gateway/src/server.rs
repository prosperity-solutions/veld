//! Listener, Host-based dispatch, and graceful shutdown. One listener serves
//! two audiences, split by `Host`:
//!
//! - the **apex domain** answers the Bearer-gated registration API that origin
//!   daemons drive, plus a branded index page on `/`;
//! - **slug subdomains** are proxied to the registered share over its tunnel;
//! - anything else is a branded 404 (except the health endpoints `/healthz`,
//!   `/livez`, and `/readyz`, answered for any host so container/LB probes
//!   work without knowing the domain).

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Request, State};
use axum::response::Response;
use tower::util::ServiceExt as _;
use tracing::{info, warn};

use crate::config::GatewayConfig;
use crate::registry::Registry;
use crate::state::AppState;
use crate::{api, proxy};

/// Max time a client may take to send its request headers (slowloris guard).
const HEADER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

pub async fn run(config: GatewayConfig) -> Result<()> {
    // Resolve the registration auth token once, up front: a misconfigured
    // token source fails the boot, never a request.
    let auth_token = veld_share::endpoint::resolve_secret(&config.auth_token)
        .await
        .context("resolving the gateway registration auth token")?;

    // Resolve the relay allow-list (and any relay tokens) once, up front — a
    // bad token source fails the boot, not a registration, and a `command`
    // source can't be re-triggered per registration.
    let relays = crate::registry::RelayAllowList::resolve(config.relays.as_ref())
        .await
        .context("resolving the gateway relay allow-list")?;

    let secret_key = node_key(&config)?;
    let registry = Registry::new(
        config.domain.clone(),
        config.lease,
        relays,
        secret_key,
        config.max_registrations,
        config.ip_families,
    );
    tokio::spawn(Arc::clone(&registry).sweep_expired_leases());

    let state = AppState {
        config: Arc::new(config),
        registry,
        auth_token: auth_token.into(),
        limiter: Arc::new(crate::auth::RateLimiter::default()),
    };

    let app = router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();

    let handle = axum_server::Handle::new();
    tokio::spawn(shutdown_on_signal(handle.clone()));

    let cfg = &state.config;
    match &cfg.tls {
        Some(tls) => {
            info!(listen = %cfg.listen, domain = %cfg.domain, "gateway listening (TLS)");
            let rustls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert, &tls.key)
                .await
                .context("loading TLS certificate/key")?;
            let mut server = axum_server::bind_rustls(cfg.listen, rustls).handle(handle);
            harden(&mut server);
            server.serve(app).await.context("serving (TLS)")?;
        }
        None => {
            info!(
                listen = %cfg.listen,
                domain = %cfg.domain,
                "gateway listening (plain HTTP — expecting an external TLS terminator)"
            );
            let mut server = axum_server::bind(cfg.listen).handle(handle);
            harden(&mut server);
            server.serve(app).await.context("serving")?;
        }
    }
    Ok(())
}

/// Bound how long a connection may take to send its request headers. Closes
/// the slowloris hole on the built-in-TLS path where the gateway is the direct
/// internet edge (behind an external terminator the terminator handles it).
/// This times out only the inbound header read — response streaming and
/// WebSocket upgrades are unaffected.
fn harden<A>(server: &mut axum_server::Server<A>) {
    server
        .http_builder()
        .http1()
        // hyper panics on every connection if a timeout is configured without
        // a timer ("timeout `header_read_timeout` set, but no timer set").
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
}

/// The gateway's routing service: every request (any host, any path) enters
/// [`dispatch`]. Public so integration tests can serve the real router.
pub fn router(state: AppState) -> Router {
    Router::new().fallback(dispatch).with_state(state)
}

/// Route a request by the viewer's host: apex → registration API + index page,
/// `<slug>.<domain>` → proxy, anything else → health endpoints or a branded 404.
async fn dispatch(State(state): State<AppState>, req: Request) -> Response {
    let host = viewer_host(req.headers(), state.config.trust_forwarded_host)
        .map(host_without_port)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let domain = &state.config.domain;
    if host == *domain {
        // Health probes answer before the API router — same single source of
        // truth as the unknown-host branch below.
        if let Some(resp) = api::health_response(req.uri().path()) {
            return resp;
        }
        return match api::router().with_state(state.clone()).oneshot(req).await {
            Ok(resp) => resp,
            Err(never) => match never {},
        };
    }

    if let Some(slug) = slug_of(&host, domain) {
        if let Some(target) = state.registry.lookup(slug).await {
            // Viewer access gate (§6.1) — runs BEFORE any tunnel stream is
            // opened, so an unauthenticated request costs the origin nothing.
            let slug_auth = crate::auth::SlugAuth::of(&target);
            return match crate::auth::gate(&state, &slug_auth, req).await {
                crate::auth::Gate::Allow(req) => proxy::handle(state, target, req).await,
                crate::auth::Gate::Respond(resp) => resp,
            };
        }
        return crate::pages::not_found(crate::pages::NotFound::Share);
    }

    // Unknown host: answer health probes (containers/LBs probe by IP or an
    // internal name, not the public domain); everything else is a 404. The
    // probe paths live in `api::health_response` — the one place to add or
    // retire them.
    match api::health_response(req.uri().path()) {
        Some(resp) => resp,
        None => crate::pages::not_found(crate::pages::NotFound::Generic),
    }
}

/// The host the viewer actually addressed. Behind a trusted edge
/// (`trust_forwarded_host`) an inbound `X-Forwarded-Host` wins over `Host`:
/// a CDN (CloudFront and friends) rewrites `Host` to its origin's hostname
/// when forwarding, so the host the viewer typed only survives the extra hop
/// in `X-Forwarded-Host`. Untrusted (default): `Host` alone — forwarding
/// headers from an anonymous-reachable edge are viewer-supplied.
///
/// In a comma-separated chain the **first** entry is taken (the host the
/// original client requested; hops append). Note the asymmetry with
/// `X-Forwarded-For`, where the trusted position is the LAST entry (the one
/// the trusted edge appended): first-is-original only holds if the edge
/// **overwrites or strips** any inbound `X-Forwarded-Host` — which is why the
/// flag's contract (config.rs) demands exactly that, and the documented
/// CloudFront function assigns (never appends) the header.
pub(crate) fn viewer_host(headers: &axum::http::HeaderMap, trust_forwarded: bool) -> Option<&str> {
    if trust_forwarded {
        let forwarded = headers
            .get("x-forwarded-host")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if forwarded.is_some() {
            return forwarded;
        }
    }
    headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
}

/// The slug label if `host` is exactly `<label>.<domain>` (one label deep).
fn slug_of<'a>(host: &'a str, domain: &str) -> Option<&'a str> {
    let label = host.strip_suffix(domain)?.strip_suffix('.')?;
    (!label.is_empty() && !label.contains('.')).then_some(label)
}

/// Strip an optional `:port` from a Host header value (IPv6 literals keep
/// their brackets' content intact).
fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    host.split(':').next().unwrap_or(host)
}

/// The gateway's persistent iroh identity. When an operator explicitly
/// configures a `state_dir` (VELD_GATEWAY_STATE_DIR — the container default),
/// a key that can't be persisted there is a **hard error**: silently
/// degrading to an ephemeral key would change `host_node_id` — and therefore
/// every public slug — on the next restart, quietly defeating the stable-URL
/// guarantee the volume is there to provide (a root-owned mount is the usual
/// cause). With no state_dir configured we fall back to the platform data dir
/// and, if even that isn't writable, an ephemeral key with a warning
/// (stateless container, no volume — fine, shares die with the process).
fn node_key(config: &GatewayConfig) -> Result<iroh::SecretKey> {
    if let Some(dir) = &config.state_dir {
        let path = dir.join("node.key");
        return veld_share::endpoint::load_or_create_secret_key(&path).with_context(|| {
            format!(
                "persisting the gateway node key at {} (a configured state_dir must be \
                 writable — a root-owned volume mount is the usual cause; chown it to the \
                 container user, or unset VELD_GATEWAY_STATE_DIR to run with an ephemeral \
                 identity)",
                path.display()
            )
        });
    }
    if let Some(dir) = dirs::data_dir().map(|d| d.join("veld-gateway")) {
        let path = dir.join("node.key");
        match veld_share::endpoint::load_or_create_secret_key(&path) {
            Ok(key) => return Ok(key),
            Err(e) => {
                warn!(error = %format!("{e:#}"), path = %path.display(),
                    "could not persist node key; using an ephemeral identity");
            }
        }
    }
    Ok(iroh::SecretKey::generate())
}

async fn shutdown_on_signal(handle: axum_server::Handle) {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
    info!("shutdown signal received; draining");
    handle.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};

    fn test_state() -> AppState {
        AppState {
            config: Arc::new(GatewayConfig {
                domain: "share.example".into(),
                listen: "127.0.0.1:0".parse().unwrap(),
                tls: None,
                auth_token: veld_core::config::SecretSource::Literal("t".into()),
                relays: None,
                lease: std::time::Duration::from_secs(90),
                state_dir: None,
                max_registrations: 8,
                trust_forwarded_headers: false,
                trust_forwarded_host: false,
                ip_families: veld_share::endpoint::IpFamilies::default(),
            }),
            registry: Registry::new(
                "share.example".into(),
                std::time::Duration::from_secs(90),
                crate::registry::RelayAllowList::Unconfined,
                iroh::SecretKey::generate(),
                8,
                veld_share::endpoint::IpFamilies::default(),
            ),
            auth_token: "t".into(),
            limiter: Arc::new(crate::auth::RateLimiter::default()),
        }
    }

    async fn dispatch_to(host: &str, path: &str) -> Response {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap();
        match router(test_state()).oneshot(req).await {
            Ok(resp) => resp,
            Err(never) => match never {},
        }
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn apex_root_serves_the_branded_index() {
        let resp = dispatch_to("share.example", "/").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        let body = body_string(resp).await;
        assert!(body.contains("Veld gateway"), "{body}");
        assert!(body.contains("class=\"wordmark\""), "{body}");
    }

    #[tokio::test]
    async fn health_endpoints_answer_on_apex_and_unknown_hosts() {
        for host in ["share.example", "10.0.0.7", "gateway.internal:8080"] {
            for path in ["/healthz", "/livez", "/readyz"] {
                let resp = dispatch_to(host, path).await;
                assert_eq!(resp.status(), StatusCode::OK, "{host}{path}");
                assert_eq!(body_string(resp).await, "ok", "{host}{path}");
            }
        }
        // Deliberately method-agnostic (probes are GET/HEAD, but the paths
        // are reserved wholesale — pinned so a change is a conscious one).
        let post = Request::builder()
            .method("POST")
            .uri("/livez")
            .header(header::HOST, "share.example")
            .body(Body::empty())
            .unwrap();
        let resp = match router(test_state()).oneshot(post).await {
            Ok(resp) => resp,
            Err(never) => match never {},
        };
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_slug_gets_the_branded_share_not_found() {
        let resp = dispatch_to("nosuchslug.share.example", "/").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_string(resp).await;
        assert!(body.contains("Share not found"), "{body}");
    }

    #[tokio::test]
    async fn apex_unmatched_path_and_unknown_host_get_the_generic_404() {
        for (host, path) in [
            ("share.example", "/nope"),
            ("evil.example", "/"),
            ("10.0.0.7", "/index.html"),
        ] {
            let resp = dispatch_to(host, path).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{host}{path}");
            let body = body_string(resp).await;
            assert!(body.contains("Nothing lives at this address"), "{body}");
        }
    }

    #[test]
    fn slug_extraction_is_exactly_one_label() {
        let d = "share.acme.internal";
        assert_eq!(slug_of("abc123.share.acme.internal", d), Some("abc123"));
        // Apex, deeper nesting, unrelated hosts, empty labels → no slug.
        assert_eq!(slug_of("share.acme.internal", d), None);
        assert_eq!(slug_of("a.b.share.acme.internal", d), None);
        assert_eq!(slug_of("evil.example", d), None);
        assert_eq!(slug_of(".share.acme.internal", d), None);
        // Suffix-similar but not subdomain-of (no dot boundary).
        assert_eq!(slug_of("evilshare.acme.internal", d), None);
    }

    #[test]
    fn viewer_host_honors_forwarded_only_when_trusted() {
        let headers = |xfh: Option<&str>| {
            let mut h = axum::http::HeaderMap::new();
            h.insert(axum::http::header::HOST, "origin.internal".parse().unwrap());
            if let Some(v) = xfh {
                h.insert("x-forwarded-host", v.parse().unwrap());
            }
            h
        };

        // Untrusted (default): a viewer-supplied X-Forwarded-Host is ignored.
        assert_eq!(
            viewer_host(&headers(Some("slug.share.acme.internal")), false),
            Some("origin.internal")
        );
        // Trusted: the forwarded host wins (CDN rewrote Host to its origin).
        assert_eq!(
            viewer_host(&headers(Some("slug.share.acme.internal")), true),
            Some("slug.share.acme.internal")
        );
        // Chain: first entry is the original viewer host.
        assert_eq!(
            viewer_host(&headers(Some("a.share.example, cdn.internal")), true),
            Some("a.share.example")
        );
        // Trusted but absent/empty → fall back to Host.
        assert_eq!(viewer_host(&headers(None), true), Some("origin.internal"));
        assert_eq!(
            viewer_host(&headers(Some("")), true),
            Some("origin.internal")
        );
    }

    #[test]
    fn host_port_stripping() {
        assert_eq!(host_without_port("a.example:8443"), "a.example");
        assert_eq!(host_without_port("a.example"), "a.example");
        assert_eq!(host_without_port("[::1]:8080"), "::1");
    }

    /// Regression: `header_read_timeout` without an explicit timer makes hyper
    /// panic on every connection ("timeout `header_read_timeout` set, but no
    /// timer set"), so a hardened server answered nothing. A request through
    /// the hardened listener must succeed.
    #[tokio::test]
    async fn hardened_server_answers_requests() {
        let app = axum::Router::new()
            .route("/healthz", axum::routing::get(|| async { "ok" }))
            .into_make_service();

        let handle = axum_server::Handle::new();
        let mut server = axum_server::bind("127.0.0.1:0".parse().unwrap()).handle(handle.clone());
        harden(&mut server);
        tokio::spawn(server.serve(app));

        let addr = handle.listening().await.expect("server bound");
        let resp = reqwest::get(format!("http://{addr}/healthz"))
            .await
            .expect("hardened server must answer, not panic the connection task");
        assert_eq!(resp.status(), 200);
        handle.shutdown();
    }
}
