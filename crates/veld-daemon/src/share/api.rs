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
    ExposeMode, GatewayRef, SharePolicy, VeldConfig, WebAccessMode, parse_config, resolve_proxy,
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
    let project = resolve_project(req.project_root.clone(), &headers);
    let ResolvedShare {
        manifest,
        relay,
        embed_relay_tokens,
        gateway,
        warnings,
        web_access,
    } = build_manifest(
        run.as_deref(),
        project.as_deref(),
        req.nodes.as_deref(),
        req.ttl_secs,
        mode,
    )?;
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
    // Trimmed: a run name is an identifier (`validate_run_name` bans whitespace
    // outright), so surrounding space is noise, never data.
    explicit.or_else(|| header_value(headers, "x-veld-run").map(|v| v.trim().to_owned()))
}

/// The project half of the address, resolved the same way: body first, then
/// the `X-Veld-Project` header Caddy injects alongside `X-Veld-Run`. Without
/// it the run name is matched against every project, which is ambiguous
/// whenever two projects run the same name — see [`StartShareRequest::project_root`].
fn resolve_project(explicit: Option<String>, headers: &HeaderMap) -> Option<String> {
    // NOT trimmed — unlike the run name, this is a filesystem path compared for
    // equality downstream, and `Path` normalizes trailing slashes but not
    // whitespace. Rewriting it would 404 with a message showing the requested
    // and actual roots as identical. Matches
    // `feedback_server::project_header`, and the body-supplied `project_root`,
    // which isn't trimmed either.
    explicit.or_else(|| header_value(headers, "x-veld-project"))
}

