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
use veld_core::config::{
    ExposeMode, GatewayRef, SharePolicy, VeldConfig, WebAccessMode, load_config,
};
use veld_core::share::{
    ApprovalMode, Capability, GatewayAccessPolicy, JoinRequest, JoinResponse, ShareManifest,
    SharedNode, SharesList, StartShareRequest, StartShareResponse,
};
use veld_core::state::GlobalRegistry;

use super::endpoint::RelayChoice;
use super::gateway::GatewayClient;
use super::manager::ShareManager;

const DEFAULT_TTL_SECS: i64 = 2 * 60 * 60;
/// Web shares default to a shorter life than peer shares (§6.1): the audience
/// is the open internet, so an idle share should die sooner.
const WEB_DEFAULT_TTL_SECS: i64 = 60 * 60;

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

    let mode = if req.web {
        ExposeMode::Web
    } else {
        ExposeMode::Peer
    };
    let run = resolve_run(req.run.clone(), &headers);
    let ResolvedShare {
        manifest,
        relay,
        embed_relay_tokens,
        gateway,
        warnings,
        web_access,
    } = build_manifest(run.as_deref(), req.nodes.as_deref(), req.ttl_secs, mode)?;
    let node_names: Vec<String> = manifest.nodes.iter().map(|n| n.node.clone()).collect();
    let expires_at = manifest.expires_at;

    if req.web {
        return start_web_share(
            &manager, req, manifest, relay, gateway, warnings, web_access,
        )
        .await;
    }

    let capability = Capability::generate();
    let (share_id, ticket) = manager
        .start_share(
            manifest,
            capability,
            req.approve.unwrap_or_default(),
            relay,
            embed_relay_tokens,
        )
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
        public_urls: Vec::new(),
        web_password: None,
    }))
}

/// The run a share request targets. An explicit run in the body always wins;
/// without one, fall back to the `X-Veld-Run` header that Caddy injects on
/// `/__veld__/`-proxied requests — the browser overlay shares the run its
/// page belongs to without knowing the run's name, even with several runs
/// active. Direct callers (the CLI) carry no such header and keep the
/// "only run" resolution downstream.
fn resolve_run(explicit: Option<String>, headers: &HeaderMap) -> Option<String> {
    explicit.or_else(|| {
        headers
            .get("x-veld-run")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    })
}

