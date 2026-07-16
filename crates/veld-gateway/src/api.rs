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
    GatewayPublicUrl, GatewayRegisterRequest, GatewayRegisterResponse, ShareTicket,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/shares", post(register))
        .route("/api/v1/shares/{id}", delete(unregister))
}

pub async fn healthz() -> &'static str {
    "ok"
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

    let info = state.registry.register(&ticket).await.map_err(|e| {
        // The failure detail is for the origin daemon's logs/CLI; it never
        // reaches an anonymous browser (this route only answers on the apex).
        warn!(error = %format!("{e:#}"), "share registration failed");
        (StatusCode::BAD_GATEWAY, format!("{e:#}"))
    })?;

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
            })
            .collect(),
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

/// Shared 404 for unmatched hosts/paths — deliberately content-free so probes
/// of the public surface learn nothing.
pub async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
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
