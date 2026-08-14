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
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde_json::Value;
use tracing::warn;

use super::management::{check_csrf, open_db};

pub fn routes() -> Router {
    Router::new()
        .route("/api/settings", get(get_settings).patch(patch_settings))
        .route("/api/shells", get(get_shells))
        .route("/api/shells/intercept", post(post_shell_intercept))
}

/// How long a probe result is reused.
///
/// Short enough that pasting the suggested line and reopening the dialog shows the
/// change — which is the whole point of measuring rather than assuming — and long
/// enough that a caller in a loop cannot turn one request into one login shell.
const INTERCEPT_TTL: Duration = Duration::from_secs(10);

/// Whether `open`/`xdg-open` are **actually** caught in the user's chosen shell.
///
/// Asks the shell, rather than reasoning about it. veld has a startup handoff for
/// zsh (`ZDOTDIR`) and for a bash that honours `$ENV` in posix mode, and none for
/// fish, nushell or macOS's bash 3.2 — but even where there is one it can lose: a
/// `.zshrc` that clears `precmd_functions`, a `.bashrc` that rebuilds `PATH` in a
/// way veld's line cannot survive. Every one of those failures is silent, and the
/// user finds out when an agent's `open <url>` goes to Safari instead of a pane.
///
/// So this spawns the real shell, with the environment a real session would get,
/// and reports what `open` resolves to. `works: null` means the shell could not be
/// asked — deliberately distinct from `false`, because "we do not know" must not be
/// shown to a user as "this is broken".
///
/// Its own endpoint rather than a field on `/api/shells`, because it costs a login
/// shell (sub-second normally, up to 10s on a stalled rc file) and the picker's list
/// must stay instant.
///
/// # Why this is a `POST`, and gated, when it reads nothing
///
/// It spawns the user's **full login shell**, rc files and all. A `GET` is a *safe*
/// method: a bare `<img src="http://127.0.0.1:<port>/api/shells/intercept">` on any
/// page the developer visits is a simple request that needs no preflight and is sent
/// regardless of whether the reply can be read — so as an ungated `GET` this was one
/// `for` loop away from unbounded process creation, and `pty.rs` already writes the
/// rule down: *a safe route must stay genuinely safe — if it ever grows a side
/// effect, it needs the header and the client needs to send it.* `POST` plus
/// [`check_csrf`] means a cross-origin caller cannot reach it at all, since the
/// custom header forces a preflight it cannot pass.
///
/// The header alone does not bound a **same-origin** caller, and same-origin is not
/// a small set: a developer's own app reaches this daemon through the helper's
/// `/__veld__` proxy. So the result is also single-flighted and cached for
/// [`INTERCEPT_TTL`] — the lock is held across the probe, so a hundred concurrent
/// requests produce one shell and ninety-nine clones of its answer.
async fn post_shell_intercept(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    check_csrf(&headers)?;
    // **Keyed on everything the answer depends on**, which is not optional: the
    // flow this feature exists for is "open the dialog, pick a different shell,
    // read the verdict", and the client refetches immediately — well inside the
    // TTL. An unkeyed cache would hand back the *previous* shell's report, with
    // the wrong name, the wrong verdict and a hint naming the wrong rc file, and
    // nothing would refetch again to correct it.
    let db = open_db()?;
    let key = (
        db.terminal_shell(),
        db.terminal_open_urls_in_app(),
        db.terminal_intercept_system_open(),
    );
    drop(db);
    type Cached = Option<((String, bool, bool), Instant, Value)>;
    static CACHE: OnceLock<tokio::sync::Mutex<Cached>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
    // Held across the await deliberately: that is what makes this single-flight.
    let mut cached = cache.lock().await;
    if let Some((cached_key, measured_at, doc)) = cached.as_ref()
        && *cached_key == key
        && measured_at.elapsed() < INTERCEPT_TTL
    {
        return Ok(Json(doc.clone()));
    }
    let doc = shell_intercept_report().await?;
    *cached = Some((key, Instant::now(), doc.clone()));
    Ok(Json(doc))
}

/// The probe itself, without the gate or the cache.
async fn shell_intercept_report() -> Result<Value, StatusCode> {
    let db = open_db()?;
    let shell = db.terminal_shell();
    let open_in_app = db.terminal_open_urls_in_app();
    let intercept = db.terminal_intercept_system_open();
    let mut opts = super::pty::shims::SessionOptions {
        open_in_app,
        intercept,
        shell_integration: db.terminal_shell_integration(),
        agent_integration: db.terminal_agent_integration(),
        bash_handoff: false,
    };
    opts.bash_handoff = opts.wants_handoff()
        && veld_core::shell::kind(&shell) == veld_core::shell::Kind::Bash
        && veld_core::shell::supports_posix_env_handoff(&shell).await;
    // The session id is synthetic: nothing in the probe reaches `veld open-url`,
    // which is the only consumer, and minting a real one would register a terminal
    // that does not exist.
    let env = super::pty::shims::session_env("settings-probe", &shell, opts);
    let shim_dir = env.get("VELD_SHIM_DIR").cloned();
    let resolved = if open_in_app && intercept && shim_dir.is_some() {
        veld_core::shell::resolved_open(&shell, &env).await
    } else {
        None
    };
    // `starts_with` on the directory, not equality with the file: the shim is
    // `<dir>/open`, and a resolution that landed anywhere else in that directory is
    // still veld's.
    let works = match (&resolved, &shim_dir) {
        (Some(path), Some(dir)) => Some(path.starts_with(dir.as_str())),
        _ => None,
    };
    Ok(serde_json::json!({
        "shell": shell,
        "name": std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&shell),
        "enabled": open_in_app && intercept,
        "works": works,
        "resolved": resolved,
        // What to paste, and where. Only sent when it would help — a hint beside a
        // working feature is noise that teaches people to ignore hints.
        "hint": (works == Some(false)).then(|| {
            let (file, line) = veld_core::shell::path_hint(&shell);
            serde_json::json!({ "file": file, "line": line })
        }),
    }))
}