/// The web path of `start`: mint a share scoped to the `web`-opted nodes,
/// hand its ticket to the configured gateway (the ticket is never surfaced to
/// a human — the capability stays between this daemon and the gateway), and
/// keep the registration alive via heartbeats until unshare.
async fn start_web_share(
    manager: &Arc<ShareManager>,
    req: StartShareRequest,
    manifest: ShareManifest,
    relay: RelayChoice,
    gateway: Option<GatewayRef>,
    mut warnings: Vec<String>,
    web_access: Vec<(String, Option<WebAccessMode>)>,
) -> Result<Json<StartShareResponse>, ApiError> {
    // Resolve the gateway BEFORE minting the share, so a missing gateway
    // config fails cleanly with nothing to tear down.
    let client = GatewayClient::resolve(gateway.as_ref())
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    // Viewer access policy (§6.1): explicit config wins; the CLI flag covers
    // config-silent nodes; the default is password — never an open URL
    // without someone having said "link" somewhere.
    let access = resolve_web_access(&web_access, req.web_access, req.web_password.as_deref())
        .map_err(|msg| (StatusCode::BAD_REQUEST, msg))?;
    if req.web_password.is_some() && access.password.is_none() {
        warnings.push(
            "password ignored: every shared node is link-access (config `share.web.access` \
             or --access link)"
                .to_string(),
        );
    }

    let node_names: Vec<String> = manifest.nodes.iter().map(|n| n.node.clone()).collect();
    let expires_at = manifest.expires_at;
    let run_id = manifest.run_id;

    // Re-running `veld share --web` for the same run replaces the previous web
    // share rather than stacking a second one. Snapshot the prior web shares
    // now but DON'T tear them down yet: the new share has a fresh capability
    // (hence fresh slugs), so both can coexist momentarily — and if the new
    // registration fails (gateway unreachable / rollback), the old share must
    // survive rather than being destroyed by a re-share that never completed.
    let prior_web = manager.web_share_ids_for_run(run_id).await;

    // The gateway is the sole intended joiner and the user just asked for
    // this exposure, so `auto` is the default; an explicit --approve still
    // wins (e.g. `manual` to eyeball the gateway's join in the dashboard).
    // Relay tokens are never embedded in a web ticket: the gateway
    // authenticates to relays from its *own* config, and the ticket should
    // carry no secrets beyond the capability.
    let capability = Capability::generate();
    let (share_id, ticket) = manager
        .start_share(
            manifest,
            capability,
            req.approve.unwrap_or(ApprovalMode::Auto),
            relay,
            false,
        )
        .await
        .map_err(internal)?;
    let token = ticket.encode().map_err(internal)?;

    let registration = match client.register(&token, Some(&access)).await {
        Ok(r) => r,
        Err(e) => {
            // No orphaned share: if the gateway won't take it, unshare the
            // new one. The prior share is untouched and still live.
            let _ = manager.unshare(&share_id).await;
            return Err((StatusCode::BAD_GATEWAY, format!("{e:#}")));
        }
    };

    // Version-skew guard (§6.1): a gateway that predates the access layer
    // ignores the policy and omits the ack — it would serve a share the user
    // asked to protect wide open. Tear the new one down; keep the prior one.
    if let Err(msg) = verify_access_ack(&access, registration.access.as_ref()) {
        let _ = client.unregister(&registration.id).await;
        let _ = manager.unshare(&share_id).await;
        return Err((StatusCode::BAD_GATEWAY, msg));
    }

    if let Err(e) = manager
        .attach_web_registration(
            &share_id,
            client.clone(),
            registration.id.clone(),
            registration.lease_secs,
            registration.urls.clone(),
            Some(access.clone()),
        )
        .await
    {
        // The share vanished mid-flight; withdraw the gateway registration.
        let _ = client.unregister(&registration.id).await;
        return Err(internal(e));
    }

    // The new share is fully live — NOW retire the ones it replaces. Their
    // fresh-capability successor means the slugs, public URLs, and password
    // all rotated; anything already handed out just died, so say so.
    if !prior_web.is_empty() && manager.unshare_ids(&prior_web).await > 0 {
        warnings.push(
            "replaced the previous web share for this run — its public URLs, one-links, and \
             password are now invalid; send the new ones"
                .to_string(),
        );
    }

    Ok(Json(StartShareResponse {
        share_id,
        // The web ticket is a secret between daemon and gateway — not returned.
        ticket: String::new(),
        join_url: String::new(),
        nodes: node_names,
        expires_at,
        warnings,
        public_urls: registration.urls,
        web_password: access.password,
    }))
}

/// Build the §6.1 access policy for a web share. `explicit` carries each
/// hostname's configured `share.web.access` (`None` = config silent);
/// `cli_default` (the `--access` flag) applies only to the silent ones; the
/// final fallback is password. Generates the share password when any node
/// needs one and the caller didn't supply a valid one.
fn resolve_web_access(
    explicit: &[(String, Option<WebAccessMode>)],
    cli_default: Option<WebAccessMode>,
    custom_password: Option<&str>,
) -> Result<GatewayAccessPolicy, String> {
    let silent_default = cli_default.unwrap_or(WebAccessMode::Password);
    let mut nodes = std::collections::BTreeMap::new();
    let mut needs_password = false;
    for (hostname, configured) in explicit {
        let mode = configured.unwrap_or(silent_default);
        needs_password |= mode == WebAccessMode::Password;
        // The wire policy is keyed by hostname; two nodes CAN share one (same
        // host, different ports — already ambiguous at the tunnel level,
        // which also routes by hostname). Strictest wins so a duplicate can
        // never silently downgrade password → link, and the outcome doesn't
        // depend on map iteration order.
        nodes
            .entry(hostname.clone())
            .and_modify(|existing| {
                if mode == WebAccessMode::Password {
                    *existing = WebAccessMode::Password;
                }
            })
            .or_insert(mode);
    }

    let password = if needs_password {
        Some(match custom_password {
            Some(p) => {
                let p = p.trim();
                let chars = p.chars().count();
                if chars == 0 {
                    return Err("--password must not be empty".to_string());
                }
                if chars < 8 {
                    return Err(
                        "--password must be at least 8 characters (or omit it for a strong \
                         generated one)"
                            .to_string(),
                    );
                }
                if chars > 128 {
                    return Err("--password must be at most 128 characters".to_string());
                }
                p.to_owned()
            }
            None => generate_password(),
        })
    } else {
        None
    };

    Ok(GatewayAccessPolicy { password, nodes })
}

