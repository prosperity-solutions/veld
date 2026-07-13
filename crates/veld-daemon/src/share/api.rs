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
use uuid::Uuid;
use veld_core::config::{ExposeMode, SharePolicy, VeldConfig, load_config};
use veld_core::share::{
    ApprovalMode, Capability, JoinRequest, JoinResponse, ShareManifest, SharedNode, SharesList,
    StartShareRequest, StartShareResponse,
};
use veld_core::state::{GlobalRegistry, ProjectState};

use super::endpoint::RelayChoice;
use super::manager::ShareManager;

const DEFAULT_TTL_SECS: i64 = 2 * 60 * 60;

/// Share routes with the manager baked in as state, ready to `.merge()`.
pub fn routes(manager: Arc<ShareManager>) -> Router {
    Router::new()
        .route("/api/shares", get(list).post(start))
        .route("/api/shares/join", post(join))
        .route("/api/shares/{id}", delete(unshare))
        .route("/api/shares/{id}/mode", post(set_mode))
        .route("/api/shares/by-run/{run_id}", delete(unshare_run))
        .route("/api/shares/joins/{id}", delete(leave))
        .route("/api/shares/requests/{id}/approve", post(approve))
        .route("/api/shares/requests/{id}/deny", post(deny))
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
        Err((
            StatusCode::FORBIDDEN,
            "missing X-Veld-Request header".to_string(),
        ))
    }
}