/// The shells this machine has, for the `terminal.shell` picker.
///
/// A separate endpoint rather than a field in the settings document, because it is
/// not a setting: it is what the *machine* can offer, it changes when someone
/// installs a shell rather than when someone saves a preference, and probing the
/// filesystem on every settings read (which every client does on every window
/// focus) would be a directory scan per focus.
///
/// Reports `auto` alongside the list so the picker can label its default with the
/// shell it actually resolves to — "Automatic (zsh)" is the answer to the question
/// this feature exists for, and the client must not compute it from `$SHELL`,
/// which in a browser it does not have and in Veld Desktop is Electron's.
async fn get_shells() -> Json<Value> {
    Json(serde_json::json!({
        "auto": veld_core::shell::auto_shell(),
        // The **user's** PATH, not the daemon's: under launchd ours is bare, and
        // /etc/shells lists only what the OS shipped — so reading the process
        // environment here offered every bash except the Homebrew one this
        // feature's whole point is to reach.
        //
        // `published_user_path`, never `cached_user_path`: this route is an
        // ungated GET, and the cached accessor *resolves inline* when nothing has
        // been published yet (deliberately without single-flighting), so during
        // the seconds between daemon start and the warm task's first answer, N
        // concurrent requests would spawn N login shells — reintroducing exactly
        // the amplification `post_shell_intercept` was just hardened against. Cold
        // means this one listing may miss a Homebrew shell for a few seconds;
        // `/etc/shells` still contributes, and the warm task fixes it shortly.
        "shells": veld_core::shell::discover(
            &veld_core::user_path::published_user_path().unwrap_or_default(),
        ),
    }))
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
    // `terminal.shell` is read by one thing that cannot re-read it per call: the
    // login shell `veld_core::user_path` spawns to learn the user's `PATH`, which
    // lives in a crate the gateway and the CLI link too and therefore holds no
    // database handle. Re-published here so a change takes effect at the next
    // resolution rather than at the next daemon restart. Terminals need nothing —
    // `mint_ticket` reads the setting per session.
    //
    // Unconditional rather than gated on the key being present in the patch: the
    // effective value is what matters and this is one database read, where a
    // `patch.contains_key` gate is a second place that has to know the key's name.
    veld_core::user_path::set_preferred_shell(Some(db.terminal_shell()));
    // Keep-awake reads its settings per decision, but only *makes* a decision on
    // a share event or its own tick — so without this, switching the automatic
    // hold off mid-share leaves the machine held for up to another thirty
    // seconds, and switching it on does nothing until the next share starts.
    // Detached and unconditional, for the same reason as the line above: the
    // reconcile returns early when nothing is held and nothing is shared, which
    // is cheaper than a second place that has to know these five key names.
    tokio::spawn(crate::feedback_server::caffeinate::settings_changed());
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
    async fn the_shell_probe_is_gated_and_is_not_a_safe_method() {
        // It spawns the user's full login shell. As an ungated GET, a bare
        // `<img src=…>` on any page the developer visits was one loop away from
        // unbounded process creation — a simple request needs no preflight and is
        // sent whether or not the reply can be read. So: not a GET, and gated.
        let res = routes()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shells/intercept")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);

        // …and the safe method is gone rather than left as a second way in.
        let res = routes()
            .oneshot(
                Request::builder()
                    .uri("/api/shells/intercept")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn the_shell_list_names_what_auto_resolves_to() {
        // The picker's default row reads "Automatic (<name>)", and the client
        // cannot work that name out itself: a browser has no `$SHELL`, and Veld
        // Desktop's is Electron's. So `auto` has to be in the payload, and it has
        // to be a value that could actually be spawned.
        let res = routes()
            .oneshot(
                Request::builder()
                    .uri("/api/shells")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let doc: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            doc["auto"].as_str().is_some_and(|s| s.starts_with('/')),
            "{doc}"
        );
        let shells = doc["shells"].as_array().expect("a list of shells");
        assert!(!shells.is_empty(), "no shell found on this machine: {doc}");
        for shell in shells {
            assert!(shell["path"].as_str().is_some_and(|p| p.starts_with('/')));
            assert!(shell["name"].as_str().is_some_and(|n| !n.is_empty()));
        }
    }

    #[tokio::test]
    async fn a_write_echoes_the_clamped_effective_document() {
        // The handler doc calls this "the point": the response carries what was
        // *stored*, so a clamp is visible instead of a control sitting at a value
        // the daemon refused. Uses an isolated database — these tests would
        // otherwise write the developer's dev DB.
        // Held for the whole test: `VELD_DB_PATH` is process-wide, and `extensions`
        // has its own tests that set and clear it. See `lock_db_env`.
        let _env = crate::feedback_server::lock_db_env();
        let dir = tempfile::TempDir::new().unwrap();
        // SAFETY: the lock above is what makes this the only writer of the variable.
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