/// Enforce the §6.1 skew guard: the gateway must ack exactly the policy we
/// asked for. Exception: an all-link policy against an ack-less (old) gateway
/// is allowed — link-access is precisely what an old gateway enforces.
///
/// `pub(crate)`: also re-checked on every heartbeat (`manager.rs`) — a
/// gateway ROLLBACK mid-share would otherwise re-register the same slugs
/// unprotected without the daemon ever noticing.
pub(crate) fn verify_access_ack(
    sent: &GatewayAccessPolicy,
    ack: Option<&veld_core::share::GatewayAccessAck>,
) -> Result<(), String> {
    let all_link = sent.nodes.values().all(|m| *m == WebAccessMode::Link);
    match ack {
        None if all_link && sent.password.is_none() => Ok(()),
        None => Err(
            "the gateway is too old to enforce viewer access control and would serve this \
             share without the password. Upgrade veld-gateway, or share link-only with \
             `--access link`."
                .to_string(),
        ),
        Some(ack) => {
            if ack.password_protected != sent.password.is_some() || ack.nodes != sent.nodes {
                return Err(format!(
                    "the gateway did not apply the requested access policy (asked \
                     password_protected={}, got {}). Not exposing the share.",
                    sent.password.is_some(),
                    ack.password_protected
                ));
            }
            Ok(())
        }
    }
}

/// Generate the share password: three dash-joined groups of four characters
/// from an unambiguous lowercase alphabet (no i/l/o/0/1) — ~59 bits, easy to
/// read out, type, copy and paste. Entropy comes from v4 UUIDs (the same
/// source capabilities use), mapped by rejection sampling (no modulo bias).
fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789"; // 31 chars
    const LEN: usize = 12;
    let mut chars = Vec::with_capacity(LEN);
    while chars.len() < LEN {
        for b in Uuid::new_v4().as_bytes() {
            if chars.len() == LEN {
                break;
            }
            // Rejection sampling: accept only bytes below the largest
            // multiple of 31 (248), so each symbol is uniform.
            if *b < 248 {
                chars.push(ALPHABET[(b % 31) as usize] as char);
            }
        }
    }
    let s: String = chars.into_iter().collect();
    format!("{}-{}-{}", &s[0..4], &s[4..8], &s[8..12])
}

