//! `POST /api/promotions/state` and `/mark` — what this user has seen.
//!
//! **The daemon never looks inside a promotion, and does not know what one is.**
//! It stores a map of opaque ids to `dismissed`/`read`, plus the instant this
//! user first opened the IDE, and hands both to whoever asks. Every headline,
//! sentence, glyph, date and kind lives in the `/ide` bundle, and so does the
//! decision about what any of it means.
//!
//! That is the same split `pane_layouts` takes, and it buys the same two things:
//! adding a promotion is a UI-only change with no daemon release behind it, and
//! an older daemon serving a newer bundle keeps working because it never had an
//! opinion about the payload. It is also why the date gate is computed client-
//! side from `first_use` rather than here — a daemon that filtered by date would
//! have to know that promotions have dates.
//!
//! Both routes are `POST`. `state` reads, but it also *stamps* `first_use` on
//! first contact, and a safe method with that side effect is reachable from any
//! page the user visits via a bare `<img src=…>`.
//!
//! Kept out of `desktop::routes()` for the reason `settings` is: promotions are
//! not desktop-specific, and that router's blanket `csrf_layer` only covers
//! routes registered before the `.layer` call, so a route appended after it
//! would be silently unprotected. `check_csrf` is called at the top of each
//! handler instead, where a reviewer reading the handler can see it.

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::Deserialize;
use tracing::warn;
use veld_core::db::PromotionState;

use super::management::{check_csrf, open_db};

/// Ceiling on how many ids **one request** may carry.
///
/// Not a security boundary — the endpoint is same-origin and CSRF-checked — and
/// note what it does *not* bound: the stored map is one JSON row that grows
/// monotonically across requests, and the daemon treats ids as opaque so it can
/// never prune one. Repeated calls with novel ids grow that row without limit.
/// That is acceptable only because reaching this endpoint at all already means
/// same-origin JS or local code, which could write the database directly — do
/// not read this constant as "the row is bounded". Promotions are for changes
/// big enough to be worth interrupting somebody over, so a real request carries
/// a handful.
const MAX_IDS: usize = 256;

/// Ceiling on one id's length. Ids are short kebab-case slugs, or a namespaced
/// form (`proj:<repo>:<slug>`) once a second source of promotions exists.
const MAX_ID_LEN: usize = 128;

pub fn routes() -> Router {
    Router::new()
        .route("/api/promotions/state", post(post_state))
        .route("/api/promotions/mark", post(post_mark))
}

#[derive(Deserialize)]
struct MarkBody {
    ids: Vec<String>,
    state: PromotionState,
}

/// Reject a list that is too long, or an id that is empty or oversized.
///
/// Returns the list unchanged rather than sanitising in place: an id the daemon
/// quietly rewrote would never match the bundle's own, so the user would be
/// shown the same card forever with nothing to see in the logs.
fn validate(ids: Vec<String>) -> Result<Vec<String>, StatusCode> {
    if ids.is_empty() || ids.len() > MAX_IDS {
        return Err(StatusCode::BAD_REQUEST);
    }
    if ids.iter().any(|id| id.is_empty() || id.len() > MAX_ID_LEN) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(ids)
}

async fn post_state(headers: HeaderMap) -> Result<Json<serde_json::Value>, StatusCode> {
    check_csrf(&headers)?;
    let db = open_db()?;
    // Order matters only for readability; both are idempotent. `first_use` is
    // stamped here because this is the first thing a loading client asks, which
    // is the closest the daemon gets to observing "the IDE was opened".
    let first_use = db.promotions_first_use().map_err(|e| {
        warn!("failed to read promotion first-use stamp: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let states = db.promotion_states().map_err(|e| {
        warn!("failed to read promotion states: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(serde_json::json!({
        "states": states,
        "first_use": first_use.to_rfc3339(),
    })))
}

async fn post_mark(
    headers: HeaderMap,
    Json(body): Json<MarkBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    check_csrf(&headers)?;
    let ids = validate(body.ids)?;
    let db = open_db()?;
    let states = db.mark_promotions(&ids, body.state).map_err(|e| {
        warn!("failed to mark promotions: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(serde_json::json!({ "states": states })))
}

#[cfg(test)]
mod tests {
    use super::{MAX_ID_LEN, MAX_IDS, MarkBody, routes, validate};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn req(uri: &str, csrf: bool, body: &str) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if csrf {
            b = b.header("x-veld-request", "1");
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn the_csrf_gate_applies_to_both_routes() {
        // Both write: `mark` obviously, and `state` stamps `first_use` on first
        // contact. The gate is two hand-placed `check_csrf` lines that nothing
        // else exercises — delete either and every other test stays green.
        for uri in ["/api/promotions/state", "/api/promotions/mark"] {
            let res = routes()
                .oneshot(req(uri, false, r#"{"ids":["a"],"state":"read"}"#))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
        }
    }

    #[tokio::test]
    async fn the_gate_lets_a_same_origin_call_through() {
        // The negative half alone cannot tell a working gate from a handler that
        // refuses everything. This one gets past `check_csrf` and fails later on
        // the database (no daemon instance in a unit test), which is exactly the
        // distinction being asserted: anything but 403.
        let res = routes()
            .oneshot(req(
                "/api/promotions/mark",
                true,
                r#"{"ids":[],"state":"read"}"#,
            ))
            .await
            .unwrap();
        assert_ne!(res.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn validate_rejects_empty_oversized_and_overlong_lists() {
        assert!(validate(vec!["ok".into()]).is_ok());
        assert!(validate(vec!["proj:my-repo:new-thing".into()]).is_ok());
        // An empty mark is a no-op that would report 200 and change nothing,
        // indistinguishable from success at the call site.
        assert!(validate(vec![]).is_err());
        assert!(validate(vec![String::new()]).is_err());
        assert!(validate(vec!["x".repeat(MAX_ID_LEN + 1)]).is_err());
        assert!(validate(vec!["x".to_string(); MAX_IDS + 1]).is_err());
    }

    #[test]
    fn a_state_name_this_daemon_does_not_know_is_rejected_at_the_edge() {
        // Deserialisation is the gate, so the handler never sees a state it
        // cannot store. A 422 here beats a silently-dropped mark.
        assert!(serde_json::from_str::<MarkBody>(r#"{"ids":["a"],"state":"read"}"#).is_ok());
        assert!(serde_json::from_str::<MarkBody>(r#"{"ids":["a"],"state":"snoozed"}"#).is_err());
    }
}