async fn start(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Json(req): Json<StartShareRequest>,
) -> Result<Json<StartShareResponse>, ApiError> {
    check_csrf(&headers)?;

    let ResolvedShare {
        manifest,
        relay,
        warnings,
    } = build_manifest(req.run.as_deref(), req.nodes.as_deref(), req.ttl_secs)?;
    let node_names: Vec<String> = manifest.nodes.iter().map(|n| n.node.clone()).collect();
    let expires_at = manifest.expires_at;

    let capability = Capability::generate();
    let (share_id, ticket) = manager
        .start_share(manifest, capability, req.approve.unwrap_or_default(), relay)
        .await
        .map_err(internal)?;
    let token = ticket.encode().map_err(internal)?;
    let join_url = format!("{}/join#{}", super::manager::join_base(), token);

    Ok(Json(StartShareResponse {
        share_id,
        ticket: token,
        join_url,
        nodes: node_names,
        expires_at,
        warnings,
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

#[derive(serde::Deserialize)]
struct ModeReq {
    approve: ApprovalMode,
}

async fn set_mode(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ModeReq>,
) -> Result<impl IntoResponse, ApiError> {
    check_csrf(&headers)?;
    manager
        .set_approve_mode(&id, req.approve)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unshare_run(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    check_csrf(&headers)?;
    let run_id = run_id
        .parse::<Uuid>()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid run id: {e}")))?;
    let stopped = manager.unshare_run(run_id).await;
    Ok(Json(serde_json::json!({ "unshared": stopped })))
}

async fn approve(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    check_csrf(&headers)?;
    manager
        .approve_request(&id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn deny(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    check_csrf(&headers)?;
    manager
        .deny_request(&id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// A manifest plus the relay policy the origin project declared, resolved
/// together from the same config so the share is both scoped (only opted-in
/// services) and routed (over the operator's relays).
struct ResolvedShare {
    manifest: ShareManifest,
    /// The relay this share routes over, resolved from an explicit opt-in.
    relay: RelayChoice,
    /// URL-bearing services excluded from the share (not opted into `peer`),
    /// surfaced as warnings so a partial share isn't silently under-exposed.
    warnings: Vec<String>,
}

/// Resolve a run to a shareable manifest by reading persisted state and the
/// project's config. Only services whose active variant opts into `peer`
/// sharing (`share.expose` contains `peer`) are included; this is the explicit
/// consent gate. The runtime `--node` filter narrows *within* the opted-in set
/// — it can never widen it.
fn build_manifest(
    run: Option<&str>,
    nodes_filter: Option<&[String]>,
    ttl_secs: Option<i64>,
) -> Result<ResolvedShare, ApiError> {
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

    let config = load_config(&project_root.join("veld.json")).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("could not load veld.json for run '{run_name}': {e}"),
        )
    })?;

    // Track why URL-bearing nodes were excluded, so the error can point the user
    // at the opt-in they are missing rather than a bare "nothing to share".
    // `node:variant` labels because `peer_opt_in` checks the *live* variant, and a
    // multi-variant node needs the opt-in on the running one specifically.
    let mut had_url_bearing = false;
    let mut not_opted_in: Vec<String> = Vec::new();
    let mut web_only: Vec<String> = Vec::new();
    let mut nodes = Vec::new();
    for ns in run_state.nodes.values() {
        let (Some(url), Some(port)) = (ns.url.as_ref(), ns.port) else {
            continue;
        };
        had_url_bearing = true;
        if let Some(filter) = nodes_filter {
            if !filter.iter().any(|n| n == &ns.node_name) {
                continue;
            }
        }
        let share = variant_share(&config, &ns.node_name, &ns.variant);
        if !share.is_some_and(|s| s.allows(ExposeMode::Peer)) {
            let label = format!("{}:{}", ns.node_name, ns.variant);
            // A `web`-only opt-in is a deliberate choice, not a missing one — call
            // it out distinctly instead of telling the user to "add peer".
            if share.is_some_and(|s| s.allows(ExposeMode::Web)) {
                web_only.push(label);
            } else {
                not_opted_in.push(label);
            }
            continue;
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
            share_exclusion_message(&run_name, had_url_bearing, &mut not_opted_in, &mut web_only),
        ));
    }

    // Partial share: some URL-bearing services were excluded. Warn rather than
    // silently under-expose (the excluded set is otherwise invisible to the user).
    not_opted_in.sort();
    not_opted_in.dedup();
    web_only.sort();
    web_only.dedup();
    let mut warnings = Vec::new();
    if !not_opted_in.is_empty() {
        warnings.push(format!(
            "not shared (no `peer` opt-in): {}",
            not_opted_in.join(", ")
        ));
    }
    if !web_only.is_empty() {
        warnings.push(format!(
            "not shared (`web` is reserved until the gateway ships): {}",
            web_only.join(", ")
        ));
    }

    // Relays must be opted into explicitly — including public — so share traffic
    // is never routed over n0's public relays by accident.
    let relay_policy = config.sharing.and_then(|s| s.relays);
    let relay = RelayChoice::resolve(relay_policy.as_ref()).ok_or((
        StatusCode::BAD_REQUEST,
        format!(
            "run '{run_name}' cannot be shared: no relay is configured. Set `sharing.relays` \
             in veld.json to \"public\" or a list of self-hosted relay URLs — relays must be \
             opted into explicitly."
        ),
    ))?;

    let now = Utc::now().timestamp();
    let ttl = ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
    Ok(ResolvedShare {
        manifest: ShareManifest {
            run_id: run_state.run_id,
            run: run_name.clone(),
            project: run_state.project.clone(),
            nodes,
            created_at: now,
            expires_at: now + ttl,
        },
        relay,
        warnings,
    })
}

/// The share policy of a node's specific variant, if any.
fn variant_share<'a>(config: &'a VeldConfig, node: &str, variant: &str) -> Option<&'a SharePolicy> {
    config
        .nodes
        .get(node)
        .and_then(|n| n.variants.get(variant))
        .and_then(|v| v.share.as_ref())
}

/// Build the "nothing to share" error from the reasons URL-bearing nodes were
/// excluded. `not_opted_in` are `node:variant`s with no (peer) `share`;
/// `web_only` opted into `web` but not `peer`. Both are sorted+deduped in place
/// for a deterministic message.
fn share_exclusion_message(
    run_name: &str,
    had_url_bearing: bool,
    not_opted_in: &mut Vec<String>,
    web_only: &mut Vec<String>,
) -> String {
    if !had_url_bearing {
        return format!("run '{run_name}' has no shareable (URL-bearing) nodes");
    }
    not_opted_in.sort();
    not_opted_in.dedup();
    web_only.sort();
    web_only.dedup();

    let mut parts: Vec<String> = Vec::new();
    if !not_opted_in.is_empty() {
        parts.push(format!(
            "Add `\"share\": {{ \"expose\": [\"peer\"] }}` to the variant(s) you want to \
             share (candidates: {}).",
            not_opted_in.join(", ")
        ));
    }
    if !web_only.is_empty() {
        parts.push(format!(
            "These opt into `web` only, which is reserved until the public gateway ships — \
             add `peer` to share Veld-to-Veld today: {}.",
            web_only.join(", ")
        ));
    }
    if parts.is_empty() {
        // URL-bearing nodes existed but the --node filter excluded them all.
        return format!("run '{run_name}' has no shareable services matching the requested nodes");
    }
    format!(
        "run '{run_name}' has no services opted into peer sharing. {}",
        parts.join(" ")
    )
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
pub(crate) fn hostname_of(url: &str) -> String {
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
    use super::*;

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

    // matchit (axum's router) panics at build time on a route conflict. The
    // `{id}` (3-seg), `{id}/mode` (4-seg), and `by-run/{run_id}` (4-seg) routes
    // are distinct — this proves they coexist without shadowing.
    #[test]
    fn share_routes_build_without_conflict() {
        let mgr = Arc::new(ShareManager::new(iroh::SecretKey::generate()));
        let _ = routes(mgr);
    }

    fn config_with_variant(share_json: &str) -> VeldConfig {
        let json = format!(
            r#"{{
                "schemaVersion": "2",
                "name": "demo",
                "nodes": {{
                    "web": {{ "variants": {{
                        "local": {{ "type": "start_server", "command": "x"{share_json} }},
                        "prod":  {{ "type": "start_server", "command": "x" }}
                    }} }}
                }}
            }}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn variant_share_resolves_the_live_variant_only() {
        let cfg = config_with_variant(r#", "share": { "expose": ["peer"] }"#);
        // `local` opts in; `prod` (same node, no share) does not.
        assert!(variant_share(&cfg, "web", "local").is_some_and(|s| s.allows(ExposeMode::Peer)));
        assert!(variant_share(&cfg, "web", "prod").is_none());
        // Unknown node / variant → None, never a panic.
        assert!(variant_share(&cfg, "missing", "local").is_none());
        assert!(variant_share(&cfg, "web", "missing").is_none());
    }

    #[test]
    fn variant_share_web_only_does_not_allow_peer() {
        let cfg = config_with_variant(r#", "share": { "expose": ["web"] }"#);
        let s = variant_share(&cfg, "web", "local").unwrap();
        assert!(!s.allows(ExposeMode::Peer));
        assert!(s.allows(ExposeMode::Web));
    }

    #[test]
    fn exclusion_message_no_url_bearing() {
        let msg = share_exclusion_message("r", false, &mut vec![], &mut vec![]);
        assert!(msg.contains("no shareable (URL-bearing) nodes"), "{msg}");
    }

    #[test]
    fn exclusion_message_not_opted_in_lists_node_variant() {
        let msg = share_exclusion_message("r", true, &mut vec!["web:local".into()], &mut vec![]);
        assert!(msg.contains("no services opted into peer sharing"), "{msg}");
        assert!(msg.contains("web:local"), "{msg}");
        assert!(msg.contains("expose"), "{msg}");
    }

    #[test]
    fn exclusion_message_web_only_is_called_out_distinctly() {
        let msg = share_exclusion_message("r", true, &mut vec![], &mut vec!["api:local".into()]);
        assert!(
            msg.contains("reserved until the public gateway ships"),
            "{msg}"
        );
        assert!(msg.contains("api:local"), "{msg}");
    }

    #[test]
    fn exclusion_message_filtered_out_all() {
        // URL-bearing nodes existed but the --node filter excluded every one.
        let msg = share_exclusion_message("r", true, &mut vec![], &mut vec![]);
        assert!(msg.contains("matching the requested nodes"), "{msg}");
    }

    #[test]
    fn exclusion_message_is_deterministic() {
        let msg = share_exclusion_message(
            "r",
            true,
            &mut vec!["z:local".into(), "a:local".into(), "a:local".into()],
            &mut vec![],
        );
        // sorted + deduped
        assert!(msg.contains("a:local, z:local"), "{msg}");
    }
}