async fn join(
    State(manager): State<Arc<ShareManager>>,
    headers: HeaderMap,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, ApiError> {
    check_csrf(&headers)?;
    let label = req.label.unwrap_or_default();
    let resp = manager
        .join(&req.ticket, &label, &req.relay_tokens, req.remember)
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
    /// DANGER opt-in (`sharing.dangerouslyEmbedRelayTokensInTicket`): embed the
    /// resolved relay token(s) in the ticket so joiners need no out-of-band
    /// config. Ships the relay secret inside the shareable link.
    embed_relay_tokens: bool,
    /// The web gateway declared in config (`sharing.gateway`), if any — used
    /// by the web share path.
    gateway: Option<GatewayRef>,
    /// URL-bearing services excluded from the share (not opted into the
    /// requested mode), surfaced as warnings so a partial share isn't silently
    /// under-exposed.
    warnings: Vec<String>,
    /// Web shares only: each shared hostname's **explicitly configured**
    /// access mode (`share.web.access`), `None` where the config is silent —
    /// the CLI flag / password default applies only to the silent ones
    /// (config is the compliance surface, §6.1).
    web_access: Vec<(String, Option<WebAccessMode>)>,
}

/// Resolve a run to a shareable manifest by reading persisted state and the
/// project's config. Only services whose active variant opts into the
/// requested `mode` (`share.expose` contains it) are included; this is the
/// explicit consent gate. The runtime `--node` filter narrows *within* the
/// opted-in set — it can never widen it.
fn build_manifest(
    run: Option<&str>,
    nodes_filter: Option<&[String]>,
    ttl_secs: Option<i64>,
    mode: ExposeMode,
) -> Result<ResolvedShare, ApiError> {
    let db = veld_core::db::Db::open().map_err(internal)?;
    let registry = db.registry().map_err(internal)?;

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

    let run_state = db
        .get_run(&project_root, &run_name)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, format!("run '{run_name}' not found")))?;
    let run_state = &run_state;

    let config = load_config(&project_root.join("veld.json")).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("could not load veld.json for run '{run_name}': {e}"),
        )
    })?;

    // Track why URL-bearing nodes were excluded, so the error can point the user
    // at the opt-in they are missing rather than a bare "nothing to share".
    // `node:variant` labels because the opt-in check uses the *live* variant, and
    // a multi-variant node needs the opt-in on the running one specifically.
    let other_mode = match mode {
        ExposeMode::Peer => ExposeMode::Web,
        ExposeMode::Web => ExposeMode::Peer,
    };
    let mut had_url_bearing = false;
    let mut not_opted_in: Vec<String> = Vec::new();
    let mut other_only: Vec<String> = Vec::new();
    let mut nodes = Vec::new();
    let mut web_access: Vec<(String, Option<WebAccessMode>)> = Vec::new();
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
        if !share.is_some_and(|s| s.allows(mode)) {
            let label = format!("{}:{}", ns.node_name, ns.variant);
            // An other-audience-only opt-in is a deliberate choice, not a
            // missing one — call it out distinctly.
            if share.is_some_and(|s| s.allows(other_mode)) {
                other_only.push(label);
            } else {
                not_opted_in.push(label);
            }
            continue;
        }
        let hostname = hostname_of(url);
        if mode == ExposeMode::Web {
            web_access.push((hostname.clone(), share.and_then(|s| s.web_access())));
        }
        nodes.push(SharedNode {
            node: ns.node_name.clone(),
            variant: ns.variant.clone(),
            hostname,
            url: url.clone(),
            upstream_port: port,
        });
    }

    if nodes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            share_exclusion_message(
                &run_name,
                had_url_bearing,
                &mut not_opted_in,
                &mut other_only,
                mode,
            ),
        ));
    }

    // Partial share: some URL-bearing services were excluded. Warn rather than
    // silently under-expose (the excluded set is otherwise invisible to the user).
    not_opted_in.sort();
    not_opted_in.dedup();
    other_only.sort();
    other_only.dedup();
    let mut warnings = Vec::new();
    if !not_opted_in.is_empty() {
        warnings.push(format!(
            "not shared (no `{}` opt-in): {}",
            mode_name(mode),
            not_opted_in.join(", ")
        ));
    }
    if !other_only.is_empty() {
        warnings.push(match mode {
            ExposeMode::Peer => format!(
                "not shared here (opted into `web` only — use `veld share --web`): {}",
                other_only.join(", ")
            ),
            ExposeMode::Web => format!(
                "not shared (opted into `peer` only — add \"web\" to their `share.expose` \
                 to expose them publicly): {}",
                other_only.join(", ")
            ),
        });
    }

    // Relays must be opted into explicitly — including public — so share traffic
    // is never routed over n0's public relays by accident.
    let sharing = config.sharing;
    let embed_relay_tokens = sharing
        .as_ref()
        .map(|s| s.dangerously_embed_relay_tokens_in_ticket)
        .unwrap_or(false);
    let gateway = sharing.as_ref().and_then(|s| s.gateway.clone());
    let relay_policy = sharing.and_then(|s| s.relays);
    let relay = RelayChoice::resolve(relay_policy.as_ref()).ok_or((
        StatusCode::BAD_REQUEST,
        format!(
            "run '{run_name}' cannot be shared: no relay is configured. Set `sharing.relays` \
             in veld.json to \"public\" or a list of self-hosted relay URLs — relays must be \
             opted into explicitly."
        ),
    ))?;

    // Loud warning when a relay secret is about to ride inside the join link, so
    // `veld share` / the dashboard surface it (the link is auto-copied) rather
    // than silently shipping the secret.
    if let Some(w) = embed_warning(embed_relay_tokens, &relay) {
        warnings.push(w);
    }

    let now = Utc::now().timestamp();
    let ttl = ttl_secs.unwrap_or(match mode {
        ExposeMode::Peer => DEFAULT_TTL_SECS,
        ExposeMode::Web => WEB_DEFAULT_TTL_SECS,
    });
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
        embed_relay_tokens,
        gateway,
        warnings,
        web_access,
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

