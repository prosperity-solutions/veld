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
    extract::Path,
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use serde_json::Value;
use tracing::warn;

use super::management::{check_csrf, open_db};

pub fn routes() -> Router {
    Router::new()
        .route("/api/settings", get(get_settings).patch(patch_settings))
        .route("/api/settings/catalog", get(get_catalog))
        .route("/api/settings/{key}", delete(delete_setting))
        .route("/api/shells", get(get_shells))
        .route("/api/shells/intercept", post(post_shell_intercept))
}

/// A failure with, when there is one, a sentence for the caller.
///
/// The rest of this daemon answers with a bare [`StatusCode`], which is right
/// where the status *is* the whole message (403 for a missing CSRF header). It
/// stops being right for a rejected settings write: the validator already knows
/// why — *"must not put %s in the host"* — and a 400 with an empty body throws
/// that away at the last hop, leaving `veld settings set` and the dialog both
/// saying "invalid" about a value whose fault is knowable.
///
/// Deliberately local to this module rather than a daemon-wide error type. Every
/// other handler here is fine as it is, and a shared type would invite the
/// question of what to put in it for handlers with nothing to say.
#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    message: Option<String>,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: Some(message.into()),
        }
    }

    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: None,
        }
    }
}

/// So `check_csrf` and `open_db`, which answer in bare statuses, still work with
/// `?` in a handler that returns this.
impl From<StatusCode> for ApiError {
    fn from(status: StatusCode) -> Self {
        Self {
            status,
            message: None,
        }
    }
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self.message {
            // The `error` key is what `veld`'s own `--json` output uses for a
            // failure, so a CLI can surface the daemon's sentence unchanged.
            Some(message) => {
                (self.status, Json(serde_json::json!({ "error": message }))).into_response()
            }
            None => self.status.into_response(),
        }
    }
}

/// The header a `veld` CLI uses to name the database it is reading.
///
/// See [`require_same_db`]. A browser never sends it, and must not have to.
const DB_HEADER: &str = "X-Veld-Db";

/// Refuse a write from a client that reads a *different* database than this
/// daemon writes.
///
/// This is not hypothetical and it is not only a dev-stack concern. `veld`'s own
/// backstop resolves a **cargo-built** binary to `.veld-dev/veld-cargo.db` so a
/// test can never migrate the real user database — but the daemon it would reach
/// over HTTP is whatever is listening on the port, which for a bare `cargo run`
/// is the *installed* daemon on the *real* database. So `veld settings set`
/// would write one database and `veld settings get` would read another: the set
/// reports success, the get reports the old value, and the user's real
/// preferences change with nothing pointing at why. That happened once, during
/// this feature's own smoke test, which is why the check exists rather than a
/// comment saying to be careful.
///
/// The comparison lives here because only this process knows which file it
/// actually opened; a CLI can compute what *it* would open and nothing more.
/// Absent header means an ordinary browser client and is allowed — the check is
/// for callers that have a database of their own, not an authentication step.
fn require_same_db(headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(raw) = headers.get(DB_HEADER) else {
        // No header at all: an ordinary browser client, which cannot know a
        // filesystem path and must not be asked to.
        return Ok(());
    };
    let ours = veld_core::db::Db::default_path().map_err(|e| {
        warn!("could not resolve this daemon's database path: {e}");
        ApiError::internal()
    })?;
    // Both ends compare the **percent-encoded** form. The header is a
    // `HeaderValue`, whose readable range is `32..=126`, while its builder
    // accepts any byte from 32 up — so a raw path carrying one non-ASCII byte
    // (an accented username is enough) produced a header this daemon could not
    // decode. Read as absent, that was waved through as "a browser client", and
    // the guard silently stopped guarding for exactly the machines it still had
    // to protect. Encoding makes the value ASCII by construction.
    let ours_encoded = veld_core::percent::encode_component(&ours.to_string_lossy());
    // A header that is present but undecodable is a **mismatch**, never an
    // absence. Unreachable once both ends encode, which is why it must refuse
    // rather than shrug: if it is ever reached, the two ends disagree about the
    // encoding and that is precisely when the comparison must not be skipped.
    let Ok(theirs) = raw.to_str() else {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            message: Some(format!(
                "this daemon's settings live in {} — the caller sent a database \
                 path this daemon cannot decode",
                ours.display()
            )),
        });
    };
    if ours_encoded == theirs {
        return Ok(());
    }
    // Decoded for the message. `theirs` is the percent-encoded form both ends
    // compare, so echoing it raw produced
    // "the caller is reading %2FUsers%2Fyou%2F..." — a sentence naming a path
    // nobody typed, in the one place a human is trying to work out which two
    // files disagree.
    let theirs = veld_core::percent::decode_component(theirs);
    Err(ApiError {
        status: StatusCode::CONFLICT,
        message: Some(format!(
            "this daemon's settings live in {} — the caller is reading {theirs}",
            ours.display()
        )),
    })
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
) -> Result<Json<Value>, ApiError> {
    check_csrf(&headers)?;
    require_same_db(&headers)?;
    if patch.is_empty() {
        // An empty patch is a no-op that would otherwise report 200 and change
        // nothing, which is indistinguishable from success at the call site.
        return Err(ApiError::bad_request("a settings patch must name a key"));
    }
    let db = open_db()?;
    db.patch_settings(&patch).map_err(|e| match e {
        veld_core::db::DbError::InvalidSetting { .. } => {
            warn!("rejected settings patch: {e}");
            // The validator's own sentence — see `DbError::InvalidSetting`. This
            // is the only place it can reach a CLI or a dialog, and dropping it
            // is what made a refused `browser.searchUrl` unfixable without
            // reading the Rust.
            ApiError::bad_request(e.to_string())
        }
        other => {
            warn!("failed to write settings: {other}");
            ApiError::internal()
        }
    })?;
    Ok(Json(after_settings_write(&db)?))
}

