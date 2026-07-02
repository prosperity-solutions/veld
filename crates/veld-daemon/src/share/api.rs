//! HTTP control API for sharing, merged into the daemon's axum server on
//! `127.0.0.1:19899`. The CLI (and dashboard) drive shares through these routes.
//!
//! Mutations require the `X-Veld-Request` header, matching the rest of the
//! management API's localhost-CSRF convention.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, response::IntoResponse};
use chrono::Utc;
use veld_core::share::{
    Capability, JoinRequest, JoinResponse, ShareManifest, SharedNode, SharesList,
    StartShareRequest, StartShareResponse,
};
use veld_core::state::{GlobalRegistry, ProjectState};

use super::manager::ShareManager;

const DEFAULT_TTL_SECS: i64 = 2 * 60 * 60;

/// Share routes with the manager baked in as state, ready to `.merge()`.
pub fn routes(manager: Arc<ShareManager>) -> Router {
    Router::new()
        .route("/api/shares", get(list).post(start))
        .route("/api/shares/join", post(join))
        .route("/api/shares/{id}", delete(unshare))
        .route("/api/shares/joins/{id}", delete(leave))
        .with_state(manager)
}

type ApiError = (StatusCode, String);

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn check_csrf(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.contains_key("x-veld-request") {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "missing X-Veld-Request header".to_string()))
    }
}

async fn start(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Json(req): Json<StartShareRequest>,
) -> Result<Json<StartShareResponse>, ApiError> {
    check_csrf(&headers)?;

    let manifest = build_manifest(req.run.as_deref(), req.nodes.as_deref(), req.ttl_secs)?;
    let node_names: Vec<String> = manifest.nodes.iter().map(|n| n.node.clone()).collect();
    let expires_at = manifest.expires_at;

    let capability = Capability::generate();
    let (share_id, ticket) = manager
        .start_share(manifest, capability)
        .await
        .map_err(internal)?;
    let token = ticket.encode().map_err(internal)?;

    Ok(Json(StartShareResponse {
        share_id,
        ticket: token,
        nodes: node_names,
        expires_at,
    }))
}

async fn join(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, ApiError> {
    check_csrf(&headers)?;
    let label = req.label.unwrap_or_default();
    let resp = manager
        .join(&req.ticket, &label)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(resp))
}

async fn list(State(manager): State<Arc<ShareManager>>) -> Json<SharesList> {
    Json(manager.list().await)
}

async fn unshare(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    check_csrf(&headers)?;
    manager
        .unshare(&id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn leave(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    check_csrf(&headers)?;
    manager
        .leave(&id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Resolve a run to a shareable manifest by reading persisted state.
fn build_manifest(
    run: Option<&str>,
    nodes_filter: Option<&[String]>,
    ttl_secs: Option<i64>,
) -> Result<ShareManifest, ApiError> {
    let registry = GlobalRegistry::load().map_err(internal)?;

    let run_name = match run {
        Some(r) => r.to_string(),
        None => sole_run(&registry)?,
    };

    let project_root = registry
        .projects
        .values()
        .find(|e| e.runs.contains_key(&run_name))
        .map(|e| e.project_root.clone())
        .ok_or((StatusCode::NOT_FOUND, format!("run '{run_name}' not found")))?;

    let project_state = ProjectState::load(&project_root).map_err(internal)?;
    let run_state = project_state
        .runs
        .get(&run_name)
        .ok_or((StatusCode::NOT_FOUND, format!("run '{run_name}' not found")))?;

    let mut nodes = Vec::new();
    for ns in run_state.nodes.values() {
        let (Some(url), Some(port)) = (ns.url.as_ref(), ns.port) else {
            continue;
        };
        if let Some(filter) = nodes_filter {
            if !filter.iter().any(|n| n == &ns.node_name) {
                continue;
            }
        }
        nodes.push(SharedNode {
            node: ns.node_name.clone(),
            variant: ns.variant.clone(),
            hostname: hostname_of(url),
            url: url.clone(),
            upstream_port: port,
        });
    }

    if nodes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("run '{run_name}' has no shareable (URL-bearing) nodes"),
        ));
    }

    let now = Utc::now().timestamp();
    let ttl = ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
    Ok(ShareManifest {
        run_id: run_state.run_id,
        project: run_state.project.clone(),
        nodes,
        created_at: now,
        expires_at: now + ttl,
    })
}

/// When no run is named, use the only running one; error if ambiguous.
fn sole_run(registry: &GlobalRegistry) -> Result<String, ApiError> {
    let mut names = registry.projects.values().flat_map(|e| e.runs.keys());
    match (names.next(), names.next()) {
        (Some(only), None) => Ok(only.clone()),
        (None, _) => Err((StatusCode::NOT_FOUND, "no active runs to share".to_string())),
        (Some(_), Some(_)) => Err((
            StatusCode::BAD_REQUEST,
            "multiple runs active; specify one with `veld share <run>`".to_string(),
        )),
    }
}

/// Strip scheme and port from a URL, leaving the bare hostname.
fn hostname_of(url: &str) -> String {
    let no_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    no_scheme
        .split('/')
        .next()
        .unwrap_or(no_scheme)
        .split(':')
        .next()
        .unwrap_or(no_scheme)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::hostname_of;

    #[test]
    fn hostname_strips_scheme_and_port() {
        assert_eq!(
            hostname_of("https://app.demo.irohtest.localhost:18443"),
            "app.demo.irohtest.localhost"
        );
        assert_eq!(
            hostname_of("https://frontend.x.proj.localhost"),
            "frontend.x.proj.localhost"
        );
    }
}