/// The DANGER warning to surface when a share is about to embed relay token(s)
/// in the ticket, or `None`. Fires iff the `dangerouslyEmbedRelayTokensInTicket`
/// opt-in is on AND a custom relay actually carries a token to embed — so it
/// stays silent for a public/token-less relay (no spurious scary warning).
fn embed_warning(embed_relay_tokens: bool, relay: &RelayChoice) -> Option<String> {
    let embeds = embed_relay_tokens
        && matches!(relay, RelayChoice::Custom(entries) if entries.iter().any(|e| e.token.is_some()));
    embeds.then(|| {
        "dangerouslyEmbedRelayTokensInTicket is on: the relay auth token is embedded in the \
         join link — treat the link as a secret (anyone with it can use your relay)."
            .to_string()
    })
}

/// Build the "nothing to share" error from the reasons URL-bearing nodes were
/// excluded. `not_opted_in` are `node:variant`s with no `share` opting into the
/// requested `mode`; `other_only` opted into the *other* audience only. Both are
/// sorted+deduped in place for a deterministic message.
fn share_exclusion_message(
    run_name: &str,
    had_url_bearing: bool,
    not_opted_in: &mut Vec<String>,
    other_only: &mut Vec<String>,
    mode: ExposeMode,
) -> String {
    if !had_url_bearing {
        return format!("run '{run_name}' has no shareable (URL-bearing) nodes");
    }
    not_opted_in.sort();
    not_opted_in.dedup();
    other_only.sort();
    other_only.dedup();

    let mode_str = mode_name(mode);
    let mut parts: Vec<String> = Vec::new();
    if !not_opted_in.is_empty() {
        parts.push(format!(
            "Add `\"share\": {{ \"expose\": [\"{mode_str}\"] }}` to the variant(s) you want to \
             share (candidates: {}).",
            not_opted_in.join(", ")
        ));
    }
    if !other_only.is_empty() {
        parts.push(match mode {
            ExposeMode::Peer => format!(
                "These opt into `web` only — use `veld share --web`, or add `peer` to share \
                 Veld-to-Veld: {}.",
                other_only.join(", ")
            ),
            ExposeMode::Web => format!(
                "These opt into `peer` only — add `web` to their `share.expose` to expose \
                 them publicly: {}.",
                other_only.join(", ")
            ),
        });
    }
    if parts.is_empty() {
        // URL-bearing nodes existed but the --node filter excluded them all.
        return format!("run '{run_name}' has no shareable services matching the requested nodes");
    }
    format!(
        "run '{run_name}' has no services opted into {mode_str} sharing. {}",
        parts.join(" ")
    )
}

