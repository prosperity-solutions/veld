//! Registration API — served only on the apex domain
//! (`https://<domain>/api/v1/…`), Bearer-token gated.
//!
//! The origin daemon drives it: `POST /api/v1/shares` registers a share (and
//! doubles as the heartbeat), `DELETE /api/v1/shares/{id}` unregisters.
//! Browsers never talk to this API; slug hosts route to the proxy instead
//! (see `main::dispatch`).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tracing::warn;
use veld_core::share::{
    GatewayAccessAck, GatewayPublicUrl, GatewayRegisterRequest, GatewayRegisterResponse,
    ShareTicket,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/api/v1/shares", post(register))
        .route("/api/v1/shares/{id}", delete(unregister))
        .fallback(fallback_not_found)
}

async fn index() -> impl IntoResponse {
    crate::pages::index()
}

async fn fallback_not_found() -> impl IntoResponse {
    crate::pages::not_found(crate::pages::NotFound::Generic)
}

/// Answer container/LB health probes — the single source of truth for the
/// probe paths. `server::dispatch` consults this for the apex domain and for
/// unknown hosts alike (probes address the pod by IP or an internal name, so
/// the paths must answer on any Host); slug hosts never reach it — their
/// paths belong to the proxied app. Add or retire a probe path HERE, nowhere
/// else.
///
/// - `/livez` (and its legacy alias `/healthz`): liveness — the process is up
///   and answering. A failing liveness probe should restart the container.
/// - `/readyz`: readiness — safe to route traffic here. The gateway has no
///   warm-up phase or external dependency to await (the registry is
///   in-memory, tunnels are dialed per registration), so readiness equals
///   liveness today; it stays `ok` through the SIGTERM drain as well, where
///   traffic shedding is handled by the listener refusing new connections.
///   Kept as a distinct endpoint so orchestrator probe configs have a stable
///   target if that ever changes.
pub fn health_response(path: &str) -> Option<axum::response::Response> {
    match path {
        "/healthz" | "/livez" | "/readyz" => Some("ok".into_response()),
        _ => None,
    }
}

type ApiError = (StatusCode, String);

/// Constant-time bearer-token check. Never logs or echoes the presented value.
fn check_auth(headers: &HeaderMap, expected: &str) -> Result<(), ApiError> {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !presented.is_empty() && ct_eq(presented.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid gateway auth token".to_string(),
        ))
    }
}

/// Constant-time byte-slice equality (length leak is inherent and harmless —
/// token length is not secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GatewayRegisterRequest>,
) -> Result<Json<GatewayRegisterResponse>, ApiError> {
    check_auth(&headers, &state.auth_token)?;

    let ticket = ShareTicket::decode(&req.ticket)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid ticket: {e}")))?;

    let info = state
        .registry
        .register(&ticket, req.access.as_ref())
        .await
        .map_err(|e| {
            // The failure detail is for the origin daemon's logs/CLI; it never
            // reaches an anonymous browser (this route only answers on the apex).
            warn!(error = %format!("{e:#}"), "share registration failed");
            (StatusCode::BAD_GATEWAY, format!("{e:#}"))
        })?;

    // Ack what is actually enforced (§6.1 skew guard): a daemon that asked
    // for protection can verify it, and an old daemon ignores the field.
    let ack = GatewayAccessAck {
        password_protected: info.password_protected,
        nodes: info
            .nodes
            .iter()
            .map(|n| (n.hostname.clone(), n.access))
            .collect(),
    };

    Ok(Json(GatewayRegisterResponse {
        id: info.id,
        lease_secs: info.lease_secs,
        urls: info
            .nodes
            .into_iter()
            .map(|n| GatewayPublicUrl {
                node: n.node,
                hostname: n.hostname,
                public_url: n.public_url,
                access: Some(n.access),
            })
            .collect(),
        access: Some(ack),
    }))
}

async fn unregister(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    check_auth(&headers, &state.auth_token)?;
    // Idempotent: removing an already-gone registration is success (the lease
    // may have expired before the origin's DELETE arrived).
    state.registry.unregister(&id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_semantics() {
        assert!(ct_eq(b"same-token", b"same-token"));
        assert!(!ct_eq(b"same-token", b"other-tokn"));
        assert!(!ct_eq(b"short", b"longer-token"));
        // Empty presented token never authenticates even against empty
        // expected (config layer already rejects empty tokens).
        assert!(!check_auth(&HeaderMap::new(), "expected").is_ok());
    }

    #[test]
    fn auth_requires_bearer_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().unwrap(),
        );
        assert!(check_auth(&headers, "dXNlcjpwYXNz").is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer tok3n".parse().unwrap(),
        );
        assert!(check_auth(&headers, "tok3n").is_ok());
        assert!(check_auth(&headers, "other").is_err());
    }
}
