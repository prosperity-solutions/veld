//! Listener, Host-based dispatch, and graceful shutdown. One listener serves
//! two audiences, split by `Host`:
//!
//! - the **apex domain** answers the Bearer-gated registration API that origin
//!   daemons drive;
//! - **slug subdomains** are proxied to the registered share over its tunnel;
//! - anything else is a content-free 404 (except `/healthz`, answered for any
//!   host so container/LB probes work without knowing the domain).

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
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
    );
    tokio::spawn(Arc::clone(&registry).sweep_expired_leases());

    let state = AppState {
        config: Arc::new(config),
        registry,
        auth_token: auth_token.into(),
    };

    let app = Router::new()
        .fallback(dispatch)
        .with_state(state.clone())
        .into_make_service_with_connect_info::<std::net::SocketAddr>();

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
        .header_read_timeout(HEADER_READ_TIMEOUT);
}

/// Route a request by `Host`: apex → registration API, `<slug>.<domain>` →
/// proxy, anything else → `/healthz` or a content-free 404.
async fn dispatch(State(state): State<AppState>, req: Request) -> Response {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(host_without_port)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let domain = &state.config.domain;
    if host == *domain {
        return match api::router().with_state(state.clone()).oneshot(req).await {
            Ok(resp) => resp,
            Err(never) => match never {},
        };
    }

    if let Some(slug) = slug_of(&host, domain) {
        if let Some(target) = state.registry.lookup(slug).await {
            return proxy::handle(state, target, req).await;
        }
        return api::not_found().await.into_response();
    }

    // Unknown host: answer health probes (containers/LBs probe by IP or an
    // internal name, not the public domain); everything else is a 404.
    if req.uri().path() == "/healthz" {
        return api::healthz().await.into_response();
    }
    api::not_found().await.into_response()
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
    fn host_port_stripping() {
        assert_eq!(host_without_port("a.example:8443"), "a.example");
        assert_eq!(host_without_port("a.example"), "a.example");
        assert_eq!(host_without_port("[::1]:8080"), "::1");
    }
}