/// Remove one setting's stored row, putting it back on its default.
///
/// A `DELETE` rather than a `PATCH` carrying `null`, because those are two
/// different facts and one of them is already taken: an unknown key stores
/// whatever JSON it is handed, `null` included, so "reset this" and "store the
/// value null under this key" would be the same request. The method that means
/// *remove* is the one that says so.
async fn delete_setting(
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    check_csrf(&headers)?;
    require_same_db(&headers)?;
    let db = open_db()?;
    // `Db::unset_settings` returns the rows it actually removed, and its own doc
    // says the point is that a caller can tell "reset" from "was already on the
    // default" without a prior read. Dropping it here is what made
    // `veld settings unset <typo>` print a confident "cleared" and exit 0.
    let removed = db.unset_settings(&[key]).map_err(|e| {
        warn!("failed to unset setting: {e}");
        ApiError::internal()
    })?;
    let mut body = after_settings_write(&db)?;
    body["removed"] = serde_json::json!(removed);
    Ok(Json(body))
}

/// The two side effects every settings write owes the rest of the daemon, plus
/// the effective document to echo back.
///
/// Factored out of `patch_settings` when `DELETE` arrived rather than copied
/// into it: both of these are *unconditional* re-reads precisely so that no
/// caller has to know which keys it touched, and a second handler that
/// remembered the echo but forgot the shell re-publish would be a bug nothing
/// fails on — the wrong `PATH` shows up later, in a terminal, with no trail back
/// to a settings write.
fn after_settings_write(db: &veld_core::db::Db) -> Result<Value, ApiError> {
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
    // Note for whoever adds a settings test next: this publishes into
    // process-wide state, so a test that PATCHes `terminal.shell` to a stub
    // changes what every *other* test in this binary resolves as its user
    // `PATH` — the shape of issue #310, where two `veld-core` tests published a
    // stub shell and left the crate's suite permanently a few tests red. Patch
    // it back, or don't route through this handler.
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
        ApiError::internal()
    })?;
    Ok(serde_json::json!({ "settings": settings }))
}

