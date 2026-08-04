//! `GET`/`PATCH /api/settings` — user preferences shared by every UI client.
//!
//! Two clients hit this daemon: Veld Desktop and a plain browser tab, both loading
//! the same `/ide`. They must agree, which is the whole reason preferences are
//! server-side rather than in `localStorage`: a per-client store silently diverges
//! between the app and a browser tab, and "my font size reset" has no diagnosis.
//!
//! Kept out of `desktop::routes()` deliberately. Settings are not desktop-specific
//! — runs mode reads them too — and that router carries a blanket `csrf_layer`
//! whose doc comment warns that a route appended after the `.layer` call would be
//! silently unprotected. This module follows `management`'s idiom instead:
//! `check_csrf` called explicitly at the top of the mutating handler, where it is
//! visible in the handler being reviewed.

use std::collections::BTreeMap;

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde_json::Value;
use tracing::warn;

use super::management::{check_csrf, open_db};

pub fn routes() -> Router {
    Router::new().route("/api/settings", get(get_settings).patch(patch_settings))
}

/// Every setting's **effective** value — the Rust defaults with any stored rows
/// merged over them.
///
/// Returning effective values rather than only what is stored is what lets the UI
/// hold no defaults of its own. There is therefore no Rust↔TypeScript default pair
/// to drift, and a client added tomorrow gets a complete document without knowing
/// the schema.
async fn get_settings() -> Result<Json<Value>, StatusCode> {
    let db = open_db()?;
    let settings = db.settings().map_err(|e| {
        warn!("failed to read settings: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(serde_json::json!({ "settings": settings })))
}

/// Apply a patch: only the keys present in the body are written.
///
/// **A patch, not a replacement.** Two windows can have the settings surface open
/// at once, and a full-document `PUT` from a stale copy would revert whatever the
/// other one just changed. Per-key writes mean two clients editing different
/// settings both win, and only a genuine same-key collision resolves
/// last-write-wins — which is the right answer for a font size.
///
/// Validation, clamping and the both-or-neither rule live in
/// `veld_core::db::settings`, next to the key definitions; this is only the HTTP
/// shape of it.
async fn patch_settings(
    headers: HeaderMap,
    Json(patch): Json<BTreeMap<String, Value>>,
) -> Result<Json<Value>, StatusCode> {
    check_csrf(&headers)?;
    if patch.is_empty() {
        // An empty patch is a no-op that would otherwise report 200 and change
        // nothing, which is indistinguishable from success at the call site.
        return Err(StatusCode::BAD_REQUEST);
    }
    let db = open_db()?;
    db.patch_settings(&patch).map_err(|e| match e {
        veld_core::db::DbError::InvalidSetting { .. } => {
            warn!("rejected settings patch: {e}");
            StatusCode::BAD_REQUEST
        }
        other => {
            warn!("failed to write settings: {other}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;
    // Echo the full effective document back, so the caller applies exactly what
    // was stored rather than what it asked for — the clamp is invisible otherwise
    // and a slider would sit at a value the daemon never accepted.
    let settings = db.settings().map_err(|e| {
        warn!("failed to re-read settings: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(serde_json::json!({ "settings": settings })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn req(method: &str, csrf: bool, body: &str) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri("/api/settings")
            .header("content-type", "application/json");
        if csrf {
            b = b.header("x-veld-request", "1");
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn the_csrf_gate_applies_to_the_patch() {
        // The read is public by contract; the write is not. A settings write from
        // a cross-site page would be a silent preference rewrite.
        let res = routes()
            .oneshot(req("PATCH", false, r#"{"terminal.fontSize":14}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_empty_patch_is_rejected() {
        let res = routes().oneshot(req("PATCH", true, "{}")).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_malformed_body_is_a_client_error_not_a_panic() {
        // axum rejects at deserialization, before the handler — pinned so a future
        // change to the extractor type cannot turn this into a 500.
        let res = routes()
            .oneshot(req("PATCH", true, "not json"))
            .await
            .unwrap();
        assert!(
            res.status().is_client_error(),
            "expected 4xx, got {}",
            res.status()
        );
    }

    #[tokio::test]
    async fn a_rejected_value_is_a_400() {
        // `InvalidSetting` must not surface as "database error".
        let res = routes()
            .oneshot(req("PATCH", true, r#"{"terminal.cursorStyle":"wobble"}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_write_echoes_the_clamped_effective_document() {
        // The handler doc calls this "the point": the response carries what was
        // *stored*, so a clamp is visible instead of a control sitting at a value
        // the daemon refused. Uses an isolated database — these tests would
        // otherwise write the developer's dev DB.
        let dir = tempfile::TempDir::new().unwrap();
        // SAFETY: single-threaded test process; the daemon reads this per request.
        unsafe { std::env::set_var("VELD_DB_PATH", dir.path().join("t.db")) };

        let res = routes()
            .oneshot(req("PATCH", true, r#"{"terminal.fontSize":9999}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(doc["settings"]["terminal.fontSize"], serde_json::json!(72));
        // An untouched key still arrives, because the response is the effective
        // document rather than the patch.
        assert_eq!(
            doc["settings"]["worktree.markerStyle"],
            serde_json::json!("color")
        );

        unsafe { std::env::remove_var("VELD_DB_PATH") };
    }
}