/// A non-empty header value, read as UTF-8.
///
/// Deliberately not `HeaderValue::to_str()`: that rejects every byte >= 0x80, and
/// `X-Veld-Project` carries a filesystem path, so a project under
/// `/Users/José/app` yields a valid header `to_str()` refuses to read — which
/// would silently drop the scope and fall back to machine-wide matching. Run
/// names are ASCII by `validate_run_name`, but they share this reader.
fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(name)?;
    let value = std::str::from_utf8(raw.as_bytes()).ok()?;
    // Trim to test emptiness, return verbatim. Callers decide whether their
    // header is an identifier (trim it) or a path (don't) — see `resolve_run`
    // and `resolve_project`.
    if value.trim().is_empty() {
        return None;
    }
    Some(value.to_owned())
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
    project: Option<&str>,
    nodes_filter: Option<&[String]>,
    ttl_secs: Option<i64>,
    mode: ExposeMode,
) -> Result<ResolvedShare, ApiError> {
    let db = veld_core::db::Db::open().map_err(internal)?;
    let registry = db.registry().map_err(internal)?;

    // Scope BOTH halves by the project, not just the lookup below: an unscoped
    // "only run" picks a name the caller never typed, and the lookup would then
    // reject it against the caller's own project — reporting a run they never
    // asked for as missing from a project they never mentioned it in.
    let run_name = match run {
        Some(r) => r.to_string(),
        None => sole_run(&registry, project)?,
    };

    let project_root = resolve_share_project(&registry, project, &run_name)?;

    let run_state = db
        .get_run(&project_root, &run_name)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, format!("run '{run_name}' not found")))?;

    // `Db::registry()` carries every environment's LATEST run whatever its
    // status, so everything above resolves stopped and crashed runs too — and
    // the node states keep their `url`/`port` as history, which is all the
    // manifest builder below reads. Sharing one would mint a tunnel, or a public
    // gateway URL, pointing at ports that are gone and that another process can
    // reuse. Gate here rather than filtering the resolution, so the error can
    // say *why* a run the user can see in `veld status` was refused.
    //
    // `Running`, not `is_live()`: that also admits `Starting`, and node URLs are
    // written per node as each one comes up — so a share taken mid-startup
    // exposes only the nodes up so far, silently and with no warning, or reports
    // the misleading "no shareable (URL-bearing) nodes" when none are. It admits
    // `Stopping` too, where minting a tunnel is pointless.
    if run_state.status != veld_core::state::RunStatus::Running {
        return Err((StatusCode::CONFLICT, {
            use veld_core::state::RunStatus;
            // The remedy has to match the status. "Start it" is wrong for a
            // run that is already coming up — and that case is reachable
            // from the overlay's Share button, because Caddy routes and the
            // injected overlay go live per node as each one comes up, while
            // `Running` is only set once every node is healthy. A multi-node
            // project therefore serves a shareable-looking page for the
            // whole startup window.
            //
            // Not `outcome_label()` for the transitional states: its
            // no-end_reason fallback returns "stopping" for ANY non-live
            // status, so a stopped run with a NULL end_reason would report
            // "(stopping)".
            let (state, remedy) = match run_state.status {
                RunStatus::Starting => ("still starting", "wait for it to finish starting"),
                RunStatus::Stopping => ("shutting down", "wait for it to stop"),
                _ => {
                    return Err((
                        StatusCode::CONFLICT,
                        format!(
                            "run '{run_name}' is not running ({}) — start it \
                             before sharing",
                            run_state.outcome_label()
                        ),
                    ));
                }
            };
            format!("run '{run_name}' is {state} — {remedy} before sharing")
        }));
    }
    let run_state = &run_state;

    let config = parse_config(&project_root.join("veld.json")).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("could not load veld.json for run '{run_name}': {e}"),
        )
    })?;

    // Reject invalid proxy headers before they travel to the gateway in the
    // manifest. Validation runs on the emitting path only — `parse_config` stays
    // lenient so unrelated commands aren't blocked; see `config::validate`.
    if let Some(msg) = veld_core::config::error_summary(&veld_core::config::validate(&config)) {
        return Err((StatusCode::BAD_REQUEST, msg));
    }

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
        if !share.as_ref().is_some_and(|s| s.allows(mode)) {
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
        let proxy = variant_proxy(&config, &ns.node_name, &ns.variant);
        nodes.push(SharedNode {
            node: ns.node_name.clone(),
            variant: ns.variant.clone(),
            hostname,
            url: url.clone(),
            upstream_port: port,
            proxy: (!proxy.is_empty()).then_some(proxy),
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
///
/// Resolved, not raw: `share` is hoistable to node level (F3). A raw read would
/// refuse to share a node whose opt-in is declared once for all its variants —
/// and, worse, the reverse mistake would be a silent consent bypass, so this must
/// go through the one resolver.
fn variant_share(config: &VeldConfig, node: &str, variant: &str) -> Option<SharePolicy> {
    config.resolved(node, variant).and_then(|r| r.share)
}

/// Resolved reverse-proxy header rules for a node's specific variant
/// (project → node → variant, most specific wins).
fn variant_proxy(
    config: &VeldConfig,
    node: &str,
    variant: &str,
) -> veld_core::config::ResolvedProxy {
    let node_cfg = config.nodes.get(node);
    let variant_cfg = node_cfg.and_then(|n| n.variants.get(variant));
    resolve_proxy(
        config.proxy.as_ref(),
        node_cfg.and_then(|n| n.proxy.as_ref()),
        variant_cfg.and_then(|v| v.proxy.as_ref()),
    )
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

/// Resolve which project's `run_name` is being shared.
///
/// A named `project` is **authoritative**: the run must be in it, or this fails.
///
/// The tempting alternative — treat the project as a hint and fall back to a
/// machine-wide match when it doesn't hold the name — was tried and reverted. It
/// meant `cd repoA && veld share main`, where A has no `main`, silently shared
/// *B's* `main`; with `--web` that publishes URLs while the output names neither
/// project nor run. The apparent capability it preserved (sharing any project's
/// run from any directory) was never a designed feature: it was an artifact of
/// the global first-match scan this module exists to remove. And it contradicted
/// [`sole_run`] directly, which refuses to cross the project boundary — leaving
/// the vaguer command stricter than the more specific one.
///
/// So: naming a project that doesn't run the name is an error that says where it
/// *does* run. `cd` is the remedy, and it's in the message. Callers with no
/// project at all (a `veld share` outside any project) pass `None` and get
/// machine-wide matching, where **more than one match is a hard error** rather
/// than a guess.
///
/// The returned root is always the registry's own `PathBuf`, never the caller's
/// string — the daemon reads `veld.json` from it.
fn resolve_share_project(
    registry: &GlobalRegistry,
    project: Option<&str>,
    run_name: &str,
) -> Result<std::path::PathBuf, ApiError> {
    let mut holders: Vec<&std::path::Path> = registry
        .projects
        .values()
        .filter(|e| e.runs.contains_key(run_name))
        .map(|e| e.project_root.as_path())
        .collect();
    // Deterministic order: `registry.projects` is a `HashMap`, and an error
    // message whose contents shuffle between runs is the same class of defect
    // as the resolution bug this module is fixing.
    holders.sort_unstable();
    let roots = || {
        holders
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };

    if let Some(project) = project {
        let requested = std::path::Path::new(project);
        if let Some(hit) = holders.iter().find(|p| **p == requested) {
            return Ok(hit.to_path_buf());
        }
        // No fallthrough — see the doc above. Say where it does run, so the
        // remedy (`cd`) is obvious and the answer never surprises.
        return Err((
            StatusCode::NOT_FOUND,
            if holders.is_empty() {
                format!("run '{run_name}' not found")
            } else {
                format!(
                    "run '{run_name}' is not in {project} — it runs in: {}",
                    roots()
                )
            },
        ));
    }

    match holders.as_slice() {
        [only] => Ok(only.to_path_buf()),
        [] => Err((StatusCode::NOT_FOUND, format!("run '{run_name}' not found"))),
        // Name the candidates and a remedy that exists: there is no
        // `--project-root` flag on `veld share`, so `cd` is the answer.
        _ => Err((
            StatusCode::CONFLICT,
            format!(
                "run '{run_name}' exists in more than one project ({}) — run \
                 `veld share` from the directory of the one you mean",
                roots()
            ),
        )),
    }
}

/// When no run is named, use the only *environment*; error if ambiguous.
///
/// Not "the only running one": `Db::registry()` keeps every environment's latest
/// run whatever its status, so what this resolves may well be stopped —
/// `build_manifest`'s liveness gate is what refuses it, with a message that says
/// so. Filtering here instead would report a stopped environment as absent.
///
/// Scoped to `project` when the caller supplied one, so "the only run" means
/// the only run *of this project*. Unscoped it stays machine-wide, which is
/// what a `veld share` invoked outside any project can offer.
fn sole_run(registry: &GlobalRegistry, project: Option<&str>) -> Result<String, ApiError> {
    let requested = project.map(std::path::Path::new);
    let scoped = || {
        registry
            .projects
            .values()
            .filter(move |e| requested.is_none_or(|p| e.project_root == p))
    };
    let mut names = scoped().flat_map(|e| e.runs.keys());
    match (names.next(), names.next()) {
        (Some(only), None) => Ok(only.clone()),
        (None, _) => Err((
            StatusCode::NOT_FOUND,
            match project {
                Some(p) => format!("no environments to share in {p}"),
                None => "no environments to share".to_string(),
            },
        )),
        (Some(_), Some(_)) => {
            // Name the candidates: with the project scope applied these are
            // this project's own environments, so the list is short and the
            // user can copy one straight into the command.
            // Deduplicated, and qualified by project when unscoped: across two
            // projects both running `main`, a bare list reads "dev, main, main"
            // and the remedy it suggests (`veld share main`) then 409s.
            let mut candidates: Vec<String> = scoped()
                .flat_map(|e| {
                    e.runs.keys().map(move |name| match project {
                        Some(_) => name.clone(),
                        None => format!("{name} in {}", e.project_root.display()),
                    })
                })
                .collect();
            candidates.sort_unstable();
            candidates.dedup();
            // "environments", not "active runs": the registry keeps stopped and
            // crashed ones, so some of these may not be running.
            Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "several environments here ({}) — name one: `veld share <run>`",
                    candidates.join(", ")
                ),
            ))
        }
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
    fn header_values_survive_non_ascii_project_paths() {
        // `HeaderValue::to_str()` rejects every byte >= 0x80, and this header
        // carries a filesystem path — so reading it that way silently dropped
        // the scope for any project under e.g. /Users/José. Caddy transmits the
        // raw UTF-8 bytes happily (Go only rejects CTLs), so the daemon must
        // parse bytes, not visible ASCII.
        let path = "/Users/José/项目/app";
        let mut h = HeaderMap::new();
        h.insert("x-veld-project", path.parse().unwrap());
        assert!(
            h.get("x-veld-project").unwrap().to_str().is_err(),
            "precondition: to_str must reject this, or the test proves nothing"
        );
        assert_eq!(resolve_project(None, &h), Some(path.to_owned()));
    }

    #[test]
    fn resolve_project_prefers_body_then_caddy_header() {
        let mut h = HeaderMap::new();
        h.insert("x-veld-project", " /repos/from-page ".parse().unwrap());
        assert_eq!(
            resolve_project(Some("/repos/from-cli".into()), &h),
            Some("/repos/from-cli".into())
        );
        // Verbatim, NOT trimmed: this is a path compared for equality, and
        // `Path` does not normalize whitespace. Contrast `resolve_run` above,
        // which does trim because a run name is an identifier.
        assert_eq!(resolve_project(None, &h), Some(" /repos/from-page ".into()));
        assert_eq!(resolve_project(None, &HeaderMap::new()), None);
    }

    fn run_info(name: &str) -> veld_core::state::RegistryRunInfo {
        veld_core::state::RegistryRunInfo {
            run_id: Uuid::new_v4(),
            name: name.to_owned(),
            status: veld_core::state::RunStatus::Running,
            urls: std::collections::HashMap::new(),
        }
    }

    /// Two projects each running an environment called `main`.
    fn colliding_registry() -> GlobalRegistry {
        let entry = |root: &str, name: &str| {
            let mut runs = std::collections::HashMap::new();
            runs.insert("main".to_owned(), run_info("main"));
            (
                root.to_owned(),
                veld_core::state::RegistryEntry {
                    project_root: std::path::PathBuf::from(root),
                    project_name: name.to_owned(),
                    runs,
                },
            )
        };
        GlobalRegistry {
            projects: std::collections::HashMap::from([
                entry("/repos/alpha", "alpha"),
                entry("/repos/beta", "beta"),
            ]),
        }
    }

    #[test]
    fn a_named_project_is_authoritative() {
        // A scope that holds the name wins outright — the actual fix.
        let reg = colliding_registry();
        assert_eq!(
            resolve_share_project(&reg, Some("/repos/beta"), "main").unwrap(),
            std::path::PathBuf::from("/repos/beta")
        );
        assert_eq!(
            resolve_share_project(&reg, Some("/repos/alpha"), "main").unwrap(),
            std::path::PathBuf::from("/repos/alpha")
        );

        // A scope that does NOT hold the name is an error, even when the name is
        // unambiguous elsewhere. Falling through was tried and reverted: it made
        // `cd repoA && veld share main` publish repoB's run, and it left this
        // path laxer than `sole_run`, which never crosses the boundary.
        let mut solo = colliding_registry();
        solo.projects.remove("/repos/beta");
        let (code, msg) = resolve_share_project(&solo, Some("/repos/gamma"), "main").unwrap_err();
        assert_eq!(code, StatusCode::NOT_FOUND);
        // …but it must say where the run does live, so `cd` is the obvious fix.
        assert!(
            msg.contains("/repos/gamma") && msg.contains("/repos/alpha"),
            "{msg}"
        );

        // A name nothing runs is a plain "not found" — nowhere to point at.
        let (code, msg) = resolve_share_project(&reg, Some("/repos/alpha"), "nope").unwrap_err();
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert!(!msg.contains("it runs in"), "{msg}");
    }

    #[test]
    fn sole_run_is_scoped_to_the_project_when_one_is_given() {
        let mut reg = colliding_registry();
        // Give beta a second environment so "sole run" is ambiguous globally
        // but unambiguous within alpha.
        reg.projects
            .get_mut("/repos/beta")
            .unwrap()
            .runs
            .insert("extra".to_owned(), run_info("extra"));

        // Scoped: alpha's only run, even though the machine has three.
        assert_eq!(sole_run(&reg, Some("/repos/alpha")).unwrap(), "main");

        // Unscoped, this would pick some other project's name and the lookup
        // would then reject it against alpha. Scoped, an empty project says so.
        let (code, msg) = sole_run(&reg, Some("/repos/gamma")).unwrap_err();
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert!(msg.contains("/repos/gamma"), "{msg}");

        // Unscoped stays machine-wide, and names the candidates.
        let (code, msg) = sole_run(&reg, None).unwrap_err();
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(msg.contains("extra") && msg.contains("main"), "{msg}");
    }

    #[test]
    fn share_without_a_project_rejects_an_ambiguous_name() {
        let reg = colliding_registry();
        let (code, msg) = resolve_share_project(&reg, None, "main").unwrap_err();
        assert_eq!(code, StatusCode::CONFLICT);
        assert!(
            msg.contains("/repos/alpha") && msg.contains("/repos/beta"),
            "{msg}"
        );
        // The remedy named must exist: `veld share` has no project flag.
        assert!(!msg.contains("--project"), "{msg}");

        // Unambiguous names still resolve without a project — an older CLI, or
        // `veld share` run from outside a project, keeps working.
        let (code, _) = resolve_share_project(&reg, None, "nope").unwrap_err();
        assert_eq!(code, StatusCode::NOT_FOUND);
        let mut solo = colliding_registry();
        solo.projects.remove("/repos/beta");
        assert_eq!(
            resolve_share_project(&solo, None, "main").unwrap(),
            std::path::PathBuf::from("/repos/alpha")
        );
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
    fn variant_proxy_resolves_and_carries_into_shared_node() {
        // Project-level response rule + variant-level request rule; variant wins
        // where they overlap. Mirrors what build_manifest feeds into SharedNode.
        let json = r#"{
            "schemaVersion": "2",
            "name": "demo",
            "proxy": { "response": { "set": { "X-From": "project" } } },
            "nodes": {
                "web": { "variants": {
                    "local": {
                        "type": "start_server", "command": "x",
                        "proxy": { "request": { "remove": ["Origin"] } }
                    },
                    "bare": { "type": "start_server", "command": "x" }
                } }
            }
        }"#;
        let cfg: VeldConfig = serde_json::from_str(json).unwrap();

        let resolved = variant_proxy(&cfg, "web", "local");
        assert_eq!(resolved.request.remove, vec!["Origin"]);
        assert_eq!(resolved.response.set.get("X-From").unwrap(), "project");
        // This is exactly the elision build_manifest applies before SharedNode.
        assert!(!resolved.is_empty());

        // A variant with no own proxy still inherits the project response rule.
        let bare = variant_proxy(&cfg, "web", "bare");
        assert!(bare.request.is_empty());
        assert_eq!(bare.response.set.get("X-From").unwrap(), "project");

        // A config with no proxy anywhere resolves empty → elided to None.
        let plain = config_with_variant("");
        assert!(variant_proxy(&plain, "web", "local").is_empty());
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