/// The catalog: what each setting is, what it may hold, and what to call it.
///
/// Unauthenticated and uncached like `GET /api/settings`, and it carries no user
/// data at all — every byte of it is a compile-time constant of this binary. It
/// is served rather than bundled so that the `/ide` client and `veld settings`
/// describe a setting identically without either one holding a copy.
async fn get_catalog() -> Json<Value> {
    Json(serde_json::json!({
        "groups": veld_core::db::catalog_groups(),
        "settings": veld_core::db::catalog(),
        // Facts about *this machine* that a catalog cannot state, because they are
        // not properties of the setting. `backup.dir`'s stored value is empty until
        // somebody picks a folder, and "empty" is a real value meaning "the one veld
        // derives" — so a client showing an empty box has no way to answer the only
        // question the row is ever asked, which is *where are my backups*. Resolved
        // here rather than in the bundle because the answer depends on the platform,
        // on `VELD_DB_PATH`, and on whether this is a dev build.
        "machine": {
            "backupDir": veld_core::db::backup::default_dir(),
        },
    }))
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

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn body_json(res: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The catalog is public and complete. Public because it is the same
    /// unauthenticated contract `GET /api/settings` already has.
    ///
    /// It used to be true that *every byte is a compile-time constant of this
    /// binary*; `machine.backupDir` is the exception, and it is a runtime path.
    /// That is not a new class of exposure — `GET /api/settings` is gated
    /// identically and already returns `worktree.storageDir`, an absolute path the
    /// user chose — but the old claim would have quietly stopped being true, which
    /// is worse than the path itself.
    #[tokio::test]
    async fn the_catalog_is_served_without_a_csrf_header() {
        // **A reader of `VELD_DB_PATH` needs the lock as much as a writer does.**
        // `machine.backupDir` is derived from it, and this test computes the
        // expectation *after* the request that produced the answer — so a sibling
        // test setting or clearing the variable in between makes the two sides
        // disagree and fails here with no hint that the environment moved. Seen
        // once in a full `just test` run, green on its own immediately after.
        // Exactly one acquire per test — the mutex is not reentrant.
        let _env = crate::feedback_server::lock_db_env();
        let res = routes()
            .oneshot(
                Request::builder()
                    .uri("/api/settings/catalog")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let doc = body_json(res).await;
        let groups = doc["groups"].as_array().expect("groups");
        let settings = doc["settings"].as_array().expect("settings");
        assert_eq!(groups.len(), veld_core::db::SettingGroup::ALL.len());
        assert_eq!(settings.len(), veld_core::db::SettingKey::ALL.len());

        // The one runtime fact the catalog carries. `backup.dir`'s stored value is
        // empty until somebody picks a folder, and empty is a real value meaning
        // "the one veld derives" — so without this the settings dialog cannot
        // answer the only question that row is ever asked, and showed an invented
        // example path instead while a sentence beneath it said the default was in
        // use. The two contradicted each other; a real path says one thing.
        assert_eq!(
            doc["machine"]["backupDir"].as_str(),
            veld_core::db::backup::default_dir()
                .as_deref()
                .and_then(std::path::Path::to_str),
        );

        // Every entry carries what a client needs to render it without knowing
        // any key by name — the property the whole catalog exists for.
        for entry in settings {
            for field in [
                "key", "title", "help", "group", "type", "default", "choices",
            ] {
                assert!(!entry[field].is_null(), "{} has no {field}", entry["key"]);
            }
            assert!(
                !entry["choices"]["kind"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "{}'s choices carry no kind",
                entry["key"]
            );
        }
    }

    /// Every group a setting claims is one the catalog also publishes, over the
    /// wire rather than only in Rust. A client tabs by these ids, so a group that
    /// exists on a setting and not in the list renders an empty tab or drops the
    /// setting entirely.
    #[tokio::test]
    async fn every_settings_group_is_published() {
        let res = routes()
            .oneshot(
                Request::builder()
                    .uri("/api/settings/catalog")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let doc = body_json(res).await;
        let ids: Vec<String> = doc["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["id"].as_str().unwrap().to_string())
            .collect();
        for entry in doc["settings"].as_array().unwrap() {
            let group = entry["group"].as_str().unwrap().to_string();
            assert!(
                ids.contains(&group),
                "{} is in unpublished group {group}",
                entry["key"]
            );
        }
    }

    /// The unset is a write, so it is gated like one. Without this a cross-site
    /// page could reset any preference — quieter than changing it, and just as
    /// much of a change.
    #[tokio::test]
    async fn the_csrf_gate_applies_to_the_delete() {
        let res = routes()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/settings/terminal.fontSize")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    /// A caller reading a different database is refused, and the refusal says
    /// which two files disagree.
    ///
    /// This is the guard that would have prevented the incident in
    /// `require_same_db`'s own doc comment: a cargo-built `veld` writing the
    /// installed daemon's real database while reading its own dev one. Asserted
    /// on `DELETE` as well as `PATCH` because a reset is as destructive as a set
    /// and the two handlers are separate call sites.
    #[tokio::test]
    async fn a_caller_reading_another_database_is_refused() {
        for (method, uri, body) in [
            ("PATCH", "/api/settings", r#"{"terminal.fontSize":14}"#),
            ("DELETE", "/api/settings/terminal.fontSize", ""),
        ] {
            let res = routes()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .header("x-veld-request", "1")
                        .header(DB_HEADER, "/definitely/not/this/daemons/veld.db")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                StatusCode::CONFLICT,
                "{method} {uri} accepted a foreign database"
            );
            let doc = body_json(res).await;
            let error = doc["error"].as_str().unwrap_or_default();
            assert!(
                error.contains("/definitely/not/this/daemons/veld.db"),
                "the refusal must name the caller's database, got {error:?}"
            );
        }
    }

    /// A header the daemon cannot decode is a **mismatch**, never an absence.
    ///
    /// `HeaderValue::to_str` refuses any byte >= 127 (`http`'s `is_visible_ascii`),
    /// while the CLI builds the value from a `String`, which accepts them. So a
    /// user whose database path holds one non-ASCII byte — a non-ASCII macOS
    /// username is enough — sent a header the daemon read as *absent* and was
    /// waved through as "an ordinary browser client". A guard that exists because
    /// it already cost somebody their real settings must not fail open for the
    /// subset of users with an accent in their home directory. Both ends now
    /// compare the percent-encoded form, so the value is ASCII by construction and
    /// this arm is unreachable in practice — which is why it must be a refusal
    /// rather than a shrug.
    #[tokio::test]
    async fn a_database_header_the_daemon_cannot_read_is_refused_not_ignored() {
        let res = routes()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .header("x-veld-request", "1")
                    // Raw UTF-8, the shape the CLI used to send for
                    // `/Users/Jos\u{e9}/Library/Application Support/veld/veld.db`.
                    .header(
                        DB_HEADER,
                        axum::http::HeaderValue::from_bytes("/Users/Jos\u{e9}/veld.db".as_bytes())
                            .unwrap(),
                    )
                    .body(Body::from(r#"{"terminal.fontSize":14}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::CONFLICT,
            "an undecodable database header was treated as absent and the write was allowed"
        );
    }

    /// A caller whose database path matches is accepted, and the value it sends
    /// stays readable however the path is spelled.
    ///
    /// The other half of the fix. Refusing an undecodable header would have been
    /// enough for *safety* and would have left every user with an accent in their
    /// home directory permanently on the direct-write fallback, silently losing
    /// the two side effects that are the entire reason a write goes through the
    /// daemon. Encoding is what makes those machines work rather than merely fail
    /// safely.
    ///
    /// Deliberately does **not** set `VELD_DB_PATH`. An earlier version did, to
    /// force a non-ASCII path, and it raced `a_rejected_value_is_a_400` — that
    /// test opens the database through the handler without taking `lock_db_env`,
    /// so it saw this one's tempdir after it had been removed and reported 500
    /// instead of 400, about one run in three. The env var is process-wide; a
    /// test that mutates it is a test every unlocked sibling now depends on.
    /// `veld_core::percent`'s own tests cover the non-ASCII encoding; what this
    /// needs from the router is only that the *comparison* accepts a correctly
    /// encoded match, which the real path proves without touching the
    /// environment at all.
    #[tokio::test]
    async fn a_caller_whose_database_path_matches_is_accepted() {
        // Removing this test's own `VELD_DB_PATH` mutation was not enough: it
        // reads `Db::default_path()` here and `require_same_db` reads it again
        // inside the request, across an await, while a sibling in this module
        // sets and clears that process-wide variable. Two reads of env-derived
        // state that must agree need the lock whether or not *this* test is the
        // one writing. Measured: 3 failures in 12 filtered runs without it.
        let _env = crate::feedback_server::lock_db_env();
        let ours = veld_core::db::Db::default_path().unwrap();
        let encoded = veld_core::percent::encode_component(&ours.to_string_lossy());
        assert!(
            encoded.bytes().all(|b| (32..127).contains(&b)),
            "the encoder must produce a header a daemon can read back"
        );

        let res = routes()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .header("x-veld-request", "1")
                    .header(DB_HEADER, encoded)
                    // Empty, so this asserts on the database check alone and
                    // writes nothing: a 400 means it got *past* `require_same_db`,
                    // which is the whole claim. A 409 would mean it did not.
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "a matching database path must be accepted, not refused with 409"
        );
    }

    /// An absent header is an ordinary browser client and is allowed through.
    ///
    /// The check exists for callers that have a database of their own; making it
    /// mandatory would break `/ide`, which is the one client that cannot possibly
    /// know a filesystem path.
    #[tokio::test]
    async fn a_client_with_no_database_of_its_own_is_not_refused() {
        let res = routes()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .header("x-veld-request", "1")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 400 for the empty patch — the point is that it got *past* the database
        // check rather than being refused with a 409.
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