/// The config-facing name of an expose mode (matches the JSON values).
fn mode_name(mode: ExposeMode) -> &'static str {
    match mode {
        ExposeMode::Peer => "peer",
        ExposeMode::Web => "web",
    }
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
    use veld_core::config::{RelayEntry, SecretSource};

    #[test]
    fn resolve_run_prefers_body_then_caddy_header() {
        let hdr = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert("x-veld-run", v.parse().unwrap());
            h
        };
        // Explicit run always wins, even with a header present.
        assert_eq!(
            resolve_run(Some("cli-run".into()), &hdr("page-run")),
            Some("cli-run".into())
        );
        // No explicit run → the Caddy-injected header (trimmed).
        assert_eq!(
            resolve_run(None, &hdr("  page-run ")),
            Some("page-run".into())
        );
        // Empty/whitespace header is ignored (falls through to "only run").
        assert_eq!(resolve_run(None, &hdr("   ")), None);
        assert_eq!(resolve_run(None, &HeaderMap::new()), None);
    }

    #[test]
    fn embed_warning_only_when_embedding_a_real_token() {
        let gated = RelayChoice::Custom(vec![RelayEntry {
            url: "https://relay.example".into(),
            token: Some(SecretSource::Literal("s3cret".into())),
        }]);
        let open = RelayChoice::Custom(vec![RelayEntry::url("https://relay.example")]);

        // On + a relay with a token → warns.
        assert!(embed_warning(true, &gated).is_some());
        // On but no token to embed → silent (no spurious scary warning).
        assert!(embed_warning(true, &open).is_none());
        assert!(embed_warning(true, &RelayChoice::Public).is_none());
        // Off → silent regardless.
        assert!(embed_warning(false, &gated).is_none());
    }

    #[test]
    fn resolve_web_access_config_wins_cli_covers_silence() {
        let explicit = vec![
            ("app.x".to_string(), None),                            // silent
            ("api.x".to_string(), Some(WebAccessMode::Link)),       // explicit link
            ("admin.x".to_string(), Some(WebAccessMode::Password)), // explicit password
        ];

        // No CLI flag: silent → password; a password is minted.
        let p = resolve_web_access(&explicit, None, None).unwrap();
        assert_eq!(p.nodes["app.x"], WebAccessMode::Password);
        assert_eq!(p.nodes["api.x"], WebAccessMode::Link);
        assert_eq!(p.nodes["admin.x"], WebAccessMode::Password);
        assert!(p.password.is_some());

        // `--access link` weakens ONLY the silent node; explicit password
        // config still forces a password.
        let p = resolve_web_access(&explicit, Some(WebAccessMode::Link), None).unwrap();
        assert_eq!(p.nodes["app.x"], WebAccessMode::Link);
        assert_eq!(p.nodes["admin.x"], WebAccessMode::Password);
        assert!(
            p.password.is_some(),
            "explicit password node still needs one"
        );

        // All link (explicit + CLI) → no password minted.
        let all_link = vec![
            ("app.x".to_string(), None),
            ("api.x".to_string(), Some(WebAccessMode::Link)),
        ];
        let p = resolve_web_access(&all_link, Some(WebAccessMode::Link), None).unwrap();
        assert!(p.password.is_none());

        // A custom password is used verbatim (trimmed); empty is refused.
        let p = resolve_web_access(&explicit, None, Some("  hunter2secret  ")).unwrap();
        assert_eq!(p.password.as_deref(), Some("hunter2secret"));
        assert!(resolve_web_access(&explicit, None, Some("   ")).is_err());
        assert!(resolve_web_access(&explicit, None, Some(&"x".repeat(200))).is_err());
    }

    #[test]
    fn resolve_web_access_duplicate_hostnames_take_the_strictest_mode() {
        // Two nodes can legally share one hostname (same host, different
        // ports). The wire policy is hostname-keyed, so the pair collapses to
        // one entry — which must never downgrade to link by iteration order.
        for order in [
            vec![
                ("app.x".to_string(), Some(WebAccessMode::Password)),
                ("app.x".to_string(), Some(WebAccessMode::Link)),
            ],
            vec![
                ("app.x".to_string(), Some(WebAccessMode::Link)),
                ("app.x".to_string(), Some(WebAccessMode::Password)),
            ],
        ] {
            let p = resolve_web_access(&order, None, None).unwrap();
            assert_eq!(p.nodes["app.x"], WebAccessMode::Password);
            assert!(p.password.is_some());
        }
    }

    #[test]
    fn generated_passwords_are_well_formed_and_distinct() {
        let a = generate_password();
        let b = generate_password();
        assert_ne!(a, b);
        for pw in [&a, &b] {
            let groups: Vec<&str> = pw.split('-').collect();
            assert_eq!(groups.len(), 3, "{pw}");
            for g in groups {
                assert_eq!(g.len(), 4, "{pw}");
                assert!(
                    g.bytes()
                        .all(|c| b"abcdefghjkmnpqrstuvwxyz23456789".contains(&c)),
                    "{pw}"
                );
            }
        }
    }

    #[test]
    fn access_ack_guard_blocks_old_gateways_for_protected_shares() {
        use std::collections::BTreeMap;
        use veld_core::share::GatewayAccessAck;

        let mut nodes = BTreeMap::new();
        nodes.insert("app.x".to_string(), WebAccessMode::Password);
        let protected = GatewayAccessPolicy {
            password: Some("pw".into()),
            nodes: nodes.clone(),
        };

        // Old gateway (no ack) + protected share → refuse.
        assert!(verify_access_ack(&protected, None).is_err());
        // Matching ack → ok.
        let ack = GatewayAccessAck {
            password_protected: true,
            nodes: nodes.clone(),
        };
        assert!(verify_access_ack(&protected, Some(&ack)).is_ok());
        // Ack claiming no protection → refuse.
        let bad = GatewayAccessAck {
            password_protected: false,
            nodes,
        };
        assert!(verify_access_ack(&protected, Some(&bad)).is_err());

        // All-link policy against an old gateway is fine — link-access is
        // exactly what an old gateway enforces.
        let mut link_nodes = BTreeMap::new();
        link_nodes.insert("app.x".to_string(), WebAccessMode::Link);
        let open = GatewayAccessPolicy {
            password: None,
            nodes: link_nodes,
        };
        assert!(verify_access_ack(&open, None).is_ok());
    }

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
        let msg = share_exclusion_message("r", false, &mut vec![], &mut vec![], ExposeMode::Peer);
        assert!(msg.contains("no shareable (URL-bearing) nodes"), "{msg}");
    }

    #[test]
    fn exclusion_message_not_opted_in_lists_node_variant() {
        let msg = share_exclusion_message(
            "r",
            true,
            &mut vec!["web:local".into()],
            &mut vec![],
            ExposeMode::Peer,
        );
        assert!(msg.contains("no services opted into peer sharing"), "{msg}");
        assert!(msg.contains("web:local"), "{msg}");
        assert!(msg.contains("expose"), "{msg}");
    }

    #[test]
    fn exclusion_message_other_audience_is_called_out_distinctly() {
        // Peer share, web-only nodes → point at `veld share --web`.
        let msg = share_exclusion_message(
            "r",
            true,
            &mut vec![],
            &mut vec!["api:local".into()],
            ExposeMode::Peer,
        );
        assert!(msg.contains("veld share --web"), "{msg}");
        assert!(msg.contains("api:local"), "{msg}");

        // Web share, peer-only nodes → point at adding `web` to expose.
        let msg = share_exclusion_message(
            "r",
            true,
            &mut vec![],
            &mut vec!["api:local".into()],
            ExposeMode::Web,
        );
        assert!(msg.contains("opt into `peer` only"), "{msg}");
        assert!(msg.contains("no services opted into web sharing"), "{msg}");
    }

    #[test]
    fn exclusion_message_web_mode_names_the_web_opt_in() {
        let msg = share_exclusion_message(
            "r",
            true,
            &mut vec!["web:local".into()],
            &mut vec![],
            ExposeMode::Web,
        );
        assert!(msg.contains(r#""expose": ["web"]"#), "{msg}");
    }

    #[test]
    fn exclusion_message_filtered_out_all() {
        // URL-bearing nodes existed but the --node filter excluded every one.
        let msg = share_exclusion_message("r", true, &mut vec![], &mut vec![], ExposeMode::Peer);
        assert!(msg.contains("matching the requested nodes"), "{msg}");
    }

    #[test]
    fn exclusion_message_is_deterministic() {
        let msg = share_exclusion_message(
            "r",
            true,
            &mut vec!["z:local".into(), "a:local".into(), "a:local".into()],
            &mut vec![],
            ExposeMode::Peer,
        );
        // sorted + deduped
        assert!(msg.contains("a:local, z:local"), "{msg}");
    }
}
