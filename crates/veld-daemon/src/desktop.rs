//! Veld Desktop APIs: the repo/worktree registry behind the `/ide` management
//! UI and its Electron shell.
//!
//! A "repo" is a git repository the user imported (keyed by its main checkout
//! root); worktrees are its `git worktree` checkouts. Run state is not
//! duplicated here — the UI joins a worktree to `/api/environments` by path
//! (every worktree with a root config is its own veld project root).
//!
//! Git subprocesses run with the user's login-shell `PATH` (AGENTS.md daemon
//! rule) and argument-vector spawning — no shell interpolation. Mutating
//! endpoints carry the same `X-Veld-Request` CSRF gate as the management API.

use std::path::{Path as FsPath, PathBuf};

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use veld_core::db::{
    ConfigSource, Db, DiscoveredWorktree, GitCreateSource, RepoRecord, WorktreeRecord,
    default_alias,
};
use veld_core::user_path::cached_user_path;

use super::management::{check_csrf, is_safe_identifier, open_db, spawn_veld, validate_run_name};

/// Build an axum [`Router`] for the desktop APIs (mounted into the daemon's
/// HTTP server alongside the management routes).
///
/// CSRF is enforced as a LAYER, not per handler: every non-GET request on
/// this router must carry `X-Veld-Request` (see `check_csrf`), so a mutating
/// route cannot ship ungated by forgetting a per-handler call.
///
/// **Add new routes ABOVE the `.layer(...)` call.** axum applies middleware
/// only to routes registered before it — "Additional routes added after
/// `layer` is called will not have the middleware added" — so a `.route()`
/// appended after it would be silently unprotected.
pub fn routes() -> Router {
    Router::new()
        .route("/api/repos", get(list_repos).delete(remove_repo))
        .route("/api/repos/refresh", post(refresh_repos))
        .route("/api/repos/update-main", post(update_main))
        .route("/api/repos/revert-root", post(revert_repo_root))
        .route("/api/repos/import", post(import_repo))
        // The branches a worktree can be created from. A GET: it reads refs and
        // fetches nothing (see `list_branches`), so it stays inside this
        // router's read-only-GET contract.
        .route("/api/repos/branches", get(list_branches))
        .route("/api/worktrees", post(create_worktree))
        .route(
            "/api/worktrees/{id}",
            patch(patch_worktree).delete(delete_worktree),
        )
        .route("/api/worktrees/{id}/start", post(start_worktree_run))
        .route("/api/worktrees/{id}/restore", post(restore_worktree))
        .route("/api/worktrees/{id}/status", get(worktree_status))
        // Extension surfaces are worktree-scoped, so they live here and
        // inherit `csrf_layer` — both of them execute a project-declared
        // command, so neither may ever become a GET.
        .route(
            "/api/worktrees/{id}/extensions/status",
            post(super::extensions::status),
        )
        .route(
            "/api/worktrees/{id}/extensions/activate",
            post(super::extensions::activate),
        )
        .route("/api/worktrees/{id}/revert", post(revert_worktree))
        .route("/api/worktrees/{id}/delete", post(delete_trashed_worktree))
        .route("/api/trash", delete(empty_trash))
        .route(
            "/api/worktrees/{id}/trash-error",
            delete(dismiss_trash_error),
        )
        // `/api/lane-order`, not `/api/lanes/order`: a static segment wins over a
        // dynamic one, so `/api/lanes/order` would shadow `/api/lanes/{name}` for a
        // lane the user is allowed to call "order" — `PATCH`/`DELETE` would hit this
        // POST-only node and 405, leaving that lane impossible to rename or delete.
        // Same reasoning for the worktree order against `/api/worktrees/{id}`, where
        // the id is numeric and so cannot actually collide — kept parallel anyway,
        // because the next reader should not have to work out which of the two was
        // safe by accident.
        .route("/api/worktree-order", post(reorder_worktrees))
        // `/api/repo-order`, not `/api/repos/order`: a repo is addressed by its
        // root *path*, so an order endpoint hanging off `/api/repos/` would sit in
        // the same namespace as a path segment. Same reasoning as the two orders
        // below it.
        .route("/api/repo-order", post(reorder_repos))
        .route("/api/worktree-emoji", get(worktree_emoji))
        .route("/api/lanes", get(list_lanes).post(create_lane))
        .route("/api/lane-order", post(reorder_lanes))
        .route("/api/lanes/{name}", patch(rename_lane).delete(delete_lane))
        .route("/api/pick-directory", post(pick_directory))
        // Veld's own database: is it intact, is it being backed up, and put a
        // backup back. A GET for the read (so a browser tab can poll it without
        // the CSRF header) and POSTs for the two things that change something.
        .route("/api/db-health", get(db_health))
        .route("/api/db-health/notified", post(db_health_notified))
        .route("/api/db-health/restore", post(db_health_restore))
        .route(
            "/api/open-worktree-storage-dir",
            post(open_worktree_storage_dir),
        )
        .layer(axum::middleware::from_fn(csrf_layer))
}

/// Reject any mutating request without the `X-Veld-Request` header. GETs on
/// this router are read-only by contract (enforced by keeping side effects
/// out of them — see `list_repos`).
async fn csrf_layer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::Method;
    use axum::response::IntoResponse;
    // HEAD rides along with GET (axum auto-serves it for get() routes) and is
    // equally side-effect-free. OPTIONS is deliberately NOT exempt: a CORS
    // preflight without the header failing is exactly the cross-origin block
    // this gate exists for.
    let safe = req.method() == Method::GET || req.method() == Method::HEAD;
    if !safe && check_csrf(req.headers()).is_err() {
        return err(StatusCode::FORBIDDEN, "missing X-Veld-Request header").into_response();
    }
    next.run(req).await
}

// ---------------------------------------------------------------------------
// Process-global guards
// ---------------------------------------------------------------------------

/// A "one at a time, process-wide" gate.
///
/// Extracted from [`pick_directory`], where it was a function-local `static`
/// plus an inline drop guard — correct, and untestable without opening a real
/// modal dialog on somebody's screen. The release-on-drop half is the part worth
/// pinning: every early return in that handler (a timeout, a backend failure,
/// `?` on a serialization error) depends on it, and a leak there wedges the
/// endpoint at 409 until the daemon restarts.
struct SingleFlight(std::sync::atomic::AtomicBool);

/// Held while a single-flight section runs; releases on drop.
struct SingleFlightGuard<'a>(&'a SingleFlight);

impl SingleFlight {
    const fn new() -> Self {
        Self(std::sync::atomic::AtomicBool::new(false))
    }

    /// `None` when somebody else is already inside.
    fn try_enter(&self) -> Option<SingleFlightGuard<'_>> {
        use std::sync::atomic::Ordering;
        if self.0.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(SingleFlightGuard(self))
        }
    }
}

impl Drop for SingleFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A debounce clock with the value the last real run produced.
///
/// Extracted from [`refresh_repos`] for the same reason as [`SingleFlight`]: it
/// was a function-local `static` whose only exercise was a handler that spawns
/// git. The memo is not an optimisation — it is what keeps concurrent clients
/// *consistent*. Inside the window, a caller must be handed the previous run's
/// answer rather than recomputing a weaker one (`is_dir` instead of a git
/// reconcile), because the two disagree exactly when something is wrong.
struct Debounce<T>(std::sync::Mutex<Option<(std::time::Instant, T)>>);

impl<T: Clone> Debounce<T> {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    /// The last recorded value, if it was recorded within `window`.
    fn fresh_within(&self, window: std::time::Duration) -> Option<T> {
        let last = self.0.lock().expect("refresh debounce mutex poisoned");
        match &*last {
            Some((at, value)) if at.elapsed() < window => Some(value.clone()),
            _ => None,
        }
    }

    /// Start the window again, at `value`.
    fn record(&self, value: T) {
        *self.0.lock().expect("refresh debounce mutex poisoned") =
            Some((std::time::Instant::now(), value));
    }
}

// ---------------------------------------------------------------------------
// Native directory picker
// ---------------------------------------------------------------------------

/// Result of one picker-backend attempt.
enum Pick {
    Chosen(String),
    Cancelled,
    /// The backend ran but failed (no GUI session, permission denied, …).
    Failed(String),
    /// The backend binary doesn't exist on this system.
    Unavailable,
}

async fn run_picker(cmd: &str, args: &[&str]) -> Pick {
    let out = tokio::process::Command::new(cmd)
        .args(args)
        .env("PATH", cached_user_path().await)
        // If the request is abandoned (timeout, client gone) the dialog
        // process must not linger on the user's screen.
        .kill_on_drop(true)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            Pick::Chosen(String::from_utf8_lossy(&o.stdout).trim().to_string())
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
            // osascript reports a dismissed dialog as "User canceled. (-128)"
            // (the numeric code is locale-independent); zenity/kdialog signal
            // cancel purely via exit code 1 — stderr must be IGNORED there,
            // because GTK/Qt binaries spawned from a daemon context routinely
            // print module/a11y warnings even on a clean cancel. Anything
            // else is a real failure (no display, TCC denial) and must NOT
            // read as cancel.
            let cancelled = if cmd == "osascript" {
                stderr.contains("-128") || stderr.to_lowercase().contains("user canceled")
            } else {
                o.status.code() == Some(1)
            };
            if cancelled {
                Pick::Cancelled
            } else {
                Pick::Failed(if stderr.is_empty() {
                    format!("{cmd} exited with {}", o.status)
                } else {
                    stderr
                })
            }
        }
        Err(_) => Pick::Unavailable,
    }
}

#[derive(Deserialize)]
struct PickDirectoryQuery {
    #[serde(default)]
    purpose: Option<String>,
}

/// The dialog's prompt/title, picked from a **fixed, server-known set** —
/// never a caller-supplied string. `purpose` is matched, not interpolated:
/// the macOS branch below builds an AppleScript string literal around
/// whichever one of these comes back, and a client-controlled prompt would
/// need escaping this daemon has no reason to write when two hardcoded
/// options say everything a caller of this endpoint means today. Anything
/// unrecognised (including absent) is the original, unchanged default.
fn pick_directory_prompt(purpose: Option<&str>) -> &'static str {
    match purpose {
        Some("worktree-storage") => "Choose a folder for worktree checkouts",
        Some("backup-dir") => "Choose a folder for Veld's database backups",
        _ => "Choose a git repository",
    }
}

/// Open the OS folder picker and return the chosen absolute path. The daemon
/// runs in the user's GUI session (it already opens Terminal.app), so it can
/// host the dialog for the browser build too — the web platform itself never
/// exposes absolute paths. Responses: 200 `{path}`, 204 on cancel, 409 while
/// another pick is already open, 408 after the 10-minute timeout, 501 when no
/// picker backend exists, 500 when the backend fails (no GUI session, macOS
/// permission denial).
async fn pick_directory(
    Query(q): Query<PickDirectoryQuery>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;

    let prompt = pick_directory_prompt(q.purpose.as_deref());

    // Single-flight: dialogs are modal on the user's screen; N tabs (or a
    // scripted loop) must not stack N of them. The guard releases on drop, which
    // is what covers every early return below.
    static PICKER_OPEN: SingleFlight = SingleFlight::new();
    let Some(_open) = PICKER_OPEN.try_enter() else {
        return Err(err(
            StatusCode::CONFLICT,
            "a directory picker is already open",
        ));
    };

    // 10 minutes: the request intentionally blocks while the dialog is open.
    let picked = tokio::time::timeout(std::time::Duration::from_secs(600), async {
        if cfg!(target_os = "macos") {
            // `choose folder` is a Standard Additions dialog — deliberately no
            // "System Events" activate (that is TCC-gated and a denial would
            // abort the script before the dialog ever shows).
            let script = format!("POSIX path of (choose folder with prompt \"{prompt}\")");
            run_picker("osascript", &["-e", &script]).await
        } else {
            // Linux: try zenity, then kdialog.
            let mut last = Pick::Unavailable;
            let zenity_title = format!("--title={prompt}");
            for (cmd, args) in [
                (
                    "zenity",
                    &["--file-selection", "--directory", &zenity_title][..],
                ),
                (
                    "kdialog",
                    &["--getexistingdirectory", ".", "--title", prompt][..],
                ),
            ] {
                match run_picker(cmd, args).await {
                    Pick::Unavailable => continue, // binary missing — try next
                    outcome => {
                        last = outcome;
                        break;
                    }
                }
            }
            last
        }
    })
    .await
    .map_err(|_| err(StatusCode::REQUEST_TIMEOUT, "picker timed out"))?;

    match picked {
        Pick::Chosen(path) if !path.is_empty() => {
            Ok(Json(serde_json::json!({ "path": path })).into_response())
        }
        Pick::Chosen(_) | Pick::Cancelled => Ok(StatusCode::NO_CONTENT.into_response()),
        Pick::Failed(reason) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("directory picker failed: {reason}"),
        )),
        Pick::Unavailable => Err(err(
            StatusCode::NOT_IMPLEMENTED,
            "no directory picker available on this system",
        )),
    }
}

/// FNV-1a. Deterministic across every future `rustup update stable`, unlike
/// `std::hash::Hasher`'s default algorithm, whose own docs disclaim that
/// stability "over releases" — and this hash has to be stable forever, since
/// it decides which folder a repository's worktrees keep landing in.
fn stable_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    bytes.iter().fold(FNV_OFFSET, |hash, &b| {
        (hash ^ b as u64).wrapping_mul(FNV_PRIME)
    })
}

/// A short, on-disk grouping folder for one repository's worktrees:
/// `<slugified basename>-<8 hex chars of a hash of the canonicalized root>`.
///
/// Plumbing, not a display name — the alias/`display_name` already own that
/// job in the rail — so a hash suffix is fine here. It exists because the
/// basename alone cannot promise uniqueness: `repos.name` (also a basename,
/// set at import) carries no `UNIQUE` constraint, only `repos.root` does, so
/// two different repositories both called `backend` — cloned into two
/// different parent directories — would otherwise be handed the *same*
/// bucket. That is precisely what a shared custom storage directory does:
/// funnel every imported repository into one folder, where a bare basename
/// collision becomes two unrelated projects permanently blocking each
/// other's common aliases (`main`, `feat`) with the loud "already exists"
/// `409` below — not silent, but not survivable either, since neither
/// project can ever use that alias again while the other one also lands
/// there. The same folder-per-parent-directory case exists in `sibling`
/// mode too when two repositories happen to share one, just less often. The
/// hash is over the *canonicalized* path so a symlink or a trailing
/// component cannot change which bucket a repository lands in.
fn project_slug(repo_root: &FsPath) -> String {
    let canon = canonicalize_prefix(repo_root);
    let base = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    // `slugify` already caps at 48 bytes and trims a dash truncation could
    // expose (`url.rs`) — no second cap needed here.
    let mut slug = veld_core::url::slugify(&base);
    if slug.is_empty() {
        // A basename with no ASCII alphanumerics (e.g. entirely CJK) slugifies
        // to "" — the same degenerate case the `file_name()` fallback three
        // lines up exists for, just reached a different way. Left unhandled,
        // the bucket name would start with the hash's leading `-`.
        slug = "repo".to_string();
    }
    // `as_encoded_bytes`, not `to_string_lossy`: on Linux a filename is
    // arbitrary bytes with no UTF-8 requirement, and `to_string_lossy`
    // replaces anything invalid with U+FFFD — so two byte-distinct roots
    // that both contain invalid UTF-8 could lossy-mangle to the same string
    // and hash identically, which is exactly the collision this function
    // exists to prevent. The encoded form is lossless and just as cheap to
    // hash. (`repo_root` is a JSON string / SQLite TEXT column in practice,
    // so this is defence in depth rather than a live bug today.)
    let hash = stable_hash(canon.as_os_str().as_encoded_bytes()) as u32;
    format!("{slug}-{hash:08x}")
}

/// Canonicalize as much of `path` as already exists, resolving the rest
/// **lexically** rather than leaving it untouched. Plain [`Path::canonicalize`]
/// requires the *whole* path to exist, which a checkout path about to be
/// created by `git worktree add` never does yet — but a symlink earlier in
/// the path still needs resolving, or a prefix check against an
/// already-canonical `repo_root` can miss a real match. This is not exotic:
/// macOS's own temp directory is `/var/folders/...`, and `/var` is itself a
/// symlink to `/private/var` — so an unresolved path one level under a fresh
/// `_worktrees` there would never `starts_with` a canonicalized repo root
/// even when it truly is inside it.
///
/// The unresolved tail is walked component-by-component (popping on `..`,
/// dropping `.`) rather than joined verbatim, because it does not exist yet
/// **and is about to**: `create_dir_all` will create every literal component
/// `git worktree add` is given, `..` included, and once that happens it
/// resolves — so a storage root of `/base/ghost/../Proj` reads as outside
/// `Proj` lexically right up until the moment `ghost` is created, at which
/// point it always was inside it. Comparing the raw, unnormalized tail would
/// miss that window entirely; this closes it by applying the same `..` a
/// created filesystem would.
///
/// Preconditions on `path`: absolute. Every caller today gets that from
/// upstream validation (`import_repo`/`CreateWorktreeBody::path` both check
/// `is_absolute`) — nothing here re-derives it, because `Path::ancestors`
/// on a relative path resolves against the daemon's own working directory,
/// silently pointing this at a location nobody chose.
fn canonicalize_prefix(path: &FsPath) -> PathBuf {
    debug_assert!(
        path.is_absolute(),
        "canonicalize_prefix needs an absolute path"
    );
    for ancestor in path.ancestors() {
        if let Ok(canon) = ancestor.canonicalize() {
            let suffix = path
                .strip_prefix(ancestor)
                .unwrap_or_else(|_| FsPath::new(""));
            let mut out = canon;
            for component in suffix.components() {
                match component {
                    std::path::Component::ParentDir => {
                        out.pop();
                    }
                    std::path::Component::CurDir => {}
                    other => out.push(other.as_os_str()),
                }
            }
            return out;
        }
    }
    path.to_path_buf()
}

/// Whether `checkout_path` would land inside `repo_root`'s own working tree —
/// see the caller in `create_worktree` for why that has to be refused rather
/// than merely unwise. Both sides are resolved through [`canonicalize_prefix`]
/// (`repo_root` always exists; `checkout_path` usually doesn't yet) so a
/// symlink on either side can't dodge the check.
fn checkout_inside_repo(checkout_path: &FsPath, repo_root: &FsPath) -> bool {
    canonicalize_prefix(checkout_path).starts_with(canonicalize_prefix(repo_root))
}

/// Open a directory in the OS file manager — mac's `open`, Linux's `xdg-open`
/// (Windows is not one of this daemon's supported targets today, so
/// `explorer.exe` has no branch here).
///
/// Spawned and left to run rather than awaited to completion. macOS `open`
/// really does hand off to Finder and exit immediately, but Linux
/// `xdg-open`'s generic fallback `exec`s the chosen handler directly and can
/// run for as long as that window stays open — awaiting it in full would
/// hold this HTTP request (and the button's spinner) open for just as long.
/// Reaped in the background instead, the same shape `open_terminal`
/// ([`crate::management`]) already uses for the terminal it launches — that
/// one also validates its path against the project registry first, which
/// this does not need to: it only ever runs on a value this daemon just read
/// from its own settings, never on one a caller supplied.
///
/// A short grace window still watches for an *immediate* exit — no
/// `DISPLAY`/`WAYLAND_DISPLAY`, no handler registered for `xdg-open` — so
/// that "the process launched" and "the folder actually opened" stay two
/// different claims. Long enough for an exec failure, nowhere near long
/// enough to matter for a real GUI launch.
async fn open_in_file_manager(dir: &FsPath) -> Result<(), String> {
    let path_env = cached_user_path().await;
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = tokio::process::Command::new(cmd)
        .arg(dir)
        .env("PATH", path_env)
        .spawn()
        .map_err(|e| format!("failed to run {cmd}: {e}"))?;
    match tokio::time::timeout(std::time::Duration::from_millis(500), child.wait()).await {
        Ok(Ok(status)) if !status.success() => Err(format!("{cmd} exited with {status}")),
        Ok(Ok(_)) => Ok(()), // exited quickly and cleanly — macOS `open`'s normal case
        Ok(Err(e)) => Err(format!("failed to wait on {cmd}: {e}")),
        Err(_) => {
            // Still running past the grace window: a real GUI session, or
            // `xdg-open`'s generic exec fallback. Reap it in the background
            // rather than the caller waiting on it, per the doc comment above.
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            Ok(())
        }
    }
}

/// Open the *effective* worktree storage directory in the OS file manager —
/// the settings dialog's Open Folder button.
///
/// Takes no path from the caller: it reads [`Db::worktree_storage_dir`]
/// itself, the same value `create_worktree` acts on, rather than trusting a
/// client-supplied path. That value is `None` in the default "next to each
/// repository" mode — there is no single directory to open in that mode, a
/// worktree's own folder is what "reveal in file manager" on its context menu
/// is for — so this 409s there instead of guessing which repo's sibling
/// folder was meant.
///
/// Also 404s if the configured directory does not exist (or is not a
/// directory) yet: the validator that accepts `worktree.storageDir`
/// deliberately does not require it to exist — an unmounted volume must stay
/// a savable value — so this is the one place that distinction has to be
/// checked before doing something with it, rather than handing `open` a path
/// it will refuse, or a file it will happily launch.
async fn open_worktree_storage_dir() -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    let dir = db.worktree_storage_dir().ok_or_else(|| {
        err(
            StatusCode::CONFLICT,
            "no custom worktree storage directory is configured",
        )
    })?;
    if !dir.is_dir() {
        return Err(err(
            StatusCode::NOT_FOUND,
            format!("{} does not exist or is not a directory", dir.display()),
        ));
    }
    open_in_file_manager(&dir)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Error shape
// ---------------------------------------------------------------------------

/// JSON error body: worktree/git failures carry real diagnostics ("branch
/// already checked out at …") the UI must surface, unlike the bare status
/// codes of the older management endpoints.
pub(crate) type ApiError = (StatusCode, Json<serde_json::Value>);

pub(crate) fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    (code, Json(serde_json::json!({ "error": msg.into() })))
}

/// **This is also where a damaged database gets noticed**, and the `Any` bound is
/// what makes that free rather than a discipline.
///
/// Every desktop handler already funnels its database failures through here, so
/// classifying the error at this one point means no call site has to remember to
/// report a fault — and the next handler somebody writes inherits it. A
/// `DbError` downcasts and is offered to [`crate::dbhealth`]; the handful of
/// call sites that pass a bare message (`db_err("repo vanished after import")`)
/// downcast to nothing and are unaffected.
///
/// The alternative — a second `db_err_typed` used at the ~50 sites where the
/// concrete type is known — was rejected: the two would drift the moment anyone
/// reached for the wrong one, and nothing would say so. This was the failure
/// mode in the first place: 265 database errors passed through this exact
/// function during the incident and every one of them was logged and forgotten.
pub(crate) fn db_err(e: impl std::fmt::Display + std::any::Any) -> ApiError {
    if let Some(db_error) = (&e as &dyn std::any::Any).downcast_ref::<veld_core::db::DbError>() {
        crate::dbhealth::note_error(db_error);
    }
    warn!("desktop api database error: {e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
}

/// Like [`db_err`], but reports a rejected value as the client error it is.
///
/// The handlers validate before writing, so `InvalidEmoji` shouldn't surface
/// here — but that makes the handler-side check look redundant, and deleting
/// it would silently downgrade a helpful 400 into a "database error" 500.
/// This keeps the DB-layer rejection honest either way.
fn write_err(e: veld_core::db::DbError) -> ApiError {
    match e {
        // Fixed message, value only to the log: the variant's Display
        // Debug-formats the rejected string, and echoing unbounded
        // client-supplied input back into a response body is a habit worth
        // not starting.
        veld_core::db::DbError::InvalidEmoji(_) => {
            warn!("rejected worktree emoji: {e}");
            err(
                StatusCode::BAD_REQUEST,
                "emoji must be one of the curated worktree glyphs",
            )
        }
        // Same posture as the emoji arm: the handler pre-checks, and this keeps
        // the DB-layer rejection from degrading into a 500 if that check is ever
        // deleted as redundant. Fixed message, value only to the log — the same
        // habit the emoji arm keeps about echoing client input.
        veld_core::db::DbError::InvalidColor(_) => {
            warn!("rejected worktree marker colour: {e}");
            err(
                StatusCode::BAD_REQUEST,
                "marker_color must be a lowercase #rrggbb colour",
            )
        }
        // On the PATCH path there is no handler pre-check: the DB layer
        // resolves the collision inside a transaction so two concurrent renames
        // can't both win. (`create_worktree` does pre-check, to avoid creating
        // a checkout it would then reject — this arm is what catches losing
        // that race.) Echoing the alias is safe, unlike the emoji case:
        // `validate_alias` has already bounded it to 1-64 identifier
        // characters, and the UI needs to say *which* alias is taken.
        veld_core::db::DbError::AliasTaken(ref alias) => err(
            StatusCode::CONFLICT,
            format!("another checkout of this repo is already called \"{alias}\""),
        ),
        // The main-checkout refusal lives in the DB layer (`trash_worktree`) rather
        // than in this handler, so every path that can bin a worktree inherits it
        // instead of each one having to remember. Keeping the 400 here preserves the
        // status the UI already handles.
        veld_core::db::DbError::RefusingMainWorktree => err(
            StatusCode::BAD_REQUEST,
            "refusing to remove the main checkout",
        ),
        veld_core::db::DbError::UnknownLane(_) => {
            warn!("rejected lane assignment: {e}");
            err(StatusCode::BAD_REQUEST, "no such lane in this repo")
        }
        veld_core::db::DbError::OrderTooLong(_) => {
            warn!("rejected oversized reorder: {e}");
            err(
                StatusCode::BAD_REQUEST,
                format!(
                    "a reorder may list at most {} entries",
                    veld_core::db::MAX_ORDER_LEN
                ),
            )
        }
        other => db_err(other),
    }
}

pub(crate) fn open_desktop_db() -> Result<Db, ApiError> {
    open_db().map_err(|code| err(code, "failed to open the veld database"))
}

// ---------------------------------------------------------------------------
// Git plumbing
// ---------------------------------------------------------------------------

/// Run `git -C <dir> <args…>` with the user's login-shell PATH and return the
/// raw stdout bytes, **untrimmed**. Trimming is done by [`git`] for callers that
/// want a clean line; this raw form exists for [`git_status`], whose porcelain
/// codes carry a significant leading space (` M` is an unstaged edit) that a
/// `.trim()` would silently destroy — which is exactly the bug that shipped
/// when `git_status` used [`git`] and plain edits went undetected.
async fn git_raw(dir: &FsPath, args: &[&str]) -> Result<Vec<u8>, String> {
    git_raw_with_index(dir, None, args).await
}

/// [`git_raw`], plus the option of pointing git at an index file that is not
/// the checkout's own.
///
/// `GIT_INDEX_FILE` is the only way to stage into something other than the
/// working checkout's index, and staging into a scratch copy is how
/// [`capture_uncommitted`] learns what `git add` *would* do without doing it
/// to a checkout somebody is working in.
async fn git_raw_with_index(
    dir: &FsPath,
    index: Option<&FsPath>,
    args: &[&str],
) -> Result<Vec<u8>, String> {
    let path_env = cached_user_path().await;
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("PATH", path_env)
        // **`GIT_DIR` beats `-C`.** These four are inherited, and the daemon is
        // auto-started by any `veld` CLI call — including one made from inside a
        // hook, a `git rebase --exec` or a `git bisect run`, where they point at
        // whatever repository git was operating on. An inherited pair would aim
        // every git call here at a checkout nobody named, and the worst of them
        // is `apply_captured`'s `read-tree -u --reset`, which writes: the
        // destination would get nothing while a `git status` through this same
        // wrapper reported a plausible count from the hijacked checkout.
        // `veld_core::project_id` strips the same four for the same reason. The
        // directory is the only input this should have — plus `GIT_INDEX_FILE`
        // where a caller asks for it *below*, which is why the removal comes
        // first.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE");
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run git: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("git {} failed with {}", args.join(" "), output.status)
        } else {
            stderr
        })
    }
}

/// Run `git -C <dir> <args…>` with the user's login-shell PATH. Returns
/// trimmed stdout, or the trimmed stderr as the error message.
pub(super) async fn git(dir: &FsPath, args: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&git_raw(dir, args).await?)
        .trim()
        .to_string())
}

/// [`git`], run against a scratch index file. See [`git_raw_with_index`].
async fn git_with_index(dir: &FsPath, index: &FsPath, args: &[&str]) -> Result<String, String> {
    Ok(
        String::from_utf8_lossy(&git_raw_with_index(dir, Some(index), args).await?)
            .trim()
            .to_string(),
    )
}

/// One file that stops `git worktree remove` from succeeding, as reported by
/// [`git_status`].
///
/// The two fields are the two things the UI needs to let the user decide:
/// *what* is in the way (`path`) and *why* (`kind`). `kind` is a stable
/// short label, not the raw porcelain code, so the client renders "modified"
/// rather than decoding `" M"` itself — and so a future porcelain code that
/// appears in the wild cannot silently become an empty label.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct DirtyFile {
    /// Path relative to the worktree root.
    path: String,
    /// Human label: `modified`, `untracked`, `deleted`, `added`, `renamed`,
    /// `copied`, `conflicted`, or `changed` for anything unclassified.
    kind: &'static str,
}

/// The dirty state of a worktree, as the trash/delete flow needs it.
#[derive(Debug, Serialize)]
struct StatusView {
    /// Whether `git worktree remove` would refuse this checkout right now.
    dirty: bool,
    /// The files in the way, in git's own (path) order. Empty when `dirty` is
    /// false.
    files: Vec<DirtyFile>,
}

/// Map a porcelain v1 status pair to the label [`DirtyFile::kind`] carries.
///
/// The porcelain codes are `<index><worktree>`; either side being non-space is
/// a change, and untracked is the literal `??`. Unmerged/conflicted codes
/// (`DD`, `AU`, `UD`, `UU`, …) all carry a `U` in one of the two positions, so
/// they are caught before the single-letter tests below.
fn dirty_kind(code: &str) -> &'static str {
    let b = code.as_bytes();
    if code == "??" {
        return "untracked";
    }
    // Unmerged/conflicted states. Most carry a `U`, but the `AA` (both added)
    // and `DD` (both deleted) unmerged codes do not — so the literal pair must
    // be matched too, not just the `U` presence.
    let any = |c: u8| b[0] == c || b[1] == c;
    if any(b'U') || code == "AA" || code == "DD" {
        return "conflicted";
    }
    // Rename/copy before the single-letter tests: a staged rename that is also
    // modified (`RM`) should read "renamed", not "modified" — the move is the
    // story the file list is telling.
    if any(b'R') {
        return "renamed";
    }
    if any(b'C') {
        return "copied";
    }
    if any(b'M') {
        return "modified";
    }
    if any(b'A') {
        return "added";
    }
    if any(b'D') {
        return "deleted";
    }
    "changed"
}

/// Parse `git status --porcelain=v1 -z` output into a file list.
///
/// The `-z` form is NUL-delimited and path-safe (no quoting, no mangled
/// spaces or newlines), which is why it is chosen over the line-oriented
/// `--porcelain` for something that renders paths back to a human. Each record
/// is `<XY> <path>\0`; a rename or copy adds a trailing `<original>\0` (the
/// origin), which is not a record of its own and must be skipped.
///
/// The paths reported are the *destination* paths, exactly as `git worktree
/// remove` would refuse them — this list is the set of files that are in the
/// way, not a diff summary, so showing the dest for a rename is the honest
/// answer to "what would be discarded?"
fn parse_git_status(porcelain: &str) -> Vec<DirtyFile> {
    let mut files = Vec::new();
    let mut i = 0usize;
    let fields: Vec<&str> = porcelain.split('\0').collect();
    while i < fields.len() {
        let f = fields[i];
        i += 1;
        if f.is_empty() {
            continue;
        }
        // The record is `<XY> <path>` where `XY` is two status characters and
        // **the first of them may itself be a space** (e.g. `" M a.txt"`), so
        // the separator is at byte 2, not the first space. Splitting on the
        // first space would read `" M a.txt"` as an empty code and a path of
        // `"M a.txt"` — dropping every leading-space code.
        if f.len() < 4 || f.as_bytes()[2] != b' ' {
            continue;
        }
        let code = &f[..2];
        let path = &f[3..];
        if path.is_empty() {
            continue;
        }
        files.push(DirtyFile {
            path: path.to_string(),
            kind: dirty_kind(code),
        });
        // A rename/copy record is followed by its `<original>` as its own
        // NUL-delimited field with no `<XY> ` prefix; skip it so it is not
        // rendered as a second file.
        //
        // **Both columns, and the index column is the one that matters.** Git
        // detects renames against the index, so a staged rename's code is
        // `R ` — the marker sits in `X` (byte 0) and byte 1 is a *space*. This
        // tested byte 1 alone, so the skip never fired for the only shape that
        // produces the extra field. It went unnoticed because the malformed-
        // record guard above rejects most origin paths by accident, and the
        // condition is exact: that guard requires a space at **byte 2**, so
        // only an origin path whose *third character* is a space gets through
        // and is reported as a file that does not exist. `PR review notes.md`
        // becomes a file called `review notes.md` with a status code of `PR`;
        // `01 Track.mp3` becomes `Track.mp3` with code `01`. Two-character
        // prefixes, which is what makes this reachable rather than
        // theoretical — and note that a *longer* prefix does not qualify:
        // `IMG 1234.jpg` has `G` at byte 2, so the guard rejects it and no
        // phantom appears. Pinned by
        // `a_rename_origin_is_never_reported_as_a_file_of_its_own`.
        let marks_origin = |c: u8| c == b'R' || c == b'C';
        if marks_origin(code.as_bytes()[0]) || marks_origin(code.as_bytes()[1]) {
            i += 1;
        }
    }
    files
}

/// Run `git status --porcelain=v1 -z` in a directory and parse the result.
///
/// Returns an empty list for a clean worktree. An error means git could not
/// run at all (a missing checkout, a broken repo), which the caller surfaces
/// rather than guessing at the dirty state.
async fn git_status(dir: &FsPath) -> Result<Vec<DirtyFile>, String> {
    // Via `git_raw`, not `git`: the porcelain codes for unstaged changes begin
    // with a space (` M`), and the shared helper's `.trim()` would strip it,
    // turning an unstaged edit into an empty-looking record. This is the bug
    // the integration test below pins.
    let out = git_raw(dir, &["status", "--porcelain=v1", "-z"]).await?;
    Ok(parse_git_status(&String::from_utf8_lossy(&out)))
}

// ---------------------------------------------------------------------------
// Per-worktree git signals ("is this checkout used?")
// ---------------------------------------------------------------------------

/// What git says about one checkout, for the rail's at-a-glance glyph.
///
/// **Independent facts, never a glyph.** The daemon does not send a
/// `state: "dirty" | "unpushed" | "clean"` because the fourth and fifth states
/// (conflicted, mid-rebase, detached, behind) will arrive, and an enum users have
/// built habits on is the expensive thing to change. Folding these into one glyph
/// is the client's job — see `rowstate/rowState.ts` in the UI, which also decides
/// when the activity glyph outranks any of this and takes the row's one slot.
///
/// Every field is optional and `None` means *not known*, which is deliberately a
/// different fact from a zero or a `false`: a checkout whose volume is unmounted, a
/// repo with no remote, and a worktree the dirty sweep has not reached yet all
/// render as no glyph rather than as "clean". `carried_file_count` above made the
/// same distinction for the same reason, and collapsing it was the bug there too.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
struct WorktreeGitSignals {
    /// `git status --porcelain` is non-empty — staged, unstaged or untracked work
    /// that exists in this checkout and nowhere else.
    ///
    /// Measured by the sweep, not on the request path, and dropped once it is older
    /// than [`DIRTY_MAX_AGE`] so a wedged sweep shows nothing rather than an answer
    /// from five minutes ago.
    dirty: Option<bool>,
    /// The branch's upstream as git spells it (`origin/feat-x`), or `None` when the
    /// branch has none — never pushed, or a detached HEAD.
    upstream: Option<String>,
    /// Commits this branch has that its upstream does not: the "not pushed yet"
    /// half of the question. `None` when there is no upstream to compare against.
    ///
    /// Note what this counts for a branch whose upstream is `origin/main` rather
    /// than `origin/<itself>` — a worktree created from the default branch and
    /// never pushed. Then `ahead` is the branch's own commits, which is the same
    /// answer by a different route, and the one the reader wants either way.
    ahead: Option<i64>,
    /// The mirror image, carried because it costs nothing — it comes out of the
    /// same `for-each-ref` field as `ahead`. No glyph renders it today: the repo's
    /// staleness pill in the top bar already answers "behind" for the main
    /// checkout, and a second amber signal per row was not asked for. It is on the
    /// wire so that adding one later is a UI change and not a protocol change.
    behind: Option<i64>,
    /// The branch has an upstream configured whose remote-tracking ref is **gone** —
    /// git's own `[gone]`, which appears once `fetch --prune` has seen the remote
    /// branch disappear.
    ///
    /// **No glyph renders this, and it is still worth sending.** A merged mark was
    /// built for it and removed on maintainer instruction: a squash-merge-and-delete
    /// leaves exactly this state, and so does a pull request closed without merging
    /// and then deleted, so the mark would be confidently wrong about the one thing
    /// a reader wants it for. Real pull-request state holds a PR number and belongs
    /// to an `ide.extensions` badge — this repo's own top-bar one already does it
    /// with `gh`.
    ///
    /// The UI reads it as a **guard** instead: without it a merged-and-tidied
    /// checkout looks clean with an upstream and is reported as "everything is
    /// pushed" to a remote branch that no longer exists. It also still reaches the
    /// tooltip, which is where a sentence may be probabilistic where a glyph may
    /// not.
    upstream_gone: bool,
}

impl WorktreeGitSignals {
    /// Whether there is anything here worth sending.
    ///
    /// An all-`None` value is what a worktree looks like before the sweep has run
    /// and after every probe failed, and the two are the same to a client: no
    /// glyph. Sending `null` instead of a struct of nulls keeps that unambiguous
    /// on the wire.
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How old a swept `dirty` answer may be before it is served as `None`.
///
/// The sweep is kicked by the UI's own poll, so in normal use an entry is at most
/// [`DIRTY_REFRESH_AGE`] plus one sweep old. This is the backstop for the abnormal
/// case — a checkout on a network filesystem that takes a minute to answer, a
/// spawn that never returns — where the failure mode worth avoiding is a confident
/// glyph nobody is refreshing. Six times the refresh age, so an ordinary slow
/// sweep does not make rows blink.
const DIRTY_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(90);

/// How old an entry must be before the sweep recomputes it.
///
/// Not a poll interval: the UI polls every 5s and this is what stops that becoming
/// a `git status` in every checkout every 5s, which is the cost
/// [`worktree_status`] was written to avoid putting on the listing. Measured on
/// this repo, `git --no-optional-locks status --porcelain=v1 -z` is 16ms in a warm
/// worktree and 155ms in a cold one with a large ignored `target/`, so 15s over 18
/// worktrees is a few percent of one core rather than a busy loop.
const DIRTY_REFRESH_AGE: std::time::Duration = std::time::Duration::from_secs(15);

/// How many `git status` children the sweep runs at once.
///
/// The same reasoning — and the same number — as the delete dialog's bounded
/// worker: each one runs git in a checkout on a machine already running this
/// project's dev servers, so the sweep is deliberately not `join_all` over 18
/// worktrees.
const DIRTY_CONCURRENCY: usize = 4;

/// `ahead`/`behind`/`gone` per worktree, keyed by worktree id.
///
/// Refreshed **synchronously** on each [`refresh_repos`] poll, because it costs one
/// `for-each-ref` per *repo* — 21ms measured, whatever the worktree count — and a
/// glyph that appears a poll late for no reason is worse than one that costs 21ms.
static UPSTREAMS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<i64, WorktreeGitSignals>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// `dirty` per worktree, keyed by worktree id, with the instant it was measured.
///
/// Refreshed **asynchronously** by [`spawn_dirty_sweep`], because it costs a
/// `git status` per *worktree* and the poll must not wait for eighteen of them.
static DIRTY: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<i64, (bool, std::time::Instant)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// One `for-each-ref` per repo: every local branch's upstream and how it tracks.
///
/// **One process for the whole repo, and it never enters a working tree.** It reads
/// refs and walks the object graph, so it does not care whether a worktree's volume
/// is mounted, does not touch an index, and does not contend with the developer's
/// own git — which is what makes the push half of the glyph affordable on a 5s poll
/// when the dirty half is not.
///
/// Keyed by **worktree path**, from `%(worktreepath)`, so the caller does not have
/// to match branch names itself. A branch checked out nowhere reports an empty
/// path and is skipped: it has no row in the rail.
///
/// Tab-delimited with a NUL record separator, the same shape [`list_branches`] uses.
/// Safe because `git check-ref-format` refuses control characters in a refname, so
/// neither a branch name nor an upstream name can contain either separator;
/// `%(upstream:track,nobracket)` is the one field with punctuation in it
/// (`ahead 1, behind 2`) and it contains neither.
///
/// This is a second `for-each-ref` rather than a column added to
/// [`list_branches`]'s. That one answers "what may I check out" for the branch
/// picker, on demand and for branches with no worktree at all; this one answers
/// "how does each checked-out branch stand" on every poll. Merging them would make
/// the picker pay for tracking information it does not render, and this pay for
/// rows it discards.
async fn repo_upstreams(
    repo_root: &FsPath,
) -> std::collections::HashMap<String, WorktreeGitSignals> {
    let mut out = std::collections::HashMap::new();
    let Ok(raw) = git(
        repo_root,
        &[
            "for-each-ref",
            "--format=%(worktreepath)%09%(upstream:short)%09%(upstream:track,nobracket)%00",
            "refs/heads",
        ],
    )
    .await
    else {
        return out;
    };
    for record in raw.split('\0') {
        let record = record.trim_start_matches('\n');
        if record.trim().is_empty() {
            continue;
        }
        let mut fields = record.split('\t');
        let (Some(path), Some(upstream), Some(track)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        out.insert(path.to_owned(), parse_upstream_track(upstream, track));
    }
    out
}

/// Turn `%(upstream:short)` + `%(upstream:track,nobracket)` into the signal fields.
///
/// The four shapes git emits, and the one distinction that has to survive:
///
/// | `upstream` | `track` | means |
/// |---|---|---|
/// | `""` | `""` | no upstream configured — never pushed, or detached |
/// | `origin/x` | `""` | in sync |
/// | `origin/x` | `ahead 1`, `behind 2`, `ahead 1, behind 2` | as it says |
/// | `origin/x` | `gone` | configured, but the remote-tracking ref is gone |
///
/// **`""` and `gone` must not collapse into each other**, and neither may collapse
/// into "in sync": "never pushed" and "pushed, then deleted upstream" are opposite
/// facts about whether the work got anywhere, and both come back with no counts.
/// That is why `ahead`/`behind` are `None` rather than `0` for a branch with no
/// upstream — there is nothing to be zero commits away from.
fn parse_upstream_track(upstream: &str, track: &str) -> WorktreeGitSignals {
    if upstream.is_empty() {
        return WorktreeGitSignals::default();
    }
    let mut signals = WorktreeGitSignals {
        upstream: Some(upstream.to_owned()),
        ..WorktreeGitSignals::default()
    };
    if track == "gone" {
        signals.upstream_gone = true;
        return signals;
    }
    // An upstream that exists is comparable, so the counts are `Some` even when
    // they are zero — the empty `track` of a branch in sync means "0 and 0", which
    // is a real answer and not a missing one.
    signals.ahead = Some(0);
    signals.behind = Some(0);
    for part in track.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            signals.ahead = n.parse().ok();
        } else if let Some(n) = part.strip_prefix("behind ") {
            signals.behind = n.parse().ok();
        }
    }
    signals
}

/// Refresh [`UPSTREAMS`] for one repo, from rows the caller already has.
///
/// Entries for this repo's worktrees are replaced wholesale rather than merged, so
/// a branch that lost its upstream, or a worktree that went away, does not leave a
/// stale answer behind. Other repos' entries are untouched: the map is global and
/// this runs once per repo per poll.
async fn refresh_upstreams(repo_root: &FsPath, worktrees: &[WorktreeRecord]) {
    let by_path = repo_upstreams(repo_root).await;
    let mut cache = UPSTREAMS.lock().expect("upstream cache mutex poisoned");
    for wt in worktrees {
        match by_path.get(&wt.path) {
            Some(signals) => cache.insert(wt.id, signals.clone()),
            None => cache.remove(&wt.id),
        };
    }
}

/// Recompute `dirty` for whichever of these worktrees has the stalest answer.
///
/// **Fire-and-forget, by design.** The poll returns whatever [`DIRTY`] already
/// holds and this fills it in for the next one, so a rail with eighteen rows never
/// makes the request that draws it wait on eighteen `git status` children. The
/// glyph therefore appears about one poll after an IDE window opens, which is the
/// price of the listing never getting slower — the trade
/// [`worktree_status`]' doc comment describes from the other side.
///
/// Nothing runs while no window is open, because the poll is what kicks this. That
/// is the same property the extension-badge RPC has, reached the same way, and it
/// is why this is not a daemon-owned interval task.
///
/// Trashed worktrees are skipped: their row shows restore/delete controls, not
/// state, and a checkout mid-`git worktree remove` is the one place a `git status`
/// races something destructive.
fn spawn_dirty_sweep(targets: Vec<(i64, String)>) {
    /// One sweep at a time, however many windows are polling. Concurrent windows
    /// collapse onto the running sweep instead of multiplying its git spawns —
    /// the reason `LAST_SYNC` debounces the reconcile beside it.
    static SWEEPING: std::sync::LazyLock<std::sync::Mutex<bool>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(false));

    let due: Vec<(i64, String)> = {
        let cache = DIRTY.lock().expect("dirty cache mutex poisoned");
        targets
            .iter()
            .filter(|(id, _)| {
                cache
                    .get(id)
                    .is_none_or(|(_, at)| at.elapsed() >= DIRTY_REFRESH_AGE)
            })
            .cloned()
            .collect()
    };
    // Prune ids that are no longer registered anywhere in this poll's targets, so
    // a long-lived daemon does not accumulate a row per worktree ever seen.
    {
        let live: std::collections::HashSet<i64> = targets.iter().map(|(id, _)| *id).collect();
        DIRTY
            .lock()
            .expect("dirty cache mutex poisoned")
            .retain(|id, _| live.contains(id));
        UPSTREAMS
            .lock()
            .expect("upstream cache mutex poisoned")
            .retain(|id, _| live.contains(id));
    }
    if due.is_empty() {
        return;
    }
    {
        let mut sweeping = SWEEPING.lock().expect("dirty sweep mutex poisoned");
        if *sweeping {
            return;
        }
        *sweeping = true;
    }
    tokio::spawn(async move {
        for chunk in due.chunks(DIRTY_CONCURRENCY) {
            let measured = futures_util::future::join_all(
                chunk
                    .iter()
                    .map(|(id, path)| async move { (*id, git_is_dirty(FsPath::new(path)).await) }),
            )
            .await;
            let mut cache = DIRTY.lock().expect("dirty cache mutex poisoned");
            for (id, dirty) in measured {
                match dirty {
                    Some(dirty) => cache.insert(id, (dirty, std::time::Instant::now())),
                    // A checkout that could not be read at all — an unmounted
                    // volume, a `.git` mid-surgery — loses its entry rather than
                    // keeping the last answer. The row then shows no glyph, which
                    // is what "we do not know" has to look like.
                    None => cache.remove(&id),
                };
            }
        }
        *SWEEPING.lock().expect("dirty sweep mutex poisoned") = false;
    });
}

/// Whether one checkout has uncommitted work. `None` if git could not answer.
///
/// **`--no-optional-locks` is load-bearing, not tidiness.** Without it `git status`
/// refreshes and rewrites `.git/index`, and this runs unprompted in every
/// registered checkout: it would contend for `index.lock` with the developer's own
/// git, and turn the daemon's own read into a filesystem event that a watcher in
/// their dev server then rebuilds on. Verified on git 2.50.1 — with the flag, the
/// index's mtime and size are unchanged across a run.
///
/// Not [`git_status`], which parses the same porcelain into a file list for the
/// delete dialog. This wants one bit, so it never parses records — but it asks the
/// same question, at git's default `-unormal`, because an untracked file *is* work
/// that exists only in this checkout and is exactly what `git worktree remove`
/// refuses on. Any answer narrower than that would have the glyph disagree with the
/// dialog that blocks the delete.
async fn git_is_dirty(dir: &FsPath) -> Option<bool> {
    // `git_raw`, not `git`, for the reason `git_status` uses it: a porcelain code
    // carries a significant leading space and this asks only whether there is a
    // record at all, so trimming would answer `false` for an unstaged edit.
    let out = git_raw(
        dir,
        &["--no-optional-locks", "status", "--porcelain=v1", "-z"],
    )
    .await
    .ok()?;
    Some(!out.iter().all(|b| *b == 0 || b.is_ascii_whitespace()))
}

/// The cached signals for one worktree, or `None` when nothing is known.
///
/// Read from [`worktree_view`], which is why this takes an id and not a path: the
/// listing is built from database rows and must not spawn git — the halves that do
/// are [`refresh_upstreams`] (on the poll) and [`spawn_dirty_sweep`] (behind it).
fn git_signals_for(id: i64) -> Option<WorktreeGitSignals> {
    let mut signals = UPSTREAMS
        .lock()
        .expect("upstream cache mutex poisoned")
        .get(&id)
        .cloned()
        .unwrap_or_default();
    if let Some((dirty, at)) = DIRTY.lock().expect("dirty cache mutex poisoned").get(&id) {
        if at.elapsed() < DIRTY_MAX_AGE {
            signals.dirty = Some(*dirty);
        }
    }
    (!signals.is_empty()).then_some(signals)
}

/// Parse `git worktree list --porcelain` output. The first entry is the main
/// checkout. Detached checkouts get the branch label `(detached)`; bare
/// entries are skipped (nothing to open or run there).
///
/// **`prunable` entries are skipped too**, which is a correctness requirement and
/// not a tidy-up. Git keeps a worktree's administrative entry under
/// `.git/worktrees/<n>/` after the checkout itself is gone, and reports it with a
/// `prunable <reason>` line (e.g. "gitdir file points to non-existent location")
/// until `git worktree prune` runs — whose default expiry is
/// `gc.worktreePruneExpire`, **three months**. Treating such an entry as
/// discovered means `sync_worktrees` sees the path and keeps the row alive, so a
/// worktree deleted outside veld (`rm -rf`, a `git worktree move`, a wiped
/// scratch disk) stayed in the rail indefinitely pointing at nothing. Skipping it
/// lets the existing `path NOT IN (…)` delete reap the row on the next poll.
///
/// **The cost, stated because it is not free.** Reaping the row also discards the
/// user state on it — alias, marker, lane, manual position — and git reports
/// `prunable` for *any* absent checkout, including one on an unmounted external or
/// network volume, which is transient. So a worktree on a disk that is currently
/// unmounted comes back re-registered with a fresh alias and marker and no lane.
/// A grace period (`missing_since`, reap only after N hours) is the fix that serves
/// both cases and is deliberately not in this change: it puts a clock in the
/// reconcile pass, and the pass having exactly one new branch is what makes it
/// reviewable. Note the repo-level case is already covered — if the *repo root*
/// is unreachable, `git worktree list` fails, [`discover_worktrees`] returns
/// `Err`, and `RepoView.available` goes false with every row left untouched.
/// (It was `sync_repo_worktrees` that answered this before the two halves were
/// split; that function's own `Err` is a *database* failure, which is now
/// deliberately not what `available` reports.)
fn parse_worktree_list(porcelain: &str) -> Vec<DiscoveredWorktree> {
    let mut out = Vec::new();
    let mut first = true;
    for block in porcelain.split("\n\n") {
        let mut path: Option<&str> = None;
        let mut branch: Option<&str> = None;
        let mut bare = false;
        let mut detached = false;
        let mut prunable = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p);
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b));
            } else if line == "bare" {
                bare = true;
            } else if line == "detached" {
                detached = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                prunable = true;
            }
        }
        let Some(path) = path else { continue };
        // `is_main` is consumed before the skips so that a bare or prunable first
        // block does not promote the next worktree to main.
        let is_main = std::mem::take(&mut first);
        if bare || prunable {
            continue;
        }
        let branch = if detached {
            "(detached)".to_string()
        } else {
            branch.unwrap_or("(unknown)").to_string()
        };
        out.push(DiscoveredWorktree {
            path: path.to_string(),
            branch,
            is_main,
        });
    }
    out
}

/// Canonicalize discovered worktree paths before storing them. Git porcelain
/// already emits physical (symlink-resolved) paths, and `veld start` derives
/// the project root from `getcwd` (also physical) — canonicalizing here keeps
/// the UI's join key (`worktrees.path` == `projects.root`, string equality)
/// stable even when git reports a path through a symlink. Falls back to the
/// raw path when canonicalization fails (e.g. checkout vanished mid-sync).
fn canonicalize_discovered(mut discovered: Vec<DiscoveredWorktree>) -> Vec<DiscoveredWorktree> {
    for d in &mut discovered {
        if let Ok(p) = std::fs::canonicalize(&d.path) {
            d.path = p.to_string_lossy().into_owned();
        }
    }
    discovered
}

/// Discover a repo's worktrees on disk and reconcile the database rows.
async fn sync_repo_worktrees(db: &Db, repo_root: &FsPath) -> Result<Vec<WorktreeRecord>, ApiError> {
    let discovered = discover_worktrees(repo_root).await?;
    db.sync_worktrees(repo_root, &discovered).map_err(db_err)
}

/// Ask git what checkouts this repo has. **Takes a path and nothing else**, and
/// that is load-bearing rather than tidy.
///
/// This is the half of [`sync_repo_worktrees`] that answers "can this repo be
/// operated on", and it is split out so that the answer has no access to a
/// database result to be contaminated by. The two used to be one call whose
/// `Result` was collapsed with `.is_ok()` in [`refresh_repos`], which meant a
/// damaged SQLite page — in an unrelated table, reached only through a foreign
/// key cascade — rendered as `repository unavailable` over a repo sitting right
/// there on disk, with the start/stop controls hidden and the real error thrown
/// away. Two functions with disjoint consumers is what stops that being
/// re-collapsed by the next person in a hurry.
async fn discover_worktrees(repo_root: &FsPath) -> Result<Vec<DiscoveredWorktree>, ApiError> {
    let porcelain = git(repo_root, &["worktree", "list", "--porcelain"])
        .await
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    Ok(canonicalize_discovered(parse_worktree_list(&porcelain)))
}

/// One local branch of a repo, as the create dialog's source picker needs it.
///
/// `checked_out_in` is the load-bearing field: git refuses `git worktree add`
/// for a branch that is already checked out in another worktree, so a picker
/// that cannot say which branches are taken offers choices that fail on click.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct LocalBranchView {
    /// Short name (`feat/x`), exactly as git has it — never slugged, since it
    /// is passed straight back as a ref.
    name: String,
    /// The checkout that currently holds it, or `None` when the branch is free.
    checked_out_in: Option<String>,
    /// `origin/feat/x`, or `None` for a branch that tracks nothing.
    upstream: Option<String>,
}

/// One remote-tracking branch, as the create dialog's source picker needs it.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct RemoteBranchView {
    /// `origin/feat/x` — what a `git worktree add` start-point argument names.
    name: String,
    /// `feat/x` — the local branch name the dialog offers by default. The
    /// remote's own name is stripped, because a local branch called
    /// `origin/feat/x` is legal, confusing, and never what was meant.
    local_name: String,
    /// Whether a local branch called `local_name` already exists. Such a
    /// branch cannot be *created* from the remote ref, so the dialog steers
    /// the user to the local-branch source instead of letting the create fail.
    has_local: bool,
}

/// The branches a repo can produce a worktree from.
#[derive(Debug, Serialize)]
struct BranchesView {
    local: Vec<LocalBranchView>,
    remote: Vec<RemoteBranchView>,
}

/// Parse `git for-each-ref refs/heads` in the format
/// `<short>\t<upstream:short>\t<worktreepath>`.
///
/// **Records are NUL-terminated and fields are tab-separated, and both halves
/// are about the worktree path.** A refname cannot contain a tab or a newline
/// (`git check-ref-format` rejects every ASCII control character) but a
/// *filesystem path* can hold either — so the record separator is `%00` rather
/// than the newline `for-each-ref` would otherwise end each record with, and
/// the path is the last field so `splitn(3, …)` leaves whatever it holds
/// intact. Splitting records on lines was the first version, and a worktree at
/// a path containing a newline produced a phantom branch row named after the
/// path's second half, plus a real branch reported as checked out at the
/// first half.
fn parse_local_branches(out: &str) -> Vec<LocalBranchView> {
    out.split('\0')
        // `for-each-ref` still ends each record with its own newline *after*
        // the `%00`, so every record but the first arrives with a leading one.
        // Trimmed from the start only — a refname cannot begin with a newline,
        // and trimming the end would eat a path that does.
        .map(|r| r.trim_start_matches('\n'))
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let name = fields.next()?;
            if name.is_empty() {
                return None;
            }
            let upstream = fields.next().unwrap_or("");
            let worktree = fields.next().unwrap_or("");
            Some(LocalBranchView {
                name: name.to_string(),
                checked_out_in: (!worktree.is_empty()).then(|| worktree.to_string()),
                upstream: (!upstream.is_empty()).then(|| upstream.to_string()),
            })
        })
        .collect()
}

/// Parse `git for-each-ref refs/remotes` in the format
/// `<short>\t<symref>`, NUL-terminated per record (see
/// [`parse_local_branches`] for why).
///
/// **Symbolic refs are skipped.** `refs/remotes/origin/HEAD` is a symref to the
/// remote's default branch and shortens to the bare remote name (`origin`),
/// which is not a branch anybody can check out — offering it would put a row
/// called "origin" in the picker that duplicates whatever `origin/main` already
/// is.
fn parse_remote_branches(out: &str, local: &[LocalBranchView]) -> Vec<RemoteBranchView> {
    out.split('\0')
        .map(|r| r.trim_start_matches('\n'))
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let (name, symref) = line.split_once('\t').unwrap_or((line, ""));
            if name.is_empty() || !symref.is_empty() {
                return None;
            }
            // `origin/feat/x` → `feat/x`. Only the first component is the
            // remote, so `split_once` and not `rsplit_once`: a branch called
            // `feat/x` under `origin` must not become `x`.
            let (_remote, local_name) = name.split_once('/')?;
            if local_name.is_empty() {
                return None;
            }
            Some(RemoteBranchView {
                name: name.to_string(),
                local_name: local_name.to_string(),
                has_local: local.iter().any(|b| b.name == local_name),
            })
        })
        .collect()
}

/// The branches a worktree can be created from: local, and remote-tracking.
///
/// A GET, and side-effect-free like every other GET on this router — in
/// particular **it does not fetch**. The remote-tracking refs it reports are as
/// fresh as the last fetch, which `refresh_repos` already performs at most once
/// a minute per repo while an IDE window is open (see `maybe_fetch`), so a
/// remote branch pushed in the last minute may not be listed yet. That is the
/// deliberate trade: a picker that fetched on open would spawn a network
/// operation from a read, and would do it again on every re-render.
async fn list_branches(Query(q): Query<RepoQuery>) -> Result<Json<BranchesView>, ApiError> {
    let repo_root = PathBuf::from(&q.repo_root);
    // Registered repos only, matching every other repo-scoped endpoint: this
    // spawns git in a caller-supplied directory, and the registry is what makes
    // that directory one the user already chose.
    let db = open_desktop_db()?;
    db.get_repo(&repo_root)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "repo not imported"))?;
    let heads = git(
        &repo_root,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(upstream:short)\t%(worktreepath)%00",
            "refs/heads",
        ],
    )
    .await
    .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let local = parse_local_branches(&heads);
    let remotes = git(
        &repo_root,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(symref)%00",
            "refs/remotes",
        ],
    )
    .await
    .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let remote = parse_remote_branches(&remotes, &local);
    Ok(Json(BranchesView { local, remote }))
}

// ---------------------------------------------------------------------------
// Carrying uncommitted work into a spin-off
// ---------------------------------------------------------------------------

/// A snapshot of one checkout's uncommitted state, as two git tree objects.
///
/// Two trees rather than a patch, because the pair is an exact model of what a
/// checkout can be in: `staged_tree` is the index, `full_tree` is the index
/// plus every unstaged edit, deletion and untracked file. Every combination
/// falls out of the pair correctly — a staged add that was then edited, a
/// deletion staged but the file still on disk, a mode flip — where a textual
/// diff has to reconstruct each of them and a filesystem copy cannot see the
/// index at all.
///
/// Both trees are built from **copies** of the source's index file, so the
/// capture writes nothing but shared objects. That is the constraint the whole
/// mechanism is shaped by: the source is somebody's live checkout, quite
/// possibly with an agent editing it, and the obvious alternative —
/// `git stash create`, which produces exactly this snapshot as one commit —
/// rewrites the source's index under `index.lock` even with
/// `--no-optional-locks` (measured, git 2.50). A spin-off must not be able to
/// make a concurrent `git add` in the source fail.
#[derive(Debug)]
struct CapturedWork {
    /// The commit both trees are relative to: the source's `HEAD`, read once,
    /// and the start point the spin-off's branch is then cut from. Reading it
    /// once is what stops a commit landing in the source mid-capture leaving
    /// the trees describing a different base than the new branch has — the
    /// staged/unstaged split is only meaningful against one HEAD.
    head: String,
    /// The source's index as a tree — its **staged** view.
    staged_tree: String,
    /// The index plus every unstaged edit, deletion and untracked (non-ignored)
    /// file — its **working-tree** view.
    full_tree: String,
    /// The source's working tree changed *while it was being read*, so
    /// `full_tree` may mix two moments.
    ///
    /// Measured by staging the source twice and comparing the two trees — not
    /// by comparing `git status` before and after, which was the first version
    /// and cannot see the likeliest concurrent write there is: an agent
    /// changing the **contents** of a file it had already dirtied. Porcelain
    /// status output is byte-identical across that (` M f.txt` either way), so
    /// the signal was `false` in exactly the case it existed for.
    ///
    /// What it still cannot see: a change that reverts itself between the two
    /// stagings, and a `git add` in the *source* (which alters that checkout's
    /// staged/unstaged split but leaves this capture describing one coherent
    /// earlier moment rather than a torn one — so it is not drift).
    ///
    /// Reported, never retried. Waiting for a live checkout to fall quiet is
    /// not a promise this endpoint can keep, and a silent torn snapshot is the
    /// one outcome worse than a named one.
    drifted: bool,
}

/// Stage everything in `src` into `index` and return the resulting tree.
///
/// `add -A` against a **copy** of the checkout's index. It stages every
/// unstaged edit, every deletion and every untracked file, touching nothing of
/// the source's but the shared object database — and it applies the same ignore
/// rules a plain `git add` does, so `target/` and `node_modules/` are never
/// captured. That exclusion is the reason this is `add` and not a directory
/// copy: an ignored tree is routinely tens of gigabytes.
///
/// Its own function because [`capture_uncommitted`] calls it **twice** and
/// compares the two trees, which is the drift signal — and a comparison
/// deserves a seam a test can drive without racing a writer against a live
/// checkout. (That race is not merely awkward to test: a file being rewritten
/// non-atomically makes `add` fail outright with `short read while indexing`,
/// so a busy source can cost the create a 422 rather than a drift flag.)
async fn stage_everything(src: &FsPath, index: &FsPath) -> Result<String, String> {
    git_with_index(src, index, &["add", "-A", "--"]).await?;
    git_with_index(src, index, &["write-tree"]).await
}

/// Snapshot `src`'s uncommitted state without writing anything of `src`'s.
///
/// Refuses, before anything is created, for the two states the two-tree model
/// cannot represent — see the guards. Everything else it delegates to git:
/// ignore rules, clean filters, deletion detection and type changes are all
/// `git add`'s answers, not ours, which is why this is `add -A` into a scratch
/// index and not a directory walk.
async fn capture_uncommitted(src: &FsPath) -> Result<CapturedWork, String> {
    // A checkout mid-merge has conflict stages in its index, and `write-tree`
    // cannot represent an unmerged index at all — it exits 128 with
    // `error building trees`. Splicing the stages across instead was
    // considered and rejected: the *merge* (`MERGE_HEAD`, `MERGE_MSG`) is
    // per-worktree state that no tree object carries, so the spin-off would
    // get conflict markers in a checkout where `git merge --abort` has nothing
    // to abort — a worse place to be than not having the changes.
    if !git(src, &["ls-files", "--unmerged"]).await?.is_empty() {
        return Err(
            "that checkout is in the middle of a merge, so its uncommitted changes cannot \
             be carried across — finish or abort the merge first, or create the worktree \
             without carrying them"
                .to_owned(),
        );
    }
    // A sparse checkout's out-of-cone files are absent from disk, which
    // `git add -A` reads as deletions — so `full_tree` would instruct the new
    // checkout to delete every path outside the cone, and the new checkout is
    // not sparse. Refusing beats materialising that.
    if git(src, &["config", "--bool", "core.sparseCheckout"])
        .await
        .as_deref()
        == Ok("true")
    {
        return Err(
            "that checkout is sparse, so its uncommitted changes cannot be carried across \
             — create the worktree without carrying them"
                .to_owned(),
        );
    }

    let head = git(src, &["rev-parse", "HEAD"]).await?;
    // A linked worktree has its own index under `.git/worktrees/<name>/`, which
    // is what `--absolute-git-dir` resolves to from inside it — the main
    // checkout's `.git/index` would be a different checkout's staged state.
    let git_dir = PathBuf::from(git(src, &["rev-parse", "--absolute-git-dir"]).await?);
    let index = git_dir.join("index");

    // **0700, and outside the checkout.** After `git add -A` the scratch index
    // holds the source repository's whole path inventory — untracked
    // non-ignored filenames included — plus per-file sizes, modes and blob
    // hashes. `TempDir::new()` alone is 0755, and on Linux
    // `std::env::temp_dir()` is `/tmp` whenever `TMPDIR` is unset, which is the
    // shape of the daemon's own systemd user unit (it sets neither
    // `PrivateTmp=` nor `TMPDIR`) — so a second local uid could read it.
    //
    // It must also stay **outside the source's working tree**: `add -A` would
    // otherwise capture the scratch index as an untracked file, and the second
    // staging below would then differ from the first every single time, making
    // `drifted` permanently true. (Inside `.git` would be excluded and safe,
    // but writing anything of the source's is the one thing this function
    // promises not to do.)
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(|e| format!("failed to create a scratch directory: {e}"))?;
    let staged_index = scratch.path().join("staged.index");
    let full_index = scratch.path().join("full.index");
    // Copied, not shared: every git call below writes the index it is pointed
    // at. A missing index file means there is no staged state to read at all,
    // which for a real checkout means something is wrong with it — refuse
    // rather than proceed against an empty index, whose `write-tree` is the
    // empty tree and whose `read-tree -u --reset` would empty the new checkout.
    for dest in [&staged_index, &full_index] {
        std::fs::copy(&index, dest)
            .map_err(|e| format!("failed to read {}: {e}", index.display()))?;
    }

    let staged_tree = git_with_index(src, &staged_index, &["write-tree"]).await?;
    let full_tree = stage_everything(src, &full_index).await?;
    // Stage the source a second time and compare the trees: identical means
    // nothing wrote the working tree between the two passes. Cheap despite
    // looking like double work — the first pass left this index's stat cache
    // fresh, so the second re-hashes only what actually changed (a bare
    // `touch`, same content, produces the same tree).
    let full_tree_again = stage_everything(src, &full_index).await?;

    let drifted = full_tree_again != full_tree;
    Ok(CapturedWork {
        head,
        staged_tree,
        // `full_tree` is the earlier of the two and the one that gets applied;
        // the second exists only to answer whether it is still the whole truth.
        full_tree,
        drifted,
    })
}

/// Reconstitute a [`CapturedWork`] in a checkout that is sitting clean at
/// [`CapturedWork::head`].
///
/// **Submodules are the one thing that cannot land**, and it is git's floor
/// rather than this function's: `git worktree add` does not populate
/// submodules, so a fresh checkout has an empty directory where one belongs.
/// The *index* comes out identical to the source's either way — it is an
/// **uncommitted** submodule pointer move (` M sub`) that has nowhere to go,
/// which is why the destination can report one fewer dirty path than the
/// source in that case. Verified against real git 2.50: a plain
/// `git worktree add` with no carry-over leaves the same empty directory.
///
/// Two `read-tree`s and deliberately nothing else. There is **no
/// `update-index --refresh`** afterwards: it is unnecessary (`git status` in
/// the new checkout is already correct without it, measured) and it exits 1 by
/// design when it finds a genuinely-modified file, so a caller that checked its
/// status would be reading a healthy result as a failure.
async fn apply_captured(dest: &FsPath, work: &CapturedWork) -> Result<(), String> {
    // The working-tree view, with `-u --reset`: writes every captured file to
    // disk and removes the ones the source had deleted.
    git(dest, &["read-tree", "-u", "--reset", &work.full_tree]).await?;
    // Then the staged view over the index **alone** — no `-u`, so the files on
    // disk stay as the line above left them. This is what reproduces the split
    // rather than presenting everything as staged. An untracked file is in
    // `full_tree` and not in `staged_tree`, so it lands on disk and is
    // untracked again here, which is exactly right.
    git(dest, &["read-tree", &work.staged_tree]).await?;
    Ok(())
}

/// The create response: the worktree, plus what its carry-over did.
///
/// `worktree` is **flattened**, so this is wire-compatible with the plain
/// `WorktreeView` every caller read before — a client that ignores
/// `carry_over` sees exactly the object it always saw.
#[derive(Serialize)]
struct CreatedWorktreeView {
    #[serde(flatten)]
    worktree: WorktreeView,
    /// Present only for a spin-off that was asked to carry work across.
    #[serde(skip_serializing_if = "Option::is_none")]
    carry_over: Option<CarryOverReport>,
}

/// How many paths the new checkout has uncommitted — the number reported as
/// [`CarryOverReport::files`].
///
/// **Not [`git_status`], and the difference is a wrong number.** That helper
/// answers "what would block `git worktree remove`", which is a question about
/// *paths in the way*, so it lets git collapse a whole untracked directory into
/// one `?? newdir/` record — and it honours `status.showUntrackedFiles=no`,
/// a repo-config knob shared by every worktree of that repo, under which
/// untracked paths vanish from the answer entirely. Reusing it undercounted
/// every spin-off that carried a new directory (the common case when an agent
/// has been working) and reported `0` — no toast at all — for a carry-over of
/// untracked-only work in a repo with that config set.
///
/// `-uall` answers both: it expands directories and overrides the config
/// (verified against git 2.50).
///
/// **The counting itself is [`parse_git_status`]'s, not a `split('\0').count()`.**
/// A rename is *two* NUL-delimited fields — `R  new.txt\0old.txt\0` — so a raw
/// field count reports one moved file as two, and the first version of this
/// function did exactly that: it added `-uall` to fix the undercount and
/// reimplemented the parsing a few hundred lines away from the parser that
/// already documents this trap. Reusing it fixes both directions at once and
/// keeps one owner for "what does a porcelain record mean".
///
/// **`None` when the status cannot be read at all**, which is a different fact
/// from `Some(0)` and has to stay one. A count of zero says "nothing arrived";
/// no count says "something may well have arrived and I cannot tell you how
/// much". Collapsing them into `0` made a carry-over that *succeeded* — apply
/// fine, status read failed — reach the client as `files: 0` with no `error`,
/// which fell through every branch of the UI's report and showed the user
/// nothing whatsoever.
async fn carried_file_count(dir: &FsPath) -> Option<usize> {
    // Via `git_raw` for the same reason `git_status` uses it: an unstaged
    // change's porcelain code begins with a space, and the trimming helper
    // would destroy it.
    let out = git_raw(dir, &["status", "--porcelain=v1", "-z", "-uall"])
        .await
        .ok()?;
    Some(parse_git_status(&String::from_utf8_lossy(&out)).len())
}

/// What a spin-off's carry-over actually did, reported alongside the created
/// worktree.
///
/// It exists because the alternative is a silent partial success: the checkout
/// is created, and about to be registered, *before* the carry-over runs — so
/// without this field a caller cannot tell "spun off with your changes" from
/// "spun off, changes left behind".
#[derive(Debug, Serialize)]
struct CarryOverReport {
    /// How many paths the new checkout has uncommitted afterwards. A count,
    /// because "your changes came across" is worth more with a number next to
    /// it — and **`null` when it could not be counted**, which a client must
    /// not render as zero: see [`carried_file_count`].
    files: Option<usize>,
    /// See [`CapturedWork::drifted`].
    drifted: bool,
    /// Why the carry-over did not happen. The worktree exists either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Git branch names allow `/` and more, but reject anything that could read
/// as an option or escape a path: leading `-`, whitespace/control characters,
/// and `..`.
fn validate_branch(branch: &str) -> Result<(), ApiError> {
    let bad = branch.is_empty()
        || branch.len() > 200
        || branch.starts_with('-')
        || branch.contains("..")
        || branch
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '~' || c == '^' || c == ':');
    if bad {
        return Err(err(StatusCode::BAD_REQUEST, "invalid branch name"));
    }
    Ok(())
}

fn validate_alias(alias: &str) -> Result<(), ApiError> {
    // `.`/`..` pass is_safe_identifier but fail validate_run_name later (the
    // run name defaults to the alias) — reject them here so the dead end
    // surfaces at rename time, not at start time.
    if !is_safe_identifier(alias) || alias == "." || alias == ".." {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "alias must be 1-64 characters: letters, digits, '-', '_', '.'",
        ));
    }
    Ok(())
}

/// Longest accepted worktree display name.
///
/// Not the alias's 64: this one is read out of a rail column that is 236px wide
/// by default, so the bound is about what can be seen rather than what can be
/// stored. Long enough for a sentence fragment ("Checkout V2 (final)"), short
/// enough that no single row can be the reason the rail needs a horizontal
/// scrollbar.
const MAX_DISPLAY_NAME_LEN: usize = 80;

/// Characters that are neither control characters nor visible.
///
/// **`char::is_control` is not enough, and the gap has teeth.** It reports only
/// Unicode category Cc (U+0000–1F, U+007F–9F), so every zero-width and
/// bidirectional-formatting character passes it. Three consequences, all of them
/// reachable by pasting a name copied out of an issue title or a branch name:
///
/// - **A name of one U+200B defeats the `''` sentinel.** It is not empty, so
///   `worktreeLabel` renders it instead of falling back to the alias, and the
///   rail row, the palette entry, the window title and the tray item all come out
///   *blank* — a checkout with nothing on screen identifying it. JavaScript's
///   `\s` does not match U+200B either, so the create dialog's own whitespace
///   collapsing does not catch it first.
/// - **U+2028/U+2029 are real line breaks**, which is precisely what
///   [`validate_display_name`] claims to reject. `white-space: nowrap` suppresses
///   soft wraps, not forced ones, so `"prod\u{2028}rm -rf"` renders in the rail
///   as `prod`. (Rejecting them is [`is_forbidden`]'s job, not this predicate's —
///   this one only answers whether a character renders.)
/// - **U+202E reverses the rendered label**, so `"\u{202E}tset olleH"` displays
///   as `Hello test`.
///
/// An explicit list rather than a Unicode-category crate: this is the whole set
/// of default-ignorables and bidi controls, it does not move between Unicode
/// revisions in any way that matters here, and a dependency for one predicate on
/// one field is the more expensive answer.
///
/// **This set includes the zero-width joiner and non-joiner, and they are
/// deliberately *not* rejected** — see [`is_forbidden`]. U+200D is the glue in
/// every multi-person, profession and flag emoji (`👩‍💻` is U+1F469 U+200D
/// U+1F4BB), and U+200C is orthographically required in Persian and Hindi. They
/// belong here because they contribute no glyph *on their own*, which is what
/// this predicate answers.
///
/// It **overlaps [`is_forbidden`] on purpose** rather than being the complement
/// of it. Those ranges are unreachable through the one caller today, which
/// checks `is_forbidden` first — but this predicate answers "does this character
/// render as anything", and a version that answered "yes" for U+2028 in order to
/// avoid the overlap would be wrong the moment anything else called it.
fn is_invisible(c: char) -> bool {
    c.is_control()
        || c.is_whitespace()
        || matches!(c,
            '\u{00AD}'                  // soft hyphen
            | '\u{061C}'                // arabic letter mark
            | '\u{180E}'                // mongolian vowel separator
            | '\u{200B}'..='\u{200F}'   // zero-width space/non-joiner/joiner, LRM, RLM
            | '\u{202A}'..='\u{202E}'   // bidi embedding and override
            | '\u{2060}'..='\u{2064}'   // word joiner, invisible operators
            | '\u{2066}'..='\u{2069}'   // bidi isolates
            | '\u{FEFF}'                // zero-width no-break space / BOM
            | '\u{FFF9}'..='\u{FFFB}'   // interlinear annotation
        )
}

/// Characters rejected outright, because they do not merely fail to render —
/// they change how the characters *around* them render.
///
/// `char::is_control` alone is not enough: it reports only category Cc
/// (U+0000–1F, U+007F–9F), so every one of the rest of these passes it.
/// U+2028/U+2029 are forced line breaks that `white-space: nowrap` does not
/// suppress, so `"prod\u{2028}rm -rf"` shows in the rail as `prod`; U+202E
/// reverses the label, so `"\u{202E}tset olleH"` displays as `Hello test`.
///
/// Deliberately narrower than [`is_invisible`]: a merely invisible character is
/// harmless *beside a visible one* and sometimes required (see that function's
/// note on U+200D). What is never acceptable is a name made of nothing else,
/// which [`validate_display_name`] checks separately.
fn is_forbidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{2028}' | '\u{2029}'     // line separator, paragraph separator
            | '\u{202A}'..='\u{202E}'   // bidi embedding and override
            | '\u{2066}'..='\u{2069}'   // bidi isolates
            | '\u{FEFF}'                // zero-width no-break space / BOM
            | '\u{FFF9}'..='\u{FFFB}'   // interlinear annotation
        )
}

/// Bound the free-text worktree label.
///
/// Unlike the alias this is not an identifier — it never reaches a hostname, a
/// path, or a command line — so the rule is only "a human can read it in the
/// rail". Three clauses:
///
/// 1. A length cap in **characters**, since the cap exists for legibility and
///    one emoji is one column, not four.
/// 2. Nothing from [`is_forbidden`], which changes how its neighbours render.
/// 3. **At least one visible character**, unless the name is empty.
///
/// Clause 3 is the one that matters and the one a per-character blocklist cannot
/// express. `""` is the sentinel meaning "render the alias", and `worktreeLabel`
/// falls back on exactly `""` — so a name of one zero-width space is *non-empty
/// and unrenderable at once*, and the rail row, the palette entry, the window
/// title and the tray item all come out blank with nothing identifying the
/// checkout. Requiring a visible character closes that without having to guess
/// which invisible characters someone might legitimately want in the middle of a
/// name.
///
/// Rejected rather than stripped, matching `valid_lane_name`: silently rewriting
/// what someone typed is worse than telling them it is not a name. Trimming, on
/// the other hand, *is* applied by the caller — a trailing space is a typo with
/// one obvious intent.
fn validate_display_name(name: &str) -> Result<(), ApiError> {
    if name.chars().count() > MAX_DISPLAY_NAME_LEN {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("the name must be at most {MAX_DISPLAY_NAME_LEN} characters"),
        ));
    }
    if name.chars().any(is_forbidden) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "the name cannot contain control characters or text-direction overrides",
        ));
    }
    if !name.is_empty() && name.chars().all(is_invisible) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "the name must contain at least one visible character",
        ));
    }
    Ok(())
}

/// The glyphs `validate_emoji` accepts, for the UI's picker. Served rather
/// than duplicated in TypeScript so the two can never drift; static, so the
/// picker fetches it once on open instead of riding the 5s poll.
/// The marker faces a client may choose from: the glyph allowlist and the colour
/// palette. Served rather than duplicated in TypeScript so the two can never
/// drift; static, so the picker fetches it once on open instead of riding the 5s
/// poll.
///
/// The colours are the literal values the picker offers. Not the set of *storable*
/// values — `is_worktree_color` accepts any `#rrggbb`, so a custom colour needs no
/// migration and no change here.
async fn worktree_emoji() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "emoji": veld_core::db::WORKTREE_EMOJI,
        "colors": veld_core::db::WORKTREE_COLORS,
    }))
}

/// Turn the colour check into a 400 before any DB work. The rule lives in
/// `veld_core::db::is_worktree_color`, next to the palette.
fn validate_marker_color(color: &str) -> Result<(), ApiError> {
    if !veld_core::db::is_worktree_color(color) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "marker_color must be a lowercase #rrggbb colour",
        ));
    }
    Ok(())
}

/// Turn the curated-set check into a 400 before any DB work. The rule itself
/// lives in `veld_core::db::is_worktree_emoji`, next to the constant — this
/// is only the HTTP shape of it.
fn validate_emoji(emoji: &str) -> Result<(), ApiError> {
    if !veld_core::db::is_worktree_emoji(emoji) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "emoji must be one of the curated worktree glyphs",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Repos
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct RepoList {
    repos: Vec<RepoView>,
}

#[derive(Serialize)]
struct RepoView {
    #[serde(flatten)]
    repo: RepoRecord,
    /// False when the repo can't be listed on disk right now (directory
    /// deleted or git failing) — the worktree rows below are then the last
    /// known state, not fresh.
    ///
    /// **Exactly that, and nothing about the database.** This field used to also
    /// go false when the reconciling *write* failed, so a damaged SQLite page
    /// reported a healthy repo as unavailable and hid its start/stop controls.
    /// The daemon's own health is a single global condition
    /// (`GET /api/db-health`), not one claim per repository — N repos would
    /// otherwise each report the same one broken file.
    available: bool,
    worktrees: Vec<WorktreeView>,
    /// The repo's rail lanes, in their own order.
    ///
    /// Travels with the repo rather than on its own poll because the rail cannot
    /// render a group header without both halves, and fetching them separately
    /// means a frame where a worktree's `lane` names a lane the client has not
    /// heard of yet.
    lanes: Vec<veld_core::db::LaneRecord>,
    /// How far the repo's main checkout has drifted from its remote. The IDE
    /// surfaces this as the "update main" control in the top bar. It is *core*
    /// data (computed by the git CLI — see `docs/extensions-vision.md`); the
    /// per-worktree badge an extension might render on top of it is not.
    ///
    /// `None` everywhere a GET is the caller (see [`list_repos`]): GETs on this
    /// router must not spawn subprocesses, and git status is computed only by
    /// the CSRF-gated POST paths.
    git: Option<RepoGitStatus>,
    /// `ide.news` from this repo's **main** checkout by default — the cards the
    /// project shows its own team. See `news.source` / [`veld_core::db::ConfigSource`]
    /// for the (non-default) `worktree` alternative.
    ///
    /// On the repo rather than on each worktree because news belongs to the
    /// project, and because only what has reached main counts by default: a card
    /// being drafted in a worktree must not prompt a teammate, and a repo with
    /// five worktrees must not put the same card in front of somebody five times.
    /// [`repo_view`] takes it from the main worktree and discards the rest.
    ///
    /// **Always present, possibly empty** — the `ide` block's rule, not
    /// `presets`': a project that declares no news and a project whose config
    /// could not be read are the same thing to this surface, because both mean
    /// "nothing to tell you", and neither is worth a control the user could act
    /// on. Bounded by `veld_core::ide::MAX_NEWS_ITEMS`, which is what makes it
    /// safe on an endpoint every IDE window polls.
    news: Vec<veld_core::ide::NewsItem>,
}

/// Git-derived staleness for one repo's main checkout.
#[derive(Serialize, Default)]
struct RepoGitStatus {
    /// The default branch — the main checkout's branch. This is what the IDE
    /// means by "main" even when the project calls its default branch something
    /// else.
    default_branch: Option<String>,
    /// Commits in `origin/<default>` not in the local `<default>` — how far the
    /// main checkout is behind the remote *as of the last fetch*. `None` when it
    /// cannot be computed (no remote, or `origin/<default>` has never been
    /// fetched). `0` is a real, current answer.
    behind: Option<i64>,
    /// Unix seconds of the **newest** commit the main checkout is missing — the
    /// latest thing in `origin/<default>` it does not have yet. `None` when it
    /// cannot be computed (or when `behind` is `0`, where there is nothing to
    /// be behind on). The UI mixes this with [`Self::behind`] to colour the
    /// staleness pill — few-and-recent is green, many-and-old is red.
    latest_commit: Option<i64>,
}

#[derive(Serialize)]
struct WorktreeView {
    // **Never name a field here `carry_over`.** `CreatedWorktreeView` flattens
    // this struct beside its own `carry_over`, and serde emits a duplicate key
    // for that collision with no compile-time complaint —
    // `the_create_response_flattens_the_worktree_and_omits_an_absent_carry_over`
    // is the only thing that would notice.
    #[serde(flatten)]
    worktree: WorktreeRecord,
    /// Whether this checkout's removal is past the point of no return — the
    /// terminal state the rail separates from the trash as its own "Deleting"
    /// lane.
    ///
    /// Not a database column: it is the daemon's in-memory guard (see
    /// `worktree_trash::now_deleting`), so it reads from there rather than from
    /// the row. Only true while `git worktree remove` is actually running — a
    /// worktree that has merely been *queued* for removal still reports
    /// `trashed_at` to let the user undo it.
    deleting: bool,
    /// Whether the checkout has a root config — drives whether the UI shows run
    /// controls for it.
    has_veld_config: bool,
    /// Presets from the checkout's root config, in display order, with their keys
    /// and labels. The UI shows the label a human can read; `name` is what it sends
    /// back to start the run.
    ///
    /// **`null` means the config could not be read; `[]` means it declares no
    /// presets.** The distinction is the field's whole reason for being nullable,
    /// and it is deliberately carried by the *type* rather than by a sibling
    /// boolean: a client that compares a run's recorded preset against an empty
    /// list concludes the preset was deleted, so a mid-edit or broken `veld.json`
    /// made every healthy run in that worktree read "preset dev (no longer
    /// defined)". That shipped once already. `null` forces the consumer to decide,
    /// where a flag next to an always-present array let it not notice — see
    /// `startOrigin.ts`, whose `presets: null` case exists for exactly this.
    ///
    /// (This is the reverse of the `ide` block's rule, which is always present with
    /// possibly-empty arrays. There, empty and absent mean the same thing; here they
    /// do not.)
    presets: Option<Vec<PresetView>>,
    /// Startable nodes with their variants — the UI's custom-selection
    /// source when no preset fits (hidden nodes excluded).
    nodes: Vec<NodeOptionView>,
    /// How many vars this checkout's config declares machine-overridable, so the
    /// UI can tell "this project asks you for nothing" from "this project asks
    /// and you have not answered".
    ///
    /// **`null` means the config could not be read**, exactly as for `presets`
    /// above, and for the same reason: a client that treats an unreadable config
    /// as zero would disable the only control that could show the user *why* it
    /// is unreadable. Free to compute — the config on this path is already parsed
    /// for `presets` and `nodes`.
    machine_vars: Option<usize>,
    /// What git says about this checkout — uncommitted work, commits its upstream
    /// does not have, an upstream that has been deleted. `None` when nothing is
    /// known yet, which is what a freshly opened window sees for one poll.
    ///
    /// **Cached, and read here rather than computed here.** This view is built from
    /// database rows on a 5s poll by every open window, so it must not spawn git —
    /// the same rule that keeps [`worktree_status`] a separate on-demand endpoint.
    /// See [`git_signals_for`] for which half is refreshed where.
    #[serde(skip_serializing_if = "Option::is_none")]
    git: Option<WorktreeGitSignals>,
    /// The interpreted part of the checkout's `ide` config section.
    ///
    /// **Always present, with arrays that may be empty.** Omitting it when empty
    /// is what the client types would then have to lie about — the exact defect
    /// #190 shipped with `public_urls`/`connections`.
    ide: IdeView,
}

/// A preset plus the `node:variant` set it expands to **right now**.
///
/// The expansion travels with the listing because it is the other half of a
/// comparison the client cannot otherwise make: a run records the expansion its
/// preset meant at start time (`RunInfo.started_from`), and the two together are
/// what distinguish "this run is preset X" from "this run *was* preset X, which
/// has since been edited". `ResolvedPreset::selections` cannot answer it — those
/// are the raw entries, `@preset` refs unexpanded.
///
/// The preset is always listed — one the UI can name and start beats a hole in the
/// list — and what is *said about its expansion* is a three-state answer, because
/// collapsing any two of them makes a surface state something false.
#[derive(Serialize)]
struct PresetView {
    #[serde(flatten)]
    preset: veld_core::presets::ResolvedPreset,
    expansion: Expansion,
}

/// How many presets a single repo listing expands, per worktree.
///
/// This is the endpoint's cost bound. `GET /api/repos` is CSRF-exempt and polled by
/// every IDE window, and expansion is recursion over a config that arrives with a
/// checked-out branch — so the work per poll must not be a number the config
/// chooses. Presets past this report `skipped`, which is honest and free.
///
/// 64 against a hand-written config's handful, and a project that really has more
/// than 64 presets has a bigger problem than a partial expansion list.
const PRESETS_EXPANDED_PER_LISTING: usize = 64;

/// What this listing can say about what a preset expands to *right now*.
///
/// Three states, none of them foldable into another:
///
/// - `ok` — the sorted `node:variant` tokens, directly comparable to
///   `RunInfo.started_from.selections`. An **empty** vector is a legitimate `ok`: a
///   preset whose `selections` are `[]` really does expand to nothing.
/// - `failed` — the preset exists and does not expand: a `@ref` to something gone,
///   a since-removed node, a cycle. `veld status` says "cannot be expanded — see
///   `veld lint`" for this, and lint does report it.
/// - `skipped` — nothing is wrong with the preset; this *listing* ran out of its
///   shared expansion budget. Distinct from `failed` precisely because the label
///   `failed` earns ("see `veld lint`") would send the reader to a check that
///   passes. A client that cannot compare must say so rather than guess, exactly as
///   it does when the whole config is unreadable.
///
/// Collapsing `failed` into `ok` with an empty vector was the first shape here, and
/// it made the UI report "redefined since start" for a preset the CLI called
/// unexpandable — one config state, two contradictory claims.
#[derive(Serialize)]
#[serde(tag = "state", content = "tokens", rename_all = "snake_case")]
enum Expansion {
    Ok(Vec<String>),
    Failed,
    Skipped,
}

/// The `ide` config as the UI consumes it.
///
/// A lean view rather than `veld_core::ide::IdeSection` itself: the section also
/// carries the parse problems and the still-uninterpreted key names, and those
/// belong to `veld lint`, not to a repo listing.
#[derive(Serialize, Default)]
struct IdeView {
    quicklinks: Vec<veld_core::ide::Quicklink>,
    /// Permission pre-answers for browser panes. Only Veld Desktop can act on
    /// these — a browser tab has no panes — but they travel here because the
    /// renderer is what relays them to the Electron main process.
    permissions: Vec<veld_core::ide::PermissionRule>,
    /// Pane types this project adds to the pane menu, with the commands
    /// stripped out.
    panes: Vec<PaneView>,
    /// Badges, buttons and menus this project contributes to the IDE chrome,
    /// with the commands stripped out for the same reason [`PaneView`] omits
    /// them.
    extensions: Vec<ExtensionView>,
    /// The project's staleness-sensitivity multiplier (default 1), so the UI
    /// colours the "update main" pill per the project's `ide` config rather than
    /// a global curve. Floored to `0.1` so a hand-written `0` cannot divide by
    /// zero or invert the curve.
    staleness_sensitivity: f64,
    /// This checkout's `ide.news`, **deliberately never serialized** — it is
    /// consumed by [`select_news`] inside [`repo_view`] and moved up to
    /// [`RepoView::news`] instead, per `news.source`: by default only from the
    /// *main* worktree, with every other checkout's copy discarded; opted into
    /// `worktree` mode, unioned across every checkout instead.
    ///
    /// It rides here rather than being parsed a second time because
    /// [`worktree_view`] already has the config open, and it is `skip`ped rather
    /// than merely ignored by the client because the rule it enforces is a
    /// promise about what a card can do: by default, news counts only once it
    /// has reached main, so a card being drafted in another worktree must not
    /// be able to prompt a teammate. A client-side filter would make that
    /// promise re-breakable by the next person to touch the renderer — and
    /// `news.source = worktree` is the one place that promise is deliberately
    /// traded away, in [`select_news`] itself, not here.
    #[serde(skip)]
    news: Vec<veld_core::ide::NewsItem>,
}

/// A config-declared pane as the UI needs to see it.
///
/// **The commands are deliberately absent**, and so is the token. The renderer
/// names a pane and the daemon resolves what that means from the project's own
/// config — so nothing here is a command the client could edit and post back,
/// and there is no identity for browser storage or a detach payload to drop.
#[derive(Serialize)]
struct PaneView {
    id: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<veld_core::ide::PaneIcon>,
    /// What the pane runs, as one of the runtime pane kinds.
    ///
    /// Derived from the body's variant, never written out: the irrefutable
    /// `let` below turns a second `PaneBody` variant into a compile error, but a
    /// hand-written `"terminal"` literal would survive being turned into a
    /// `match` with every arm still reporting the wrong kind to the client.
    kind: &'static str,
    /// False when something in `requires_bin` is not installed. The pane is
    /// still listed, so the menu can explain the absence rather than silently
    /// omitting an entry the repo declares.
    available: bool,
    /// The required executables that were not found, so the menu can name them.
    /// The pane's own id is not a substitute — `claude-yolo` needs `claude` and
    /// `git-log` needs `git`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing: Vec<String>,
    /// Whether the pane declares a `resume` command at all.
    can_resume: bool,
    /// Whether a restored pane whose shell is gone may resume without a click.
    auto_resume: bool,
    /// Whether a clean exit closes the pane.
    close_on_exit: bool,
    /// Whether the pane's process may rename its tab with an OSC 0/2 title.
    allow_terminal_renaming: bool,
}

/// A config-declared extension as the UI needs to see it.
///
/// **The command is deliberately absent**, exactly as in [`PaneView`]: the client
/// names an extension and the daemon resolves what that means from the project's
/// own config, so nothing here is a command a client could edit and post back.
#[derive(Serialize)]
struct ExtensionView {
    id: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<veld_core::ide::PaneIcon>,
    /// `status`, `action` or `menu`.
    kind: &'static str,
    /// The slot this renders in, or `None` for one that is only referenced.
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<String>,
    align: &'static str,
    /// False when something in `requires_bin` is not installed.
    available: bool,
    /// The required executables that were not found, so the UI can name them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing: Vec<String>,
    when_missing: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<veld_core::ide::ExtensionHint>,
    /// A menu's members, in order. Empty for the other kinds.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    items: Vec<String>,
    /// How often a `status` extension wants re-evaluating. `None` for the kinds
    /// that are not evaluated.
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_seconds: Option<u64>,
    /// A `status` extension's declared `display` (`"text"` or `"icon"`), so the
    /// client can shape its pre-first-value placeholders (loading, disabled) the
    /// same way the value it is about to replace will look, rather than showing
    /// a full-width label that narrows to a glyph the moment the first run
    /// answers. `None` for the kinds that have no `display`.
    #[serde(skip_serializing_if = "Option::is_none")]
    display: Option<&'static str>,
}

fn extension_view(ext: &veld_core::ide::Extension) -> ExtensionView {
    use veld_core::ide::{ExtensionAlign, ExtensionBody, WhenMissing};
    let missing = super::pty::missing_pane_binaries(&ext.requires_bin);
    ExtensionView {
        id: ext.id.clone(),
        label: ext.label.clone(),
        description: ext.description.clone(),
        icon: ext.icon.clone(),
        kind: ext.kind(),
        slot: ext.slot.clone(),
        align: match ext.align {
            ExtensionAlign::Start => "start",
            ExtensionAlign::End => "end",
        },
        available: missing.is_empty(),
        missing,
        when_missing: match ext.when_missing {
            WhenMissing::Hide => "hide",
            WhenMissing::Disable => "disable",
            WhenMissing::Hint => "hint",
        },
        hint: ext.hint.clone(),
        items: match &ext.body {
            ExtensionBody::Menu(menu) => menu.items.clone(),
            _ => Vec::new(),
        },
        refresh_seconds: match &ext.body {
            ExtensionBody::Status(status) => Some(status.refresh_seconds),
            _ => None,
        },
        display: match &ext.body {
            ExtensionBody::Status(status) => Some(match status.display {
                veld_core::ide::BadgeDisplay::Text => "text",
                veld_core::ide::BadgeDisplay::Icon => "icon",
            }),
            _ => None,
        },
    }
}

#[derive(Serialize)]
struct NodeOptionView {
    name: String,
    variants: Vec<String>,
    default_variant: Option<String>,
}

/// The `ide.extensions` list a worktree's view should carry, resolved from
/// `declare_root` rather than assumed to be this worktree's own — see
/// `extensions::resolve_declare_root`. Independent of whether `own_cfg`
/// (this worktree's own config) parsed at all: a worktree with no `veld.json`
/// of its own must still see main's badges when `extensions.source = main`,
/// which is the entire point of the reversal this setting exists for.
fn extensions_view_for(
    own_cfg: Option<&veld_core::config::VeldConfig>,
    own_root: &str,
    declare_root: Option<&str>,
) -> Vec<ExtensionView> {
    match declare_root {
        // `extensions.source = main` with no resolvable main checkout — fail
        // closed, exactly as `extensions::worktree_target` does for the
        // status/activate endpoints, so the listing and the endpoints that
        // act on it never disagree about what exists.
        None => Vec::new(),
        Some(root) if root == own_root => own_cfg
            .map(|c| {
                c.ide_section()
                    .extensions
                    .iter()
                    .map(extension_view)
                    .collect()
            })
            .unwrap_or_default(),
        Some(other_root) => veld_core::config::root_config_in(FsPath::new(other_root))
            .and_then(|p| veld_core::config::parse_config(&p).ok())
            .map(|c| {
                c.ide_section()
                    .extensions
                    .iter()
                    .map(extension_view)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// This worktree's `ide.quicklinks`, with any `${veld.*}` reference resolved.
///
/// A quicklink is the one *rendered* string a project templates, so the two rules
/// that govern it are different from a command's.
///
/// **Every value is percent-encoded** ([`veld_core::percent::encode_in_url`]), which
/// is the URL analogue of what slugifying `${veld.branch}` does for a shell. The
/// characters that matter are the ones `git check-ref-format` permits and a URL
/// reads as structure: a branch called `feat#2` interpolated raw would truncate the
/// link at a fragment and open the repo's front page instead of the branch, silently
/// and for the one person unlucky enough to have named a branch that way. `/` is
/// deliberately preserved, because a branch's slashes are *path* — `…/tree/feat/foo`
/// is the address, `…/tree/feat%2Ffoo` is a 404 on every host.
///
/// **A reference that will not resolve drops the link** rather than emitting a
/// half-substituted URL. The only way to reach that from a config `veld lint`
/// accepts is a branch whose name starts with `-`, for which `worktree_builtins`
/// omits `branch_raw` on purpose (see there — it is an argument-injection guard for
/// `argv`, and this inherits it because the meanings have one owner). Vanishingly
/// rare, and a missing bookmark is a better failure than a bookmark pointing at
/// somewhere real and wrong.
///
/// Interpolation is skipped entirely when no URL mentions `${`, so a project that
/// templates nothing — which is every project until it opts in — gets the same
/// strings it always did without a `HashMap` being built per worktree per poll.
fn resolved_quicklinks(
    links: Vec<veld_core::ide::Quicklink>,
    worktree_path: &str,
    branch: &str,
    config: &veld_core::config::VeldConfig,
) -> Vec<veld_core::ide::Quicklink> {
    if !links.iter().any(|link| link.url.contains("${")) {
        return links;
    }
    let mut ctx = veld_core::variables::VariableContext::new();
    for (name, value) in super::pty::worktree_builtins(FsPath::new(worktree_path), branch, config) {
        ctx.set_builtin(&name, veld_core::percent::encode_in_url(&value));
    }
    links
        .into_iter()
        .filter_map(|link| {
            let url = veld_core::variables::interpolate(&link.url, &ctx).ok()?;
            Some(veld_core::ide::Quicklink { url, ..link })
        })
        .collect()
}

fn worktree_view(db: &Db, wt: WorktreeRecord) -> WorktreeView {
    let config_path = veld_core::config::root_config_in(FsPath::new(&wt.path));
    let has_veld_config = config_path.is_some();
    let cfg = config_path
        .as_deref()
        .and_then(|p| veld_core::config::parse_config(p).ok());
    let declare_root = super::extensions::resolve_declare_root(db, &wt, db.extensions_source());
    let extensions_view = extensions_view_for(cfg.as_ref(), &wt.path, declare_root.as_deref());
    // Display order comes from the resolver, not a sort here — the UI list and
    // the CLI picker must agree, or the key printed next to a preset in one
    // surface means something else in the other.
    // `None` when the config did not parse — never an empty list, which means
    // "declares no presets". See `WorktreeView::presets`.
    let presets: Option<Vec<PresetView>> = cfg.as_ref().map(|c| {
        veld_core::presets::resolve(c)
            .into_iter()
            .enumerate()
            .map(|(i, preset)| {
                // Bounded by *count*, with each preset keeping its own expansion
                // budget — not by one budget shared across the listing.
                //
                // Sharing was the first shape and it could not tell its two failure
                // modes apart: a preset refused because an earlier one had eaten the
                // budget looked exactly like a broken preset, so the UI sent the
                // reader to `veld lint` for a config lint reports nothing about. A
                // per-preset budget also keeps this endpoint's verdict identical to
                // `veld lint`'s and `veld status`'s, which is the property that
                // stopped two surfaces contradicting each other in the first place.
                //
                // The endpoint stays bounded because the count is: 64 presets × the
                // 4096-step budget, per worktree, against a poll every few seconds.
                if i >= PRESETS_EXPANDED_PER_LISTING {
                    return PresetView {
                        preset,
                        expansion: Expansion::Skipped,
                    };
                }
                // Expand AND resolve, in that order — the same two steps
                // `veld start --preset` takes. `expand_preset` alone leaves a
                // bare `node` without its default variant, so its tokens
                // would differ from a run's recorded ones for every selection
                // written without an explicit variant.
                let expansion = veld_core::graph::expand_preset(&preset.name, c)
                    .and_then(|sels| veld_core::graph::resolve_selections(&sels, c))
                    .map(|sels| {
                        Expansion::Ok(veld_core::state::StartOrigin::new(None, &sels).selections)
                    })
                    .unwrap_or(Expansion::Failed);
                PresetView { preset, expansion }
            })
            .collect()
    });
    let mut nodes: Vec<NodeOptionView> = cfg
        .as_ref()
        .map(|c| {
            c.nodes
                .iter()
                .filter(|(_, n)| !n.hidden.unwrap_or(false))
                .map(|(name, n)| {
                    let mut variants: Vec<String> = n.variants.keys().cloned().collect();
                    variants.sort();
                    NodeOptionView {
                        name: name.clone(),
                        variants,
                        default_variant: n.default_variant.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let mut ide = cfg
        .as_ref()
        .map(|c| {
            let section = c.ide_section();
            let panes = section
                .panes
                .iter()
                .map(|p| {
                    let veld_core::ide::PaneBody::Terminal(terminal) = &p.body;
                    let kind = match &p.body {
                        veld_core::ide::PaneBody::Terminal(_) => "terminal",
                    };
                    let missing = super::pty::missing_pane_binaries(&p.requires_bin);
                    PaneView {
                        id: p.id.clone(),
                        label: p.label.clone(),
                        description: p.description.clone(),
                        icon: p.icon.clone(),
                        kind,
                        available: missing.is_empty(),
                        missing,
                        can_resume: terminal.resume.is_some(),
                        auto_resume: terminal.auto_resume,
                        close_on_exit: terminal.close_on_exit,
                        allow_terminal_renaming: terminal.allow_terminal_renaming,
                    }
                })
                .collect();
            // Read the floored scalar before the vec fields are moved below, so
            // the partial moves do not leave `section` half-borrowed.
            let staleness_sensitivity = section.staleness_sensitivity_safe();
            IdeView {
                quicklinks: resolved_quicklinks(section.quicklinks, &wt.path, &wt.branch, c),
                permissions: section.permissions,
                panes,
                // Set below, from `declare_root` rather than this worktree's own
                // section — a worktree whose own config fails to parse still
                // needs a value here, which is why it isn't read from `section`.
                extensions: Vec::new(),
                staleness_sensitivity,
                news: section.news,
            }
        })
        .unwrap_or_default();
    // Independent of whether `cfg` parsed at all: a worktree with no `veld.json`
    // of its own (or a broken one) still gets `declare_root`'s extensions when
    // `extensions.source = main` — see `extensions_view_for`.
    ide.extensions = extensions_view;
    // `wt` is moved into the view below, so the guard read happens here — before
    // the move — rather than in the literal, where `wt.id` would not resolve.
    let deleting = super::worktree_trash::now_deleting(wt.id);
    // Read before `wt` is moved into the view below, the same reason `deleting` is.
    let git = git_signals_for(wt.id);
    let machine_vars = cfg.as_ref().map(|c| {
        c.vars
            .iter()
            .flatten()
            .filter(|(_, decl)| decl.machine().is_some())
            .count()
    });
    WorktreeView {
        worktree: wt,
        deleting,
        has_veld_config,
        presets,
        nodes,
        machine_vars,
        git,
        ide,
    }
}

/// Pick `RepoView.news` out of every checkout's own `ide.news`, per
/// `news.source` — see [`ConfigSource`].
///
/// News belongs to the *project*, and by default only what has landed on main
/// counts. Taking it here — rather than letting each checkout carry its own —
/// is what makes "a card being drafted in a worktree cannot prompt anybody"
/// true by construction. It also keeps the payload one copy per repo on an
/// endpoint every IDE window polls, instead of one per worktree.
///
/// Every other checkout's list is dropped with the `WorktreeView` it rode in
/// on: `IdeView::news` is `#[serde(skip)]`, so nothing but this function can
/// move a card onto the wire.
fn select_news(
    worktrees: &mut [WorktreeView],
    source: ConfigSource,
) -> Vec<veld_core::ide::NewsItem> {
    match source {
        ConfigSource::Main => worktrees
            .iter_mut()
            .find(|w| w.worktree.is_main)
            .map(|w| std::mem::take(&mut w.ide.news))
            .unwrap_or_default(),
        // `news.source = worktree`: preview a card before it merges. There is no
        // single "current" worktree at this layer (`repo_view` answers for every
        // worktree at once, for every window's poll), so this unions every
        // checkout's own declared news instead of picking one — deliberately a
        // testing posture, not a production one: it is exactly the guarantee the
        // `main` default exists for ("a draft cannot reach a teammate") traded
        // away on purpose, same as `extensions.source = worktree`.
        ConfigSource::Worktree => {
            // Main is folded in first, and a later worktree **overrides** an id
            // already seen rather than being dropped by it — two properties that
            // matter for different reasons:
            //
            // - Main first means main's own cards hold their slots before
            //   anything else is considered, so a same-day flood of distinct
            //   ids from other worktrees cannot crowd them out on a tie — see
            //   the reversed tie-break below. It does **not** protect a main
            //   card against a genuinely older `since`: the cap always drops
            //   the oldest card it has, insertion order notwithstanding, and
            //   main's own cards are not exempt from that rule.
            // - Override, not skip, means editing an *already-merged* card on a
            //   branch previews the edit — the literal use case this mode
            //   exists for — instead of always losing to main's stale copy of
            //   the same id.
            //
            // A trashed worktree is skipped entirely: it is being removed, not
            // being worked in, and its draft cards should not spend cap slots
            // or reach anybody.
            //
            // The non-main fold-in order is otherwise `list_worktrees`'s own
            // `WT_ORDER` (lane, then its position, then name) — user-mutable by
            // dragging a lane or renaming a worktree. Two non-main worktrees
            // declaring the same id in this mode means which one previews can
            // change when a lane is reordered; this mode already carries the
            // "for testing, not daily use" label for a reason, and this is one
            // of them. `sort_by_key` below relies on a **stable** sort to keep
            // that order deterministic among non-main worktrees; do not change
            // it to `sort_unstable_by_key`.
            let mut order: Vec<usize> = (0..worktrees.len())
                .filter(|&i| worktrees[i].worktree.trashed_at.is_empty())
                .collect();
            order.sort_by_key(|&i| !worktrees[i].worktree.is_main);

            let mut index_by_id: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut merged: Vec<veld_core::ide::NewsItem> = Vec::new();
            for i in order {
                for item in std::mem::take(&mut worktrees[i].ide.news) {
                    match index_by_id.get(&item.id) {
                        Some(&pos) => merged[pos] = item,
                        None => {
                            index_by_id.insert(item.id.clone(), merged.len());
                            merged.push(item);
                        }
                    }
                }
            }
            // Over the cap, drop the **oldest `since`** first, the same
            // principle as the project's own per-config cap (`parse_news` in
            // `veld_core::ide`). The tie-break is deliberately the mirror image
            // of that function's, though: `parse_news` breaks a same-day tie by
            // ascending array position because *there* position is a finer-
            // grained proxy for authored order (one project, one author,
            // appended over time). Here position instead just records which
            // worktree the card rode in on — main always first — so breaking a
            // same-day tie the same way would silently drop main's own cards
            // first on every tie, which is precisely the crowding-out this
            // function's main-first ordering exists to prevent. So ties favour
            // the **lower** index (main, and whichever worktree was folded in
            // earliest) surviving instead.
            if merged.len() > veld_core::ide::MAX_NEWS_ITEMS {
                let mut by_age: Vec<usize> = (0..merged.len()).collect();
                by_age.sort_by(|&a, &b| merged[a].since.cmp(&merged[b].since).then(b.cmp(&a)));
                let doomed: std::collections::HashSet<usize> = by_age
                    .into_iter()
                    .take(merged.len() - veld_core::ide::MAX_NEWS_ITEMS)
                    .collect();
                let mut kept = Vec::with_capacity(veld_core::ide::MAX_NEWS_ITEMS);
                for (i, item) in merged.into_iter().enumerate() {
                    if !doomed.contains(&i) {
                        kept.push(item);
                    }
                }
                merged = kept;
            }
            merged
        }
    }
}

async fn repo_view(
    db: &Db,
    repo: RepoRecord,
    available: bool,
    git: Option<RepoGitStatus>,
) -> Result<RepoView, ApiError> {
    let mut worktrees: Vec<WorktreeView> = db
        .list_worktrees(FsPath::new(&repo.root))
        .map_err(db_err)?
        .into_iter()
        .map(|wt| worktree_view(db, wt))
        .collect();
    let news = select_news(&mut worktrees, db.news_source());
    let lanes = db.list_lanes(FsPath::new(&repo.root)).map_err(db_err)?;
    Ok(RepoView {
        repo,
        available,
        worktrees,
        lanes,
        git,
        news,
    })
}

/// Fetch a repo's remote, at most once per [`FETCH_INTERVAL`] per repo, so the
/// staleness signal tracks the remote without hammering it (and only while the
/// IDE is open — this runs on the UI's poll). Failures are swallowed: offline,
/// or a repo with no remote, leaves the last successful fetch's refs in place
/// and `behind` still computes against them. A fetch is deliberately **not** a
/// fast-forward — it never touches a working tree — which is the line that keeps
/// a background fetch safe where the auto-update the maintainer rejected is not.
const FETCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
static LAST_FETCH: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

async fn maybe_fetch(repo_root: &FsPath) {
    let key = repo_root.to_string_lossy().into_owned();
    // The guard is dropped before the await: a `std::sync::MutexGuard` is not
    // `Send`, and holding one across an await would make this future non-Send,
    // which axum rejects for a handler.
    let due = {
        let last = LAST_FETCH.lock().expect("last-fetch mutex poisoned");
        last.get(&key)
            .map(|t| t.elapsed() >= FETCH_INTERVAL)
            .unwrap_or(true)
    };
    if due {
        // **`--prune`, which is what makes `[gone]` a thing that can be observed.**
        // A merge-and-delete leaves `origin/<branch>` behind locally until something
        // prunes it, and until then `%(upstream:track)` reports the branch as merely
        // in sync — so the rail's "this one is finished" glyph would never appear.
        // Pruning only deletes remote-tracking refs the remote no longer has; it
        // touches no local branch, no working tree and no worktree, which keeps it
        // on the safe side of the same line the non-fast-forward fetch is on.
        let _ = git(repo_root, &["fetch", "--prune", "origin"]).await;
        LAST_FETCH
            .lock()
            .expect("last-fetch mutex poisoned")
            .insert(key, std::time::Instant::now());
    }
}

/// How far a repo's main checkout is behind `origin/<default>`, computed by the
/// git CLI (not the DB). `None` for every field it cannot determine.
///
/// The count is against the remote-tracking refs as they are *right now* — it
/// does not fetch. Fetching is the caller's job (see the throttled fetch in
/// [`refresh_repos`]), because a GET here would spawn git and this helper is
/// also reachable from paths that must not.
async fn repo_git_status(db: &Db, repo_root: &FsPath) -> RepoGitStatus {
    let default_branch = db
        .list_worktrees(repo_root)
        .ok()
        .and_then(|wts| wts.into_iter().find(|w| w.is_main).map(|w| w.branch));
    let Some(default_branch) = default_branch else {
        return RepoGitStatus::default();
    };
    // Commits in origin/<default> not in local <default> — how far *behind* the
    // main checkout is. (The other direction, `<default>..origin/<default>`,
    // would be how far ahead.) Fails — `None` — when `origin/<default>` does
    // not exist, which means the remote has never been fetched or has no such
    // branch.
    let behind = git(
        repo_root,
        &[
            "rev-list",
            "--count",
            &format!("{default_branch}..origin/{default_branch}"),
        ],
    )
    .await
    .ok()
    .and_then(|s| s.trim().parse::<i64>().ok());
    // Committer timestamp (`%ct`, seconds since epoch) of the newest commit in
    // the same range — the most recent thing the main checkout is missing.
    let latest_commit = git(
        repo_root,
        &[
            "log",
            "-1",
            "--format=%ct",
            &format!("{default_branch}..origin/{default_branch}"),
        ],
    )
    .await
    .ok()
    .and_then(|s| s.trim().parse::<i64>().ok());
    RepoGitStatus {
        default_branch: Some(default_branch),
        behind,
        latest_commit,
    }
}

// ---------------------------------------------------------------------------
// Veld's own database: health and recovery
// ---------------------------------------------------------------------------

/// What the IDE polls to know whether veld's own state is intact.
///
/// A GET, and therefore ungated — which is correct here for the same reason
/// [`list_repos`] is: it spawns nothing and takes no write lock. It does read
/// the backups directory and deep-check one artifact, so it is `spawn_blocking`
/// work rather than inline.
async fn db_health() -> Result<Json<crate::dbhealth::HealthView>, ApiError> {
    let view = tokio::task::spawn_blocking(crate::dbhealth::view_blocking)
        .await
        .map_err(|e| {
            warn!("db health view panicked: {e}");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not read database health",
            )
        })?;
    Ok(Json(view))
}

#[derive(Deserialize)]
struct NotifiedBody {
    /// The `notify.id` the client is claiming.
    id: String,
}

#[derive(Serialize)]
struct ClaimResponse {
    /// Whether *this* caller may raise the system notification. False means
    /// another window already has.
    claimed: bool,
}

/// Claim a pending notification, so several open windows raise one system banner
/// between them rather than one each.
///
/// The claim is what makes "we already told the human" durable: it is written
/// beside the database rather than into it, because the fault being announced is
/// frequently the database refusing writes.
///
/// **The id is checked against the closed set, not against a length.** It becomes
/// a key in a file the daemon rewrites whole and re-parses on every health poll,
/// and this router is same-origin with any page a veld run serves — so a length
/// bound plus a comment saying "the ids are a closed set" let any such page grow
/// that file without limit and tax every poll.
async fn db_health_notified(
    Json(body): Json<NotifiedBody>,
) -> Result<Json<ClaimResponse>, ApiError> {
    if !crate::dbhealth::is_notify_id(&body.id) {
        return Err(err(StatusCode::BAD_REQUEST, "not a notification id"));
    }
    let id = body.id.clone();
    let claimed = tokio::task::spawn_blocking(move || crate::dbhealth::claim_notified(&id))
        .await
        .map_err(|e| {
            warn!("recording a database notification panicked: {e}");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not record the notification",
            )
        })?;
    Ok(Json(ClaimResponse { claimed }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreDbResponse {
    /// The artifact that was put back.
    restored_from: String,
    /// Where the database that was there has been kept. It is never deleted —
    /// it is the only evidence of what went wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_moved_to: Option<String>,
    schema_version: i64,
    /// Whether the daemon will come back by itself. False when nothing is
    /// managing it, where the caller has to start it again.
    restarts_automatically: bool,
    /// What to run when it will *not* come back on its own.
    ///
    /// The CLI has said this since `veld backup restore` existed (its
    /// `restart_hint`), and the IDE said only "start the daemon again" — leaving
    /// somebody who had just replaced their database in a GUI strictly worse off
    /// than the same person in a terminal. `None` when the restart is automatic.
    #[serde(skip_serializing_if = "Option::is_none")]
    restart_hint: Option<String>,
}

/// Put the newest restorable backup back, then restart the daemon.
///
/// **Why this restarts rather than swapping the file underneath itself.** Every
/// database access in this process is short-lived (`Db::open()` per request, per
/// scheduler pass), but "short" is not "none": a stats tick holding a handle
/// across the rename would keep writing into the displaced file, and those
/// writes would be silently lost. `veld backup restore` refuses to run at all
/// while a daemon is up for exactly this reason. Rather than invent a quiescing
/// protocol for every task, this raises `SIGTERM` on itself once the file is in
/// place: the ordinary graceful shutdown runs — which deliberately leaves
/// terminal shells alive, because their PTYs belong to holder processes — and
/// launchd (`KeepAlive`) or systemd (`Restart=always`) starts the daemon again
/// within seconds, on the restored file. `veld update` already depends on this
/// same property.
/// **Two gates, and the CSRF header is not one of them.**
///
/// This router is merged into the server Caddy proxies at `/__veld__/*` on every
/// run's own origin, so a script on the user's dev app is *same-origin* with it —
/// the exposure [`crate::feedback_server::ide::get_state`] already documents. On
/// that surface `check_csrf` is a header a same-origin `fetch` sets in one line,
/// so it separates cross-origin from same-origin and nothing else. Every other
/// mutation here is bounded by that reasoning; this one would not be — replacing
/// the whole database and stopping the daemon is a different class of capability
/// from renaming a worktree, and it reaches code execution by way of the settings
/// table it installs.
///
/// So:
///
/// 1. **A fault must already be recorded.** On a healthy machine this endpoint
///    does nothing at all, which removes "roll a working database back" from the
///    set of things any caller can do.
/// 2. **A human must confirm in a native dialog** the page cannot draw or
///    dismiss — the same `osascript`/`zenity` mechanism [`pick_directory`] uses.
///    No GUI to ask on (a headless box, a TCC refusal) means refusal, pointing at
///    `veld backup restore`, which has its own daemon-down and TTY consent gates.
///    The extra click is real friction on a rare, destructive action, and it is
///    the only thing on this surface that distinguishes a person from a script.
async fn db_health_restore() -> Result<Json<RestoreDbResponse>, ApiError> {
    /// One restore at a time, process-wide. Two concurrent restores would race
    /// over the same rename and the loser would displace the file the winner
    /// just wrote. Taken before the dialog, so a second caller cannot stack a
    /// second prompt on the user's screen either.
    static RESTORING: SingleFlight = SingleFlight::new();
    let _guard = RESTORING
        .try_enter()
        .ok_or_else(|| err(StatusCode::CONFLICT, "a restore is already running"))?;

    // Gate 1. Also the honest answer to "why did nothing happen": a restore is a
    // recovery action, and there is nothing to recover from.
    //
    // Keyed on *corruption*, not on any fault: a full disk or a read-only volume
    // is not fixed by installing an old copy, and arming this endpoint on one
    // would put a destructive action behind a transient condition.
    if !crate::dbhealth::corruption_recorded() {
        return Err(err(
            StatusCode::CONFLICT,
            "the database is not reporting damage — nothing to restore from",
        ));
    }

    // Gate 2.
    match confirm_destructive(
        "Replace Veld's database with the newest backup?\n\nEverything Veld has \
         learned since that copy was taken is lost, including which environments \
         are running. The current database is kept, renamed.",
    )
    .await
    {
        Confirmed::Yes => {}
        Confirmed::No => return Err(err(StatusCode::CONFLICT, "the restore was cancelled")),
        Confirmed::CannotAsk(why) => {
            warn!("database restore refused — cannot confirm with a human: {why}");
            return Err(err(
                StatusCode::CONFLICT,
                "a restore has to be confirmed on this machine and no dialog could be \
                 shown — run `veld backup restore` instead",
            ));
        }
    }

    let report = tokio::task::spawn_blocking(restore_newest_backup)
        .await
        .map_err(|e| {
            warn!("database restore panicked: {e}");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the restore did not complete",
            )
        })??;

    // Drops the fault *and* the "already told them" marker for it.
    //
    // The in-memory half is nearly ceremonial — the state is process-local and
    // this process exits in 750 ms — and an earlier comment here claimed it was
    // stopping the fault from "surviving into the restarted daemon's first health
    // response", which it cannot do either way. The half that matters is the
    // marker file, which *does* outlive the process: without clearing it, a
    // failing volume that damages the restored file within the cooldown — the
    // expected shape of this fault, not a freak one — would raise no system
    // notification at all.
    crate::dbhealth::clear_fault();

    // After the response, not before it: the client needs to be told where its
    // old database went, and this process is about to stop answering. A short
    // delay is enough for axum to flush, and the restart is not urgent — the
    // file on disk is already the restored one.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        info!("restarting after a database restore");
        request_own_shutdown();
    });

    Ok(Json(report))
}

/// The blocking half of [`db_health_restore`].
fn restore_newest_backup() -> Result<RestoreDbResponse, ApiError> {
    use veld_core::db::backup;

    let target = Db::default_path().map_err(|e| {
        warn!("database restore: no database path: {e}");
        err(StatusCode::INTERNAL_SERVER_ERROR, "no database path")
    })?;
    let dir = Db::open()
        .ok()
        .and_then(|db| db.backup_prefs().dir)
        .or_else(backup::default_dir)
        .ok_or_else(|| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "no backup directory could be determined",
            )
        })?;

    // `newest_restorable`, not `newest`: it deep-checks, so this cannot put back
    // a copy that took the damage with it. A `None` here is a real answer — say
    // so rather than restoring something unverified.
    let candidate = backup::newest_restorable(&dir, chrono::Utc::now()).ok_or_else(|| {
        err(
            StatusCode::CONFLICT,
            "none of the backups on disk can be restored",
        )
    })?;

    let report = backup::restore(&candidate.path, &target).map_err(|e| {
        warn!("database restore failed: {e}");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("restore failed: {e}"),
        )
    })?;
    info!(
        "restored the database from {} (schema v{})",
        report.restored_from.display(),
        report.schema_version
    );

    let automatic = crate::dbhealth::service_manager_will_restart();
    Ok(RestoreDbResponse {
        restored_from: report.restored_from.display().to_string(),
        previous_moved_to: report
            .previous_moved_to
            .as_ref()
            .map(|p| p.display().to_string()),
        schema_version: report.schema_version,
        restarts_automatically: automatic,
        restart_hint: if automatic {
            None
        } else {
            crate::dbhealth::restart_hint()
        },
    })
}

/// The answer to a native yes/no prompt.
enum Confirmed {
    Yes,
    No,
    /// No dialog could be put on screen (headless, no backend installed, a TCC
    /// refusal). **Never treated as yes**: the whole point of asking is that a
    /// script cannot answer, so an unanswerable question is a refusal.
    CannotAsk(String),
}

/// Put a destructive question on the user's screen and wait for the answer.
///
/// Backends in the same order and for the same reasons as [`pick_directory`]'s:
/// `osascript` on macOS, then `zenity`/`kdialog` on Linux. Reuses [`run_picker`],
/// whose cancel-versus-failure handling is already careful about the difference
/// between "the user said no" and "GTK printed a warning".
async fn confirm_destructive(message: &str) -> Confirmed {
    // `display dialog` with an explicit `cancel button` is what makes a dismissal
    // arrive as osascript's -128 rather than as a successful run whose stdout
    // happens to say Cancel — `run_picker` keys on exactly that.
    #[cfg(target_os = "macos")]
    let attempts: Vec<(&str, Vec<String>)> = vec![(
        "osascript",
        vec![
            "-e".to_string(),
            format!(
                r#"display dialog {} with title "Veld" buttons {{"Cancel", "Replace database"}} default button "Cancel" cancel button "Cancel" with icon caution"#,
                applescript_string(message)
            ),
        ],
    )];
    #[cfg(not(target_os = "macos"))]
    let attempts: Vec<(&str, Vec<String>)> = vec![
        (
            "zenity",
            vec![
                "--question".to_string(),
                "--title=Veld".to_string(),
                format!("--text={message}"),
                "--ok-label=Replace database".to_string(),
                "--cancel-label=Cancel".to_string(),
            ],
        ),
        (
            "kdialog",
            vec![
                "--title".to_string(),
                "Veld".to_string(),
                "--warningyesno".to_string(),
                message.to_string(),
            ],
        ),
    ];

    // **Bounded, like `pick_directory`'s 10 minutes, and for a sharper reason
    // here.** The request blocks while the dialog is up, and this handler holds a
    // `SingleFlight` guard whose own docstring warns that leaking it "wedges the
    // endpoint at 409 until the daemon restarts". Without a timeout, a dialog
    // nobody ever answers — the machine is locked, the user walked away — does
    // exactly that to the one endpoint that recovers a broken database.
    // `run_picker` sets `kill_on_drop`, so the abandoned dialog leaves the screen
    // when this future is dropped.
    let asked = tokio::time::timeout(std::time::Duration::from_secs(600), async {
        // A headless Linux daemon must not read as "the user said no". zenity and
        // kdialog both exit 1 for a refusal *and* for a display they cannot open, and
        // `run_picker` maps exit 1 to `Cancelled` — correct for a picker the user
        // opened, wrong here, where the difference decides whether we tell them to
        // run `veld backup restore` instead. With no display there is nobody to ask.
        #[cfg(not(target_os = "macos"))]
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return Confirmed::CannotAsk("no display to show a confirmation on".to_string());
        }

        let mut last = String::from("no dialog backend is available on this system");
        for (cmd, args) in &attempts {
            let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
            match run_picker(cmd, &borrowed).await {
                Pick::Chosen(_) => return Confirmed::Yes,
                Pick::Cancelled => return Confirmed::No,
                Pick::Failed(why) => last = why,
                Pick::Unavailable => continue,
            }
        }
        Confirmed::CannotAsk(last)
    })
    .await;

    // An unanswered question is not a yes. Reported as "could not ask" rather
    // than as a cancellation, because the two deserve different messages: one
    // means the user said no, the other means nobody was there.
    asked.unwrap_or_else(|_| {
        Confirmed::CannotAsk("nobody answered the confirmation dialog".to_string())
    })
}

/// Quote a string for embedding in an AppleScript literal.
///
/// Only two characters matter inside AppleScript's double-quoted form —
/// backslash and the quote itself — but they matter absolutely: this string is
/// interpolated into a script that `osascript` then *executes*, so an unescaped
/// quote is a script-injection hole rather than a rendering bug. The message is
/// a constant today; this exists so it stays safe when somebody interpolates a
/// path or an error detail into it, which is the obvious next edit.
#[cfg(target_os = "macos")]
fn applescript_string(raw: &str) -> String {
    let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Ask this process to shut down the way a service manager would.
///
/// `SIGTERM` to self rather than `std::process::exit`, so the existing
/// `shutdown_signal()` path in `main` runs: it unregisters the helper routes it
/// owns and records the terminal sessions it is leaving alive. Hand-rolling an
/// exit here would be a second shutdown path that drifts from that one.
fn request_own_shutdown() {
    #[cfg(unix)]
    // SAFETY: `raise` is async-signal-safe and this delivers SIGTERM to the
    // calling process, which the daemon installs a handler for.
    unsafe {
        libc::raise(libc::SIGTERM);
    }
    #[cfg(not(unix))]
    std::process::exit(0);
}

/// List repos from the database — a pure read (GETs on this router carry no
/// CSRF gate, so they must not spawn subprocesses or take write locks).
/// `available` here is only the cheap directory-exists check; the full git
/// reconciliation happens in [`refresh_repos`].
async fn list_repos() -> Result<Json<RepoList>, ApiError> {
    let db = open_desktop_db()?;
    let mut repos = Vec::new();
    for repo in db.list_repos().map_err(db_err)? {
        let available = FsPath::new(&repo.root).is_dir();
        // `None`: this GET must not spawn git, so the top bar's git status
        // arrives on the next CSRF-gated `refresh_repos` poll.
        repos.push(repo_view(&db, repo, available, None).await?);
    }
    Ok(Json(RepoList { repos }))
}

/// Reconcile every repo's worktree rows with the checkouts git actually
/// reports, then return the fresh list — so worktrees added or removed
/// outside the app (plain `git worktree add/remove`) show up on the next
/// poll without a re-import. A repo whose directory is gone or whose git
/// call fails keeps its last-known rows and is marked `available: false`.
///
/// This is the UI's poll target. It is a POST (CSRF-gated by the router
/// layer) because it spawns git and writes — reconciliation must not be
/// triggerable by an ungated cross-origin GET. Debounced daemon-side so
/// several clients polling concurrently don't multiply the git spawns.
async fn refresh_repos() -> Result<Json<RepoList>, ApiError> {
    use std::collections::HashMap;
    use std::time::Duration;
    /// Debounce clock + the availability each repo had at the last real sync.
    /// Memoizing availability keeps concurrent clients consistent: a non-due
    /// poll must not substitute a semantically-weaker check (is_dir) that can
    /// disagree with the due poll's git result during a failure.
    static LAST_SYNC: Debounce<HashMap<String, bool>> = Debounce::new();

    let memo = LAST_SYNC.fresh_within(Duration::from_secs(2));

    let db = open_desktop_db()?;
    let mut repos = Vec::new();
    let mut availability = HashMap::new();
    let mut sweep_targets = Vec::new();
    for repo in db.list_repos().map_err(db_err)? {
        let root = PathBuf::from(&repo.root);
        let available = match &memo {
            // Repo imported inside the debounce window: not in the memo yet —
            // its rows were just written by import, dir-exists is fine.
            Some(memo) => memo.get(&repo.root).copied().unwrap_or(root.is_dir()),
            None => {
                // **Availability is git's answer, and only git's answer.**
                //
                // The reconcile below writes to SQLite and can fail on its own
                // terms — a damaged page, a locked file, a full disk. Those are
                // faults of the *daemon*, reported once and globally by
                // `dbhealth`; folding them in here is what told a user their
                // repository was unavailable while it sat on disk in perfect
                // health, and took the start/stop controls away with it.
                //
                // Note the direction of the change: `available` is now `true` in
                // strictly more situations than before. That matters for a
                // cached IDE bundle, which keeps reading this field and gating
                // its controls on it — a narrowing that can only turn `false`
                // into `true` needs no coordinated rollout, where a renamed or
                // retyped field would read as `undefined`, hence falsy, hence
                // the incident made permanent in every stale tab.
                match discover_worktrees(&root).await {
                    Ok(discovered) => {
                        if let Err(e) = db.sync_worktrees(&root, &discovered) {
                            crate::dbhealth::note_error(&e);
                            warn!("worktree reconcile failed for {}: {e}", repo.root);
                        }
                        true
                    }
                    // **Log it.** The `repository unavailable` label is what a
                    // user sees, and until now the error explaining it was built
                    // and dropped — by `.is_ok()` before this change, and by this
                    // arm after it. A rewrite of this exact expression is the
                    // moment to stop discarding the only breadcrumb.
                    Err(e) => {
                        warn!("worktree discovery failed for {}: {e:?}", repo.root);
                        false
                    }
                }
            }
        };
        availability.insert(repo.root.clone(), available);
        // Keep the remote-tracking refs fresh enough that the staleness signal
        // tracks the remote, without hammering it: fetch at most once a minute
        // per repo, and only while the IDE is open (this is the UI's poll). A
        // fetch is non-destructive — it only updates remote-tracking refs, it
        // never touches a working tree — which is what makes a background fetch
        // safe where a background *fast-forward* is not.
        maybe_fetch(root.as_path()).await;
        // The rows this repo's rail will show, read once and used twice: the
        // upstream refresh needs them to key its answers by worktree id, and the
        // dirty sweep needs their paths. `repo_view` reads them again from SQLite
        // below; a second local read is cheaper than threading a borrow through it,
        // and this way `list_repos` — which must not spawn git — needs no change.
        let rows = db.list_worktrees(root.as_path()).unwrap_or_default();
        // Synchronous: one `for-each-ref` for the whole repo, so the "not pushed"
        // and "upstream gone" halves of the glyph are as fresh as the poll itself.
        refresh_upstreams(root.as_path(), &rows).await;
        sweep_targets.extend(
            rows.iter()
                // A trashed row shows restore/delete controls rather than state,
                // and a checkout mid-`git worktree remove` is the one place a
                // `git status` races something destructive.
                .filter(|wt| wt.trashed_at.is_empty())
                .map(|wt| (wt.id, wt.path.clone())),
        );
        let git = repo_git_status(&db, &root).await;
        repos.push(repo_view(&db, repo, available, Some(git)).await?);
    }
    // After the loop, and after the response is built: the `dirty` half costs a
    // `git status` per checkout, so it fills the cache for the *next* poll rather
    // than making this one wait for eighteen children.
    spawn_dirty_sweep(sweep_targets);
    if memo.is_none() {
        LAST_SYNC.record(availability);
    }
    Ok(Json(RepoList { repos }))
}

#[derive(Deserialize)]
struct UpdateMainBody {
    root: String,
}

/// Resolve the main checkout's branch — what the IDE means by "main" even
/// when the project calls it something else — and refuse to touch the repo
/// root unless it is actually checked out on that branch.
///
/// Shared by [`update_main`] and [`revert_repo_root`]: a revert that discards
/// uncommitted work only for the fast-forward that follows to refuse anyway
/// for an unrelated reason (wrong branch) destroys work for nothing, so both
/// callers apply the same gate before doing anything destructive. A detached
/// HEAD fails `symbolic-ref` and lands here too.
async fn checked_default_branch(db: &Db, repo_root: &FsPath) -> Result<String, ApiError> {
    let default_branch = db
        .list_worktrees(repo_root)
        .map_err(db_err)?
        .into_iter()
        .find(|w| w.is_main)
        .map(|w| w.branch)
        .ok_or_else(|| err(StatusCode::CONFLICT, "cannot determine the default branch"))?;
    let head_branch = git(repo_root, &["symbolic-ref", "--short", "HEAD"])
        .await
        .map_err(|e| {
            err(
                StatusCode::CONFLICT,
                format!("repo root is not on a branch: {e}"),
            )
        })?;
    if head_branch != default_branch {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "repo root is on \"{head_branch}\", not \"{default_branch}\" — \
                 switch it to update {default_branch}"
            ),
        ));
    }
    Ok(default_branch)
}

/// Bring the repo's main checkout up to date with `origin/<default>`: fetch,
/// then fast-forward-only.
///
/// This is the **one-click "update main"** control — deliberately human-initiated
/// and never scheduled, the anti-guardian position (see
/// `docs/extensions-vision.md`). It is also deliberately narrow about when it
/// will touch the working tree:
///
/// - the main checkout must be *on* the default branch (it fast-forwards the
///   checkout's current branch);
/// - the tree must be clean (`merge --ff-only` would otherwise clobber
///   uncommitted work, and refusing is cheaper than a surprise);
/// - no run may be live in the repo root (a fast-forward mutates files a live
///   run reads config from);
/// - `--ff-only`, so a diverged branch is refused rather than silently rewritten
///   or turned into a merge commit.
///
/// Returns the fresh [`RepoView`] so the top bar's staleness badge clears in the
/// same round trip as the action.
async fn update_main(Json(body): Json<UpdateMainBody>) -> Result<Json<RepoView>, ApiError> {
    let db = open_desktop_db()?;
    let repo = db
        .get_repo(FsPath::new(&body.root))
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "repo not imported"))?;
    let repo_root = PathBuf::from(&repo.root);

    let default_branch = checked_default_branch(&db, &repo_root).await?;
    // Empty porcelain output is a clean tree; anything else is uncommitted work
    // that a fast-forward would fight. Reuses the shared `git_status` helper the
    // worktree delete flow uses (one spelling of `status --porcelain`, not two).
    let dirty = git_status(&repo_root).await.map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git status failed: {e}"),
        )
    })?;
    if !dirty.is_empty() {
        return Err(err(
            StatusCode::CONFLICT,
            "the repo root has uncommitted changes — commit or stash them first",
        ));
    }
    let live = db.live_run_names(&repo_root).map_err(db_err)?;
    if !live.is_empty() {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "a run is live in the repo root ({}) — stop it before updating main",
                live.join(", ")
            ),
        ));
    }
    // Explicit fetch: this is the human asking, so there is no throttle. Then
    // fast-forward-only — a diverged branch is refused, never rewritten.
    git(&repo_root, &["fetch", "origin"]).await.map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("fetch failed: {e}"),
        )
    })?;
    let origin_ref = format!("origin/{default_branch}");
    git(&repo_root, &["merge", "--ff-only", &origin_ref])
        .await
        .map_err(|e| {
            err(
                StatusCode::CONFLICT,
                format!("could not fast-forward {default_branch}: {e}"),
            )
        })?;
    let git = repo_git_status(&db, &repo_root).await;
    Ok(Json(repo_view(&db, repo, true, Some(git)).await?))
}

/// Discard the repo root's uncommitted changes so [`update_main`] can
/// fast-forward it.
///
/// The "revert changes" half of the top bar's dirty confirm — the same trade
/// [`revert_worktree`] makes for a worktree, but reached by repo root rather
/// than worktree id, since that endpoint refuses on the main checkout on
/// purpose. Shares [`update_main`]'s branch and live-run gates: reverting is
/// destructive, so it must refuse under exactly the conditions that would make
/// the fast-forward it exists to unblock refuse anyway — otherwise a dirty
/// repo root on the wrong branch would lose its uncommitted work for nothing.
async fn revert_repo_root(Json(body): Json<UpdateMainBody>) -> Result<Json<StatusView>, ApiError> {
    let db = open_desktop_db()?;
    let repo = db
        .get_repo(FsPath::new(&body.root))
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "repo not imported"))?;
    let repo_root = PathBuf::from(&repo.root);
    // Refuse before doing anything destructive if the fast-forward this revert
    // exists to unblock would refuse anyway (wrong branch): discarding real,
    // uncommitted work only to hit an unrelated refusal a moment later is a
    // pure loss, not progress. See `checked_default_branch`.
    checked_default_branch(&db, &repo_root).await?;
    let live = db.live_run_names(&repo_root).map_err(db_err)?;
    if !live.is_empty() {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "a run is live in the repo root ({}) — stop it before reverting",
                live.join(", ")
            ),
        ));
    }
    revert_git_changes(&repo_root)
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let files = git_status(&repo_root)
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    Ok(Json(StatusView {
        dirty: !files.is_empty(),
        files,
    }))
}

#[derive(Deserialize)]
struct ImportBody {
    /// Any directory inside the repository — the main checkout root is
    /// resolved via git.
    path: String,
}

async fn import_repo(Json(body): Json<ImportBody>) -> Result<Json<RepoView>, ApiError> {
    let given = PathBuf::from(&body.path);
    if !given.is_absolute() {
        return Err(err(StatusCode::BAD_REQUEST, "path must be absolute"));
    }
    let given = given
        .canonicalize()
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("unreadable path: {e}")))?;

    // The main checkout is the first entry of `git worktree list`, regardless
    // of whether the user picked a worktree or a subdirectory.
    let porcelain = git(&given, &["worktree", "list", "--porcelain"])
        .await
        .map_err(|e| {
            err(
                StatusCode::BAD_REQUEST,
                format!("not a git repository: {e}"),
            )
        })?;
    // Same normalization as sync-on-refresh — an import must not store raw
    // paths that the first refresh would then churn into canonical ones.
    let discovered = canonicalize_discovered(parse_worktree_list(&porcelain));
    let Some(main) = discovered.iter().find(|w| w.is_main) else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "repository has no usable checkout (bare repo?)",
        ));
    };
    let root = PathBuf::from(&main.path);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());

    let db = open_desktop_db()?;
    db.upsert_repo(&root, &name).map_err(db_err)?;
    db.sync_worktrees(&root, &discovered).map_err(db_err)?;
    let repo = db
        .get_repo(&root)
        .map_err(db_err)?
        .ok_or_else(|| db_err("repo vanished after import"))?;
    let git = repo_git_status(&db, FsPath::new(&repo.root)).await;
    Ok(Json(repo_view(&db, repo, true, Some(git)).await?))
}

#[derive(Deserialize)]
struct RemoveRepoBody {
    root: String,
}

async fn remove_repo(Json(body): Json<RemoveRepoBody>) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    // Registry-only removal — the filesystem is never touched.
    if db.remove_repo(FsPath::new(&body.root)).map_err(db_err)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(err(StatusCode::NOT_FOUND, "repo not imported"))
    }
}

// ---------------------------------------------------------------------------
// Worktrees
// ---------------------------------------------------------------------------

/// Where a new worktree's checkout comes from.
///
/// A **tagged enum** rather than a handful of optional fields, so the
/// combinations that mean nothing cannot be expressed on the wire at all: a
/// remote ref together with a source worktree, a carry-over with no worktree to
/// carry from, a `remote_ref` on a plain local checkout. Flat optionals would
/// have made each of those a runtime check somebody has to remember to write.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CreateFrom {
    /// Cut `branch` fresh — from `origin/<default>` or the repo's local `HEAD`
    /// per the `git.createFrom` setting. **The default**, and what a client
    /// that predates this field asks for by sending `create_branch: true`.
    NewBranch,
    /// Check out the existing local branch named by `branch`. What
    /// `create_branch: false` means.
    LocalBranch,
    /// Create `branch` from the remote-tracking ref `remote_ref`
    /// (`origin/feat/x`), tracking it.
    RemoteBranch { remote_ref: String },
    /// Cut `branch` from another checkout of the same repo — a spin-off.
    ///
    /// Unpushed commits come along for free, because the branch starts at that
    /// checkout's `HEAD`. `carry_over` is the rest: its staged, unstaged and
    /// untracked work, reproduced in the new checkout (see
    /// [`capture_uncommitted`]).
    ///
    /// **The source is named by `from_path`, never by `worktrees.id`.** That
    /// column is a rowid with no `AUTOINCREMENT`, so SQLite reuses it — the
    /// bug #201 shipped, and the reason `POST /api/worktree-order` is keyed on
    /// paths too. An id would be resolved when the *request* lands, and this
    /// dialog can sit open for minutes: delete the highest-id checkout, create
    /// another, and the reused id sails past both guards below to cut a branch
    /// from a checkout the user never chose — and copy its uncommitted work
    /// out. `worktrees.path` is `UNIQUE` and names one checkout for good.
    Worktree {
        from_path: String,
        #[serde(default)]
        carry_over: bool,
    },
}

#[derive(Deserialize)]
struct CreateWorktreeBody {
    repo_root: String,
    branch: String,
    /// Create `branch` (from the repo's current HEAD) instead of checking out
    /// an existing one.
    ///
    /// Superseded by `source`, and kept because it is the whole wire contract
    /// of every client written before `source` existed. Read only when `source`
    /// is absent.
    #[serde(default)]
    create_branch: bool,
    /// Where the checkout comes from. Absent falls back to `create_branch`.
    #[serde(default)]
    source: Option<CreateFrom>,
    /// Custom alias; defaults to a slug of the branch name.
    #[serde(default)]
    alias: Option<String>,
    /// The free-text name the rail renders. Absent (or `""`) means the rail
    /// shows the alias — which is what the alias-only clients that predate this
    /// field get.
    #[serde(default)]
    display_name: Option<String>,
    /// The rail lane to file the new checkout under, or absent/`""` for
    /// ungrouped.
    ///
    /// On the create request rather than a follow-up PATCH because the rail's
    /// per-lane "＋" is a create *into that lane*: a two-request version has a
    /// window in which the worktree exists in the wrong section, and a failure
    /// between the two leaves it there for good.
    #[serde(default)]
    lane: Option<String>,
    /// Custom checkout path, overriding the `worktree.storageMode`/
    /// `worktree.storageDir` settings. Defaults to
    /// `<storage root>/<project slug>/<alias>`, where the storage root is the
    /// configured storage directory, or `<repo parent>/_worktrees` when none
    /// is configured — see `project_slug`.
    #[serde(default)]
    path: Option<String>,
    /// Marker glyph chosen in the create dialog; the daemon assigns one when absent.
    #[serde(default)]
    emoji: Option<String>,
    /// Marker colour chosen in the create dialog; assigned when absent.
    #[serde(default)]
    marker_color: Option<String>,
}

impl CreateWorktreeBody {
    /// Where the checkout comes from: `source` when the client sent one, else
    /// the `create_branch` boolean that every client written before `source`
    /// existed still sends. One resolution point, so nothing downstream reads
    /// `create_branch` and nothing has to remember which wins.
    ///
    /// **A named method rather than an inline `match`**, because the
    /// maintainer's explicit requirement — the pre-existing default survives —
    /// needs something a test can *call*. It was an inline match with a test
    /// that re-implemented the same three arms in a closure, which passes
    /// whatever the real code happens to do; a review angle correctly called
    /// that a decoy.
    fn create_from(&self) -> &CreateFrom {
        match &self.source {
            Some(s) => s,
            None if self.create_branch => &CreateFrom::NewBranch,
            None => &CreateFrom::LocalBranch,
        }
    }
}

async fn create_worktree(
    Json(body): Json<CreateWorktreeBody>,
) -> Result<Json<CreatedWorktreeView>, ApiError> {
    validate_branch(&body.branch)?;
    let source = body.create_from();
    // A remote ref is passed to git as a start point, so it gets the same
    // shape check the branch does.
    if let CreateFrom::RemoteBranch { remote_ref } = source {
        validate_branch(remote_ref)?;
    }
    if let Some(ref alias) = body.alias {
        validate_alias(alias)?;
    }
    // Trimmed here rather than in the client, so every caller of the API gets the
    // same normalisation; `""` after trimming is the "no separate name" sentinel.
    let display_name = body.display_name.as_deref().map(str::trim);
    if let Some(name) = display_name {
        validate_display_name(name)?;
    }
    // Both marker faces are validated up front, next to the alias, so a rejected
    // glyph cannot leave a checkout on disk that the request then reports as failed.
    if let Some(ref emoji) = body.emoji {
        validate_emoji(emoji)?;
    }
    if let Some(ref color) = body.marker_color {
        validate_marker_color(color)?;
    }

    let db = open_desktop_db()?;
    let repo_root = PathBuf::from(&body.repo_root);
    let repo = db
        .get_repo(&repo_root)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "repo not imported"))?;
    let repo_root = PathBuf::from(&repo.root);

    // An explicit alias that a sibling already holds is rejected before `git
    // worktree add` runs: the definitive check lives in `Db::patch_worktree`,
    // but it only fires on the rename *after* the checkout exists, so failing
    // there leaves a real checkout on disk carrying a branch-derived alias
    // instead of the requested one. This read is racy by nature (a concurrent
    // create, or a sibling on disk not yet synced), so it narrows the window
    // rather than closing it — the rename below is still the authority. A
    // derived alias needs no check at all: `sync_worktrees` suffixes it via
    // `unique_alias`.
    if let Some(ref alias) = body.alias {
        let siblings = db.list_worktrees(&repo_root).map_err(db_err)?;
        // Slug comparison, matching `Db::patch_worktree` — the hostname is
        // `slugify(alias)`, so `main-2` and `main_2` are one name, not two.
        let slug = veld_core::url::slugify(alias);
        if siblings
            .iter()
            .any(|w| veld_core::url::slugify(&w.alias) == slug)
        {
            // Distinct wording from the authoritative post-create 409 in
            // `write_err`: this one guarantees nothing was created, and a
            // client that must decide whether to resync needs to tell them
            // apart.
            return Err(err(
                StatusCode::CONFLICT,
                format!(
                    "another checkout of this repo is already called \"{alias}\" \
                     — nothing was created"
                ),
            ));
        }
    }

    // Same reason as the alias pre-check above, and the same racy-by-nature
    // caveat: `Db::patch_worktree` decides inside its transaction, but it only
    // runs *after* `git worktree add`, so a lane that was never going to be
    // accepted would otherwise cost a checkout on disk filed in the wrong place.
    if let Some(lane) = body.lane.as_deref().filter(|l| !l.is_empty()) {
        let lanes = db.list_lanes(&repo_root).map_err(db_err)?;
        if !lanes.iter().any(|l| l.name == lane) {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "no such lane in this repo — nothing was created",
            ));
        }
    }

    let alias_hint = body
        .alias
        .clone()
        .unwrap_or_else(|| default_alias(&body.branch));
    let checkout_path = match &body.path {
        Some(p) => {
            let p = PathBuf::from(p);
            if !p.is_absolute() {
                return Err(err(StatusCode::BAD_REQUEST, "path must be absolute"));
            }
            p
        }
        // No per-call override: use the configured storage directory
        // (`worktree.storageMode` = `"custom"`), falling back to the sibling
        // `_worktrees` folder when none is configured — today's only behaviour.
        // Existing checkouts already on disk are unaffected either way; this
        // only decides where the *next* one is created. Either root gets a
        // per-project subfolder — see `project_slug` — so two repositories
        // that happen to share both a storage root (always true in `sibling`
        // mode when they share a parent directory; guaranteed in `custom`
        // mode, which funnels every repo into one folder) and an alias can
        // never collide on the same checkout path.
        None => {
            let storage_root = match db.worktree_storage_dir() {
                Some(base) => {
                    // The validator deliberately accepts a directory that
                    // does not exist yet — "an unmounted volume must stay a
                    // savable value" — but that principle is only honest if
                    // *this* endpoint honours it too: `create_dir_all` below
                    // has no opinion about *why* a component is missing, and
                    // would otherwise happily materialise the whole path on
                    // the boot volume while the real one is unmounted,
                    // silently orphaning every worktree's gitdir pointer
                    // once it comes back and shadows or renames over what
                    // was written in its place. `open_worktree_storage_dir`
                    // already 404s on exactly this condition; this brings
                    // create into agreement with it instead of guessing.
                    if !base.is_dir() {
                        return Err(err(
                            StatusCode::BAD_REQUEST,
                            format!(
                                "the configured worktree storage directory {} does not \
                                 exist — check Settings → Git → Worktree storage \
                                 location (an unmounted drive is a common cause)",
                                base.display()
                            ),
                        ));
                    }
                    base
                }
                None => repo_root
                    .parent()
                    .ok_or_else(|| err(StatusCode::BAD_REQUEST, "repo root has no parent"))?
                    .join("_worktrees"),
            };
            // Do not join `alias_hint` onto `storage_root` directly, even
            // for a future per-repo "opt out of nesting" knob: skipping
            // `project_slug` here reopens the exact collision it exists to
            // close, silently, for whichever repo opts out.
            storage_root
                .join(project_slug(&repo_root))
                .join(&alias_hint)
        }
    };
    // A checkout inside *any* imported repository's working tree corrupts
    // that repo's git status permanently: every file under it reads back as
    // an untracked blob (`?? …`), which 409s "update main" forever (its own
    // dirty check, below) and puts every worktree under it in the blast
    // radius of a stray `git clean -fdx` run from that repo's main checkout.
    // Checked against **every** registered repo, not only this one: a
    // configured custom storage directory funnels every repository into one
    // folder, so "inside some *other* repo veld manages" is the likelier
    // shape of this mistake, not a rarer one — the old sibling-per-repo
    // layout made either shape structurally impossible.
    let repos = db.list_repos().map_err(db_err)?;
    if let Some(hit) = repos
        .iter()
        .find(|r| checkout_inside_repo(&checkout_path, FsPath::new(&r.root)))
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!(
                "{} is inside {} — a worktree cannot live inside a repository \
                 veld manages",
                checkout_path.display(),
                hit.root
            ),
        ));
    }
    if checkout_path.exists() {
        return Err(err(
            StatusCode::CONFLICT,
            format!("{} already exists", checkout_path.display()),
        ));
    }
    if let Some(parent) = checkout_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to create {}: {e}", parent.display()),
            )
        })?;
    }

    let path_str = checkout_path.to_string_lossy().into_owned();
    // The four sources, each resolved to a `git worktree add` argv — plus, for
    // a spin-off asked to carry work across, a snapshot taken **before**
    // anything is created, so a checkout that cannot be captured (mid-merge,
    // sparse) is refused with nothing on disk to clean up.
    let mut captured: Option<CapturedWork> = None;
    let git_args: Vec<String> = match source {
        CreateFrom::NewBranch => {
            // Born-current create (`git.createFrom = "origin"`, the default):
            // fetch the remote and cut the new branch from `origin/<default>`
            // rather than the local HEAD, so a worktree is never *born* behind
            // the remote — missing the latest DB migrations, conflicting with
            // open PRs. The compounding failure this addresses is that nobody
            // goes back to update `main`, so each new worktree used to be as
            // stale as the last fetch of main.
            //
            // Falls back to local HEAD (the previous behaviour) when the fetch
            // or the origin ref fails — offline, a repo with no remote, or the
            // main checkout on a branch with no origin counterpart — never
            // silently, but never blocking the create either.
            let mut start_point: Option<String> = None;
            if db.git_create_from() == GitCreateSource::Origin {
                // The default branch is the main checkout's branch — the same
                // thing the "update main" control fast-forwards.
                let default_branch = db
                    .list_worktrees(&repo_root)
                    .map_err(db_err)?
                    .into_iter()
                    .find(|w| w.is_main)
                    .map(|w| w.branch);
                if let Some(dbranch) = default_branch {
                    // Base on origin only if the remote actually has that
                    // branch. The fetch succeeding is not enough: a repo whose
                    // main checkout sits on a local-only branch (never pushed)
                    // would otherwise fail the create against a
                    // `refs/remotes/origin/<branch>` that does not exist,
                    // instead of falling back to local HEAD as the comment
                    // promises. `--quiet` keeps the probe off stderr (the `git`
                    // helper surfaces stderr on failure).
                    if git(&repo_root, &["fetch", "origin"]).await.is_ok()
                        && git(
                            &repo_root,
                            &[
                                "rev-parse",
                                "--verify",
                                "--quiet",
                                &format!("refs/remotes/origin/{dbranch}"),
                            ],
                        )
                        .await
                        .is_ok()
                    {
                        start_point = Some(format!("origin/{dbranch}"));
                    }
                }
            }
            let mut a = vec![
                "worktree".into(),
                "add".into(),
                "-b".into(),
                body.branch.clone(),
                "--".into(),
                path_str.clone(),
            ];
            // `git worktree add -b <branch> <path> <start-point>`. The new
            // branch tracks `origin/<default>` as its upstream, which is what
            // makes a later staleness check against the base well-defined.
            if let Some(sp) = start_point {
                a.push(sp);
            }
            a
        }
        CreateFrom::LocalBranch => vec![
            "worktree".into(),
            "add".into(),
            "--".into(),
            path_str.clone(),
            body.branch.clone(),
        ],
        CreateFrom::RemoteBranch { remote_ref } => {
            // Fetch the ref's own remote first, for the same born-current
            // reason `NewBranch` fetches: a checkout of `origin/feat/x` should
            // start at what the remote has now, not at whatever the last poll
            // happened to bring in. Best-effort — offline is not a reason to
            // refuse a checkout of the ref already on disk.
            // Only after confirming the ref is one this repo actually has.
            // `remote_ref`'s first component is otherwise just a shape-checked
            // string, and `git fetch <name>` treats a name that is not a
            // configured remote as a **URL or path** — so an unchecked value
            // would have the daemon fetch from wherever it pointed. The
            // existence check also makes the failure honest: a typo'd ref
            // fails the create rather than silently skipping the fetch and
            // starting the branch from a stale one.
            let verified = git(
                &repo_root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/remotes/{remote_ref}"),
                ],
            )
            .await
            .is_ok();
            if !verified {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    format!("this repo has no remote-tracking branch {remote_ref}"),
                ));
            }
            // Best-effort from here: offline is not a reason to refuse a
            // checkout of the ref that is already on disk.
            if let Some((remote, _)) = remote_ref.split_once('/') {
                let _ = git(&repo_root, &["fetch", remote]).await;
            }
            // `--track` states the intent rather than relying on
            // `branch.autoSetupMerge`'s default, so the new branch has a
            // well-defined upstream whatever the user's git config says.
            vec![
                "worktree".into(),
                "add".into(),
                "--track".into(),
                "-b".into(),
                body.branch.clone(),
                "--".into(),
                path_str.clone(),
                remote_ref.clone(),
            ]
        }
        CreateFrom::Worktree {
            from_path,
            carry_over,
        } => {
            let src = db
                .get_worktree_by_path(from_path)
                .map_err(db_err)?
                .ok_or_else(|| {
                    err(
                        StatusCode::NOT_FOUND,
                        "no such worktree to branch off — veld knows no checkout at that path",
                    )
                })?;
            // Same repo only. Cutting a branch from another repository's HEAD
            // would produce a checkout whose history has nothing to do with the
            // repo the rail files it under.
            if !std::path::Path::new(&src.repo_root).eq(std::path::Path::new(&repo.root)) {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "that worktree belongs to a different repository",
                ));
            }
            // A trashed checkout is on its way off the disk, so reading its
            // index is a race against `git worktree remove`.
            if !src.trashed_at.is_empty() {
                return Err(err(
                    StatusCode::CONFLICT,
                    "that worktree is in the trash — restore it first",
                ));
            }
            let src_path = PathBuf::from(&src.path);
            // The capture also reads the source's HEAD, and the branch is cut
            // from that exact commit rather than from a second `rev-parse`:
            // the staged/unstaged split only means anything against one HEAD,
            // so a commit landing in the source mid-request must not leave the
            // trees and the branch describing different bases.
            let head = if *carry_over {
                let work = capture_uncommitted(&src_path)
                    .await
                    .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
                let head = work.head.clone();
                captured = Some(work);
                head
            } else {
                git(&src_path, &["rev-parse", "HEAD"])
                    .await
                    .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?
            };
            vec![
                "worktree".into(),
                "add".into(),
                "-b".into(),
                body.branch.clone(),
                "--".into(),
                path_str.clone(),
                head,
            ]
        }
    };
    let git_refs: Vec<&str> = git_args.iter().map(String::as_str).collect();
    git(&repo_root, &git_refs)
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    // Reproduce the source's uncommitted work, now that there is a clean
    // checkout at its HEAD to reproduce it into.
    //
    // **A failure here is reported, not raised.** The checkout exists and is
    // about to be registered, so a 4xx would tell the caller the create failed
    // while leaving a real worktree in the rail — the same shape of lie the
    // alias rename below is careful to avoid. `carry_over.error` is how the
    // client tells "spun off with your changes" from "spun off without them".
    let carry_over = match captured {
        None => None,
        Some(work) => {
            let error = apply_captured(&checkout_path, &work).await.err();
            // Counted from the new checkout rather than from the capture, so
            // the number describes what actually arrived. See
            // `carried_file_count` for why it is not `git_status`.
            let files = carried_file_count(&checkout_path).await;
            Some(CarryOverReport {
                files,
                drifted: work.drifted,
                error,
            })
        }
    };

    let worktrees = sync_repo_worktrees(&db, &repo_root).await?;
    let created = worktrees
        .into_iter()
        // Compare canonicalized: git records its own realpath'd form, while a
        // caller-supplied custom path may reach the same checkout through a
        // symlink or trailing component.
        .find(|w| {
            matches!(
                (
                    std::fs::canonicalize(&w.path),
                    std::fs::canonicalize(&checkout_path),
                ),
                (Ok(a), Ok(b)) if a == b
            )
        })
        .ok_or_else(|| db_err("created worktree missing after sync"))?;
    // The sync assigns a marker and no lane or label; apply what the dialog chose.
    // Before the alias rename below rather than after, because that rename is the
    // step that can lose a race and return early — and a checkout that ends up
    // under its branch-derived alias should still be wearing the marker and the
    // name the user chose.
    //
    // **The lane is deliberately a second write.** Every other field here is
    // already validated and cannot make `patch_worktree` fail, but the lane is
    // checked against the `lanes` table inside that transaction — so folding it in
    // made the whole patch fallible, and a lane deleted between the pre-check
    // above and this call discarded the name and the marker along with it while
    // returning an error that reads like the pre-check's "nothing was created".
    // Split, the failure costs only the thing that actually failed.
    let named = veld_core::db::WorktreePatch {
        display_name,
        emoji: body.emoji.as_deref(),
        marker_color: body.marker_color.as_deref(),
        ..Default::default()
    };
    if !named.is_empty() {
        db.patch_worktree(created.id, named).map_err(write_err)?;
    }
    if let Some(lane) = body.lane.as_deref().filter(|l| !l.is_empty()) {
        db.patch_worktree(
            created.id,
            veld_core::db::WorktreePatch {
                lane: Some(lane),
                ..Default::default()
            },
        )
        .map_err(write_err)?;
    }
    // Re-read rather than patching the local copy field by field: the record this
    // handler returns is what the UI renders straight away, and a hand-merged copy
    // is one forgotten field away from a rail row that only corrects itself on the
    // next poll.
    let created = db
        .get_worktree(created.id)
        .map_err(db_err)?
        .ok_or_else(|| db_err("worktree vanished after applying the dialog's choices"))?;

    // The sync derives the alias from the branch; apply an explicit custom one.
    let created = match &body.alias {
        Some(alias) if *alias != created.alias => {
            // `write_err`, not `db_err`: the pre-check above is racy against a
            // concurrent create/rename, and losing that race is a 409, not a
            // "database error" 500. `sync_repo_worktrees` has already
            // registered the row, so what survives is a registered worktree
            // under its branch-derived alias, not an orphan — the next refresh
            // shows it, and the user can rename it to something free.
            db.rename_worktree(created.id, alias).map_err(write_err)?;
            db.get_worktree(created.id)
                .map_err(db_err)?
                .ok_or_else(|| db_err("worktree vanished after rename"))?
        }
        _ => created,
    };
    Ok(Json(CreatedWorktreeView {
        worktree: worktree_view(&db, created),
        carry_over,
    }))
}

/// Partial update. Both fields are optional so the alias-only callers that
/// predate the emoji field stay wire-compatible; at least one must be present
/// or the request is a no-op worth rejecting.
///
/// `deny_unknown_fields` so a client-side typo (`{"emojii": "🦊"}`) is a 422
/// (axum rejects at deserialization) rather than a silent 200 that changed
/// nothing — with every field optional there is otherwise no signal at all.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchWorktreeBody {
    #[serde(default)]
    alias: Option<String>,
    /// The free-text name the rail renders. `""` clears it, taking the row back
    /// to rendering its alias — so this field distinguishes "leave it alone"
    /// (absent) from "there is no separate name" (empty), which is exactly the
    /// distinction a rename dialog with a clearable field needs.
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    emoji: Option<String>,
    /// The colour half of the marker — a literal `#rrggbb`.
    ///
    /// Independent of `emoji`, and settable while the UI is displaying the *other*
    /// face: both faces are stored permanently, so a user who prefers colours can
    /// still pick their glyph (and vice versa) and find it waiting when they switch
    /// the `worktree.markerStyle` setting.
    #[serde(default)]
    marker_color: Option<String>,
    /// The rail lane to group this worktree under, or `""` to ungroup it.
    ///
    /// A lane name of this repo — validated inside `Db::patch_worktree`'s
    /// transaction, so a concurrent lane deletion cannot slip a dangling name past
    /// it. Assignment rides on the worktree PATCH rather than getting its own
    /// endpoint because `patch_worktree` is the one owner of worktree-row edits.
    #[serde(default)]
    lane: Option<String>,
}

impl PatchWorktreeBody {
    /// Derived from the fields, so adding a fifth can't leave the
    /// "nothing to update" guard silently behind.
    fn is_empty(&self) -> bool {
        let Self {
            alias,
            display_name,
            emoji,
            marker_color,
            lane,
        } = self;
        alias.is_none()
            && display_name.is_none()
            && emoji.is_none()
            && marker_color.is_none()
            && lane.is_none()
    }
}

async fn patch_worktree(
    Path(id): Path<i64>,
    Json(body): Json<PatchWorktreeBody>,
) -> Result<Json<WorktreeView>, ApiError> {
    if body.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "nothing to update: send an alias, a display_name, an emoji, a marker_color, \
             a lane, or any combination",
        ));
    }
    // Validate everything before touching the database: a request carrying a
    // good alias and a bad emoji must change neither.
    if let Some(alias) = &body.alias {
        validate_alias(alias)?;
    }
    let display_name = body.display_name.as_deref().map(str::trim);
    if let Some(name) = display_name {
        validate_display_name(name)?;
    }
    if let Some(emoji) = &body.emoji {
        validate_emoji(emoji)?;
    }
    if let Some(color) = &body.marker_color {
        validate_marker_color(color)?;
    }

    let db = open_desktop_db()?;
    // One write for every column, and the alias-collision check shares its
    // transaction — see `Db::patch_worktree`.
    let existed = db
        .patch_worktree(
            id,
            veld_core::db::WorktreePatch {
                alias: body.alias.as_deref(),
                display_name,
                emoji: body.emoji.as_deref(),
                marker_color: body.marker_color.as_deref(),
                lane: body.lane.as_deref(),
            },
        )
        .map_err(write_err)?;
    if !existed {
        return Err(err(StatusCode::NOT_FOUND, "worktree not found"));
    }
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    Ok(Json(worktree_view(&db, wt)))
}

#[derive(Deserialize)]
struct DeleteQuery {
    /// Remove the checkout even with modified or untracked files
    /// (`git worktree remove --force`).
    ///
    /// Deliberately **not** persisted with the trash state — a crash must not
    /// silently upgrade a later removal to one that discards uncommitted work, and
    /// forcing is a decision worth re-taking rather than inheriting. Nothing retries
    /// it either: a removal interrupted by the daemon going away is not resumed at
    /// all, forced or not, and the worktree stays in the trash for the user to ask
    /// again (see `worktree_trash::recover`).
    #[serde(default)]
    force: bool,
}

/// Move a worktree to the trash — or, with `?force=true`, delete it outright.
///
/// Binning deletes nothing: it marks the row and returns. The checkout stays on
/// disk, restoring it is a real undo, and `git worktree remove` runs when the
/// retention period expires (the GC pass) or when the user asks for it now
/// (`POST /api/worktrees/{id}/delete`). This used to await the removal inline, which
/// froze the UI for as long as a large checkout took.
///
/// `force` remains inline and immediate: it exists to get past a refusal the user has
/// already been shown, so the answer they need is whether *this* attempt worked.
async fn delete_worktree(
    Path(id): Path<i64>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    let wt = db
        .trash_worktree(id)
        .map_err(write_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;

    if !q.force {
        // Nothing is queued and nothing is deleted: the checkout stays on disk until
        // its retention expires or the user asks for it now. This is why the request
        // is fast — there is no slow work left in it at all.
        return Ok(StatusCode::ACCEPTED);
    }

    // Forced removal stays inline: the user has already been told why the
    // un-forced attempt failed and has chosen to discard the changes, so the
    // answer they need is whether *this* attempt worked.
    //
    // It does NOT stop runs first — the background worker is what does that, and
    // awaiting a teardown inside a request is the freeze this batch removed. So
    // refuse instead of quietly deleting the directory out from under a live run:
    // `--force` is about discarding *file* changes, and letting it also mean "and
    // kill whatever is running in there" is a promise the dialog's copy does not
    // make. In practice this is a safety net rather than a common path, because the
    // un-forced attempt that produced the refusal already stopped the runs.
    // Untrash on the error path too, not just on the refusal below. `?` here would
    // return a 500 having already set `trashed_at`, silently binning a worktree the
    // user asked to delete outright and giving no sign the request failed — every
    // other exit from this function releases the row.
    let live = match db.live_run_names(FsPath::new(&wt.path)) {
        Ok(live) => live,
        Err(e) => {
            let _ = db.untrash_worktree(id, "");
            return Err(db_err(e));
        }
    };
    if let Some(name) = live.first() {
        let _ = db.untrash_worktree(id, "");
        return Err(err(
            StatusCode::CONFLICT,
            format!("environment \"{name}\" is still running in this worktree — stop it first"),
        ));
    }

    // Through the same single owner the worker uses, so it inherits the
    // deletion guard instead of being a second unguarded path — which is exactly
    // what it was, and what round 3 of the review found.
    match super::worktree_trash::delete_checkout_forced(&db, &wt).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(reason) => {
            // Back out of the trash with the reason, as the worker would — otherwise
            // a failed force leaves a row that looks like pending work forever.
            let _ = db.untrash_worktree(id, &reason);
            Err(err(StatusCode::UNPROCESSABLE_ENTITY, reason))
        }
    }
}

/// Report the git dirty state of a worktree: the files that would stop
/// `git worktree remove` from succeeding, and why.
///
/// Deliberately a separate, on-demand endpoint rather than a field on
/// `WorktreeView`: the listing is polled by every IDE window, and running git
/// in every checkout on every poll is the kind of cost that only shows up as a
/// slow rail. The trash/delete flow fetches it when a decision is being made.
///
/// The read-only contract (`GET`s on this router have no side effects) is what
/// makes it safe to call here — no CSRF header needed, and no state written.
async fn worktree_status(Path(id): Path<i64>) -> Result<Json<StatusView>, ApiError> {
    let db = open_desktop_db()?;
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    let files = git_status(FsPath::new(&wt.path))
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    Ok(Json(StatusView {
        dirty: !files.is_empty(),
        files,
    }))
}

/// Discard a worktree's uncommitted changes so its deletion can succeed.
///
/// This is the "revert the changes" half of the trash/delete flow: it resets
/// tracked files (staged and unstaged) to HEAD with `git restore`, then removes
/// untracked files and directories with `git clean -fd`. **Ignored files are
/// deliberately left alone** — `git worktree remove` does not refuse on those
/// (verified, git 2.50), so removing them would discard work for nothing.
///
/// Destructive by nature, so it is gated the same way the force-delete path is:
/// only on a non-main worktree, and only on an explicit request. The caller is
/// expected to have shown the file list first — this endpoint does not ask a
/// second question, but the UI that reaches it must. It returns the post-revert
/// status so the caller can confirm the checkout is now clean rather than
/// assuming the commands succeeded.
///
/// **Why not use `git worktree remove --force`?** Forcing discards the files as
/// a side effect of deletion; this is the *reversible-in-spirit* alternative
/// that leaves the worktree itself intact and lets the user change their mind
/// (restore, or keep it) afterwards. The two answer different questions, and the
/// trash flow now offers both.
async fn revert_worktree(Path(id): Path<i64>) -> Result<Json<StatusView>, ApiError> {
    let db = open_desktop_db()?;
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    if wt.is_main {
        return Err(err(
            StatusCode::CONFLICT,
            "the main checkout is never reverted — it is the repository itself",
        ));
    }
    revert_git_changes(FsPath::new(&wt.path))
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let files = git_status(FsPath::new(&wt.path))
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    Ok(Json(StatusView {
        dirty: !files.is_empty(),
        files,
    }))
}

/// Discard a worktree's uncommitted changes: reset tracked files (staged and
/// unstaged) to HEAD, then remove untracked files and directories.
///
/// Split out of [`revert_worktree`] so the destructive sequence can be pinned
/// against real git rather than trusted to a hand-written command list.
///
/// **Ignored files are deliberately left alone** — `git worktree remove` does
/// not refuse on them (verified, git 2.50), so removing them would discard work
/// for nothing. Hence `clean -fd`, not `clean -fdx`.
async fn revert_git_changes(path: &FsPath) -> Result<(), String> {
    // Reset the index and working tree to HEAD: clears staged additions and
    // staged/unstaged modifications to tracked files alike.
    git(path, &["restore", "--staged", "--worktree", "."]).await?;
    // Remove untracked files and directories. No `-x`: that would also remove
    // ignored files, which `git worktree remove` does not refuse on and which
    // may be the user's own tooling or notes.
    git(path, &["clean", "-fd"]).await?;
    Ok(())
}

/// Take a worktree out of the trash (undo).
///
/// A real undo for the whole retention period, since binning deletes nothing. It can
/// still lose a race against an explicit "delete now" already in the worker, which is
/// why it reports whether the row was there rather than assuming it was.
async fn restore_worktree(Path(id): Path<i64>) -> Result<Json<WorktreeView>, ApiError> {
    let db = open_desktop_db()?;
    // Refuse rather than lie. Once a deletion has started the directory is going and
    // no database write brings it back, so clearing `trashed_at` here would hand back
    // a live-looking row for a checkout that disappears moments later — the silent
    // loss the trash exists to prevent. The check and the write share one lock, so
    // the deletion cannot start between them.
    if !super::worktree_trash::try_restore(&db, id).map_err(db_err)? {
        return Err(err(
            StatusCode::CONFLICT,
            "this worktree is already being deleted",
        ));
    }
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree already removed"))?;
    Ok(Json(worktree_view(&db, wt)))
}

/// Delete a trashed worktree now, without waiting for its retention to expire.
///
/// Queues the same worker the retention sweep uses, so there is exactly one code path
/// that ever runs `git worktree remove`. Returns `409` for a worktree that is not in
/// the trash: emptying the bin is not a shortcut around the confirmation that puts
/// things in it.
async fn delete_trashed_worktree(Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    if wt.trashed_at.is_empty() {
        return Err(err(
            StatusCode::CONFLICT,
            "this worktree is not in the trash",
        ));
    }
    super::worktree_trash::enqueue(wt.id);
    Ok(StatusCode::ACCEPTED)
}

/// Empty the trash: delete every trashed worktree of a repo now.
async fn empty_trash(Query(q): Query<RepoQuery>) -> Result<Json<serde_json::Value>, ApiError> {
    let db = open_desktop_db()?;
    let trashed: Vec<i64> = db
        .list_worktrees(FsPath::new(&q.repo_root))
        .map_err(db_err)?
        .into_iter()
        .filter(|w| !w.trashed_at.is_empty())
        .map(|w| w.id)
        .collect();
    for id in &trashed {
        super::worktree_trash::enqueue(*id);
    }
    Ok(Json(serde_json::json!({ "queued": trashed.len() })))
}

/// Clear a recorded removal failure — the user has read it.
async fn dismiss_trash_error(Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    db.clear_trash_error(id).map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct WorktreeOrderBody {
    repo_root: String,
    /// The full order the client is displaying, as worktree **paths**.
    ///
    /// Paths, never ids: `worktrees.id` is a rowid and SQLite reuses it, so an
    /// id-keyed order outlives the worktree and lands on the next one created
    /// (#201). Sending the whole list rather than a move-one delta keeps the write
    /// idempotent — omitted paths go back to unplaced.
    order: Vec<String>,
}

async fn reorder_worktrees(Json(body): Json<WorktreeOrderBody>) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    db.reorder_worktrees(FsPath::new(&body.repo_root), &body.order)
        .map_err(write_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Rail lanes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RepoQuery {
    repo_root: String,
}

async fn list_lanes(Query(q): Query<RepoQuery>) -> Result<Json<serde_json::Value>, ApiError> {
    let db = open_desktop_db()?;
    let lanes = db.list_lanes(FsPath::new(&q.repo_root)).map_err(db_err)?;
    Ok(Json(serde_json::json!({ "lanes": lanes })))
}

#[derive(Deserialize)]
struct LaneBody {
    repo_root: String,
    name: String,
}

async fn create_lane(Json(body): Json<LaneBody>) -> Result<Json<serde_json::Value>, ApiError> {
    let db = open_desktop_db()?;
    let lane = db
        .create_lane(FsPath::new(&body.repo_root), &body.name)
        .map_err(lane_err)?;
    Ok(Json(serde_json::json!({ "lane": lane })))
}

#[derive(Deserialize)]
struct RenameLaneBody {
    repo_root: String,
    name: String,
}

async fn rename_lane(
    Path(from): Path<String>,
    Json(body): Json<RenameLaneBody>,
) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    let existed = db
        .rename_lane(FsPath::new(&body.repo_root), &from, &body.name)
        .map_err(lane_err)?;
    if !existed {
        return Err(err(StatusCode::NOT_FOUND, "lane not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_lane(
    Path(name): Path<String>,
    Query(q): Query<RepoQuery>,
) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    let existed = db
        .delete_lane(FsPath::new(&q.repo_root), &name)
        .map_err(db_err)?;
    if !existed {
        return Err(err(StatusCode::NOT_FOUND, "lane not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RepoOrderBody {
    /// The full project order the client is displaying, as repo **roots**. Roots
    /// the caller omits keep their relative order after the ones it lists, so a
    /// client that polled before a project was imported cannot unplace it.
    order: Vec<String>,
}

async fn reorder_repos(Json(body): Json<RepoOrderBody>) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    // `write_err`, not `db_err`: an over-long order is a *client* error, and its two
    // siblings already answer 400 with the limit in the message. Under `db_err` the
    // caller was told "database error" with a 500 — the UI's toast would then blame
    // the daemon for a request it had itself made too large. `write_err`'s other arms
    // are inert on this path; only the `OrderTooLong` one can fire.
    db.reorder_repos(&body.order).map_err(write_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct LaneOrderBody {
    repo_root: String,
    /// The full lane order the client is displaying, as lane **names** — lanes are
    /// identified by `(repo_root, name)` and have no id. Names the caller omits keep
    /// their relative order after the ones it lists.
    order: Vec<String>,
}

async fn reorder_lanes(Json(body): Json<LaneOrderBody>) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    db.reorder_lanes(FsPath::new(&body.repo_root), &body.order)
        .map_err(lane_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Lane-name rejections are client errors, not database errors.
///
/// Same posture as [`write_err`]: the message is fixed and the offending value goes
/// only to the log, because echoing unbounded client input back into a response
/// body is a habit worth not starting.
fn lane_err(e: veld_core::db::DbError) -> ApiError {
    use veld_core::db::DbError;
    match e {
        DbError::LaneTaken(_) => {
            warn!("rejected lane name: {e}");
            err(
                StatusCode::CONFLICT,
                "this repo already has a lane with that name",
            )
        }
        DbError::InvalidLaneName(_) => {
            warn!("rejected lane name: {e}");
            err(
                StatusCode::BAD_REQUEST,
                format!(
                    "a lane name must be 1–{} characters",
                    veld_core::db::MAX_LANE_NAME_LEN
                ),
            )
        }
        DbError::TooManyLanes(max) => err(
            StatusCode::CONFLICT,
            format!("this repo already has the maximum of {max} lanes"),
        ),
        DbError::OrderTooLong(_) => {
            warn!("rejected oversized reorder: {e}");
            err(
                StatusCode::BAD_REQUEST,
                format!(
                    "a reorder may list at most {} entries",
                    veld_core::db::MAX_ORDER_LEN
                ),
            )
        }
        other => db_err(other),
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct StartBody {
    #[serde(default)]
    preset: Option<String>,
    /// Explicit `node:variant` selections — the alternative to a preset for
    /// configs without presets (or custom picks). Mutually exclusive with
    /// `preset`; the UI always sends one of the two. With neither, a non-TTY
    /// `veld start` starts the project's `default_preset` if one is declared, and
    /// otherwise fails "No selections provided" — so an empty body is a spawn,
    /// not reliably a no-op.
    #[serde(default)]
    selections: Vec<String>,
    /// Run name; defaults to the worktree alias.
    #[serde(default)]
    run_name: Option<String>,
}

/// Start a veld run in a worktree by spawning `veld start` with the worktree
/// as cwd (the CLI resolves the root config from there) — the same fire-and-forget
/// pattern as the management stop/restart endpoints. Returns 202; the UI
/// observes progress via `/api/environments`.
async fn start_worktree_run(
    Path(id): Path<i64>,
    Json(body): Json<StartBody>,
) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    let wt_path = PathBuf::from(&wt.path);
    if veld_core::config::root_config_in(&wt_path).is_none() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "worktree has no veld.json or veld.jsonc — nothing to start",
        ));
    }

    let run_name = body.run_name.clone().unwrap_or_else(|| wt.alias.clone());
    validate_run_name(&run_name).map_err(|c| err(c, "invalid run name"))?;
    // Refuse a start whose environment is already live, rather than taking it over.
    //
    // `veld start` replaces a live same-named run on purpose — that is the CLI's
    // documented behaviour and stays. Through this endpoint it is never what anyone
    // asked for, because the caller is a UI that computed the name from a run list
    // it polled up to `POLL_MS` ago. Two ways that goes wrong, both real: ▶ on an
    // environment the UI believes has ended, restarted by an agent in the gap, and
    // two windows (or the top bar and the rail's context menu) independently
    // computing the same next-free name and both posting it. Either way the loser
    // is killed silently, mid-session, with no prompt — and the client cannot close
    // the race itself, because it is holding stale data by construction.
    //
    // 409, so the caller can say "that name is taken" instead of the generic
    // failure toast.
    let live = db.live_run_names(FsPath::new(&wt.path)).map_err(db_err)?;
    if live.iter().any(|n| n == &run_name) {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "environment '{run_name}' is already running here — stop or restart it, \
                 or start another under a different name"
            ),
        ));
    }
    let mut args = vec!["start".to_owned()];
    for sel in &body.selections {
        // `node:variant` — both halves identifier-safe.
        let valid = match sel.split_once(':') {
            Some((n, v)) => is_safe_identifier(n) && is_safe_identifier(v),
            None => is_safe_identifier(sel),
        };
        if !valid {
            return Err(err(StatusCode::BAD_REQUEST, "invalid node selection"));
        }
        args.push(sel.clone());
    }
    args.push("--name".to_owned());
    args.push(run_name);
    if let Some(preset) = &body.preset {
        if !is_safe_identifier(preset) {
            return Err(err(StatusCode::BAD_REQUEST, "invalid preset name"));
        }
        args.push("--preset".to_owned());
        args.push(preset.clone());
    }

    let code = spawn_veld(&wt_path, &args).await;
    match code {
        StatusCode::ACCEPTED => Ok(StatusCode::ACCEPTED),
        // `spawn_veld` refuses while `veld update` holds the update lock, and
        // nothing was spawned. Saying "failed to spawn veld start" here would
        // throw away the only part of that answer a user can act on: this is
        // temporary, it will clear in a minute or two, and retrying is the fix.
        // The IDE renders this string in a toast, so it is the whole message.
        StatusCode::SERVICE_UNAVAILABLE => Err(err(
            code,
            "a veld update is in progress — try again when it finishes",
        )),
        _ => Err(err(code, "failed to spawn veld start")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- `select_news` (`news.source`) ---------------------------------------

    fn wt_record(id: i64, is_main: bool) -> WorktreeRecord {
        WorktreeRecord {
            id,
            repo_root: "/repo".to_owned(),
            path: format!("/repo/wt{id}"),
            branch: "main".to_owned(),
            alias: format!("wt{id}"),
            display_name: String::new(),
            emoji: String::new(),
            marker_color: String::new(),
            is_main,
            created_at: String::new(),
            lane: String::new(),
            sort_position: None,
            trashed_at: String::new(),
            trash_error: String::new(),
        }
    }

    fn wt_view(id: i64, is_main: bool, news: Vec<veld_core::ide::NewsItem>) -> WorktreeView {
        WorktreeView {
            worktree: wt_record(id, is_main),
            deleting: false,
            has_veld_config: true,
            presets: None,
            nodes: Vec::new(),
            machine_vars: None,
            git: None,
            ide: IdeView {
                news,
                ..Default::default()
            },
        }
    }

    fn news_item(id: &str) -> veld_core::ide::NewsItem {
        veld_core::ide::NewsItem {
            id: id.to_owned(),
            since: "2026-01-01".to_owned(),
            eyebrow: "Eyebrow".to_owned(),
            headline: "Headline".to_owned(),
            body: "One sentence.".to_owned(),
            glyph: "inbox".to_owned(),
        }
    }

    #[test]
    fn select_news_main_takes_only_the_main_checkouts_cards() {
        let mut worktrees = vec![
            wt_view(1, true, vec![news_item("a")]),
            wt_view(2, false, vec![news_item("b")]),
        ];
        let selected = select_news(&mut worktrees, ConfigSource::Main);
        assert_eq!(
            selected.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec!["a"],
            "a non-main worktree's own news must never reach the wire in the default mode"
        );
    }

    #[test]
    fn select_news_worktree_unions_ids_declared_only_on_one_checkout() {
        let mut worktrees = vec![
            wt_view(1, true, vec![news_item("a")]),
            wt_view(2, false, vec![news_item("a"), news_item("b")]),
        ];
        let selected = select_news(&mut worktrees, ConfigSource::Worktree);
        assert_eq!(
            selected.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"],
        );
    }

    #[test]
    fn select_news_worktree_previews_an_edit_to_an_already_merged_card() {
        // The literal use case `news.source = worktree` exists for: a card with
        // id "a" already merged on main, and a branch editing its wording before
        // merging that edit. The worktree's version must win — otherwise nothing
        // ever previews and the setting does not do what it says.
        let mut main_copy = news_item("a");
        main_copy.body = "old wording, already on main".to_owned();
        let mut draft = news_item("a");
        draft.body = "new wording, being edited on this branch".to_owned();

        let mut worktrees = vec![
            wt_view(1, true, vec![main_copy]),
            wt_view(2, false, vec![draft]),
        ];
        let selected = select_news(&mut worktrees, ConfigSource::Worktree);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].body, "new wording, being edited on this branch");
    }

    #[test]
    fn select_news_worktree_main_survives_a_same_day_flood_from_other_worktrees() {
        // Main declares one card; enough other worktrees each declare one
        // distinct, same-`since` card to exceed the cap on their own. Main's
        // card must still survive — main-first insertion plus the reversed
        // tie-break both have to hold for this, not just one of them.
        let mut worktrees = vec![wt_view(1, true, vec![news_item("main-card")])];
        for i in 0..veld_core::ide::MAX_NEWS_ITEMS {
            worktrees.push(wt_view(
                2 + i as i64,
                false,
                vec![news_item(&format!("flood-{i}"))],
            ));
        }
        let selected = select_news(&mut worktrees, ConfigSource::Worktree);
        assert_eq!(selected.len(), veld_core::ide::MAX_NEWS_ITEMS);
        assert!(
            selected.iter().any(|n| n.id == "main-card"),
            "main's own card must not be the one a same-day flood from other worktrees drops"
        );
    }

    #[test]
    fn select_news_worktree_skips_trashed_checkouts() {
        let mut draft = news_item("a");
        let mut trashed = wt_view(2, false, vec![news_item("gone")]);
        trashed.worktree.trashed_at = "2026-08-13T00:00:00Z".to_owned();
        draft.id = "kept".to_owned();
        let mut worktrees = vec![
            wt_view(1, true, vec![]),
            wt_view(3, false, vec![draft]),
            trashed,
        ];
        let selected = select_news(&mut worktrees, ConfigSource::Worktree);
        assert_eq!(
            selected.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec!["kept"],
            "a trashed worktree's draft cards must not reach the wire"
        );
    }

    #[test]
    fn select_news_worktree_still_respects_the_project_wide_cap() {
        let mut worktrees: Vec<WorktreeView> = (0..(veld_core::ide::MAX_NEWS_ITEMS + 3))
            .map(|i| wt_view(i as i64, i == 0, vec![news_item(&format!("n{i}"))]))
            .collect();
        let selected = select_news(&mut worktrees, ConfigSource::Worktree);
        assert_eq!(
            selected.len(),
            veld_core::ide::MAX_NEWS_ITEMS,
            "unioning every worktree's news must not bypass the endpoint's own cap"
        );
    }

    // -- `worktree_view` extensions (`extensions.source`) --------------------

    fn open_test_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
        (dir, db)
    }

    /// The critical case this setting exists for, end to end: a worktree with
    /// **no `veld.json` of its own** — the onboarding gap, not a hypothetical —
    /// must still see main's `ide.extensions` under the default, and must not
    /// borrow them when opted into `worktree` mode. An earlier version of this
    /// change wired the setting into the `status`/`activate` endpoints but left
    /// this exact listing sourced from the viewed worktree unconditionally, so
    /// the onboarding fix did not actually reach the UI — caught in review, not
    /// after.
    #[test]
    fn worktree_view_extensions_come_from_main_when_this_worktree_predates_them() {
        let (_db_dir, db) = open_test_db();
        let repo_dir = tempfile::TempDir::new().expect("tempdir");
        let main_path = repo_dir.path().join("main");
        let old_path = repo_dir.path().join("old-worktree");
        std::fs::create_dir_all(&main_path).unwrap();
        std::fs::create_dir_all(&old_path).unwrap();
        std::fs::write(
            main_path.join("veld.json"), // root-config-gate-ok
            r#"{"schemaVersion": "3", "name": "t", "nodes": {}, "ide": {"extensions": [
                {"id": "pr", "slot": "topBar", "type": "action", "label": "PR", "argv": ["true"]}
            ]}}"#,
        )
        .unwrap();
        // `old_path` deliberately has no `veld.json` at all.

        let discovered = vec![
            DiscoveredWorktree {
                path: main_path.to_string_lossy().into_owned(),
                branch: "main".to_owned(),
                is_main: true,
            },
            DiscoveredWorktree {
                path: old_path.to_string_lossy().into_owned(),
                branch: "old".to_owned(),
                is_main: false,
            },
        ];
        db.upsert_repo(repo_dir.path(), "repo").unwrap();
        db.sync_worktrees(repo_dir.path(), &discovered).unwrap();
        let old_wt = db
            .list_worktrees(repo_dir.path())
            .unwrap()
            .into_iter()
            .find(|w| !w.is_main)
            .expect("the non-main row");

        // Default (`main`): the old worktree renders main's extension.
        let view = worktree_view(&db, old_wt.clone());
        assert_eq!(
            view.ide.extensions.len(),
            1,
            "a worktree with no veld.json of its own must still see main's extensions by default"
        );
        assert_eq!(view.ide.extensions[0].id, "pr");

        // `worktree` mode: the old worktree has nothing of its own to show.
        db.patch_settings(
            &[(
                "extensions.source".to_owned(),
                serde_json::Value::from("worktree"),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
        let view = worktree_view(&db, old_wt);
        assert!(
            view.ide.extensions.is_empty(),
            "worktree mode must not borrow main's declarations"
        );
    }

    /// The fail-closed case: `main` mode with no `is_main` row at all for this
    /// repo (a bare primary clone — see `extensions::resolve_declare_root`'s
    /// doc comment) must show no extensions, never silently fall back to the
    /// requested worktree's own — which would reopen the exact threat `main`
    /// mode exists to close.
    #[test]
    fn worktree_view_extensions_fail_closed_when_no_main_checkout_can_be_found() {
        let (_db_dir, db) = open_test_db();
        let repo_dir = tempfile::TempDir::new().expect("tempdir");
        let only_path = repo_dir.path().join("only-worktree");
        std::fs::create_dir_all(&only_path).unwrap();
        std::fs::write(
            only_path.join("veld.json"), // root-config-gate-ok
            r#"{"schemaVersion": "3", "name": "t", "nodes": {}, "ide": {"extensions": [
                {"id": "own", "slot": "topBar", "type": "action", "label": "Own", "argv": ["true"]}
            ]}}"#,
        )
        .unwrap();

        // `is_main: false` on the only worktree — the shape `parse_worktree_list`
        // produces for a bare primary clone, where `is_main` is consumed and then
        // the block is skipped, leaving no `is_main` row for the repo at all.
        let discovered = vec![DiscoveredWorktree {
            path: only_path.to_string_lossy().into_owned(),
            branch: "feat".to_owned(),
            is_main: false,
        }];
        db.upsert_repo(repo_dir.path(), "repo").unwrap();
        db.sync_worktrees(repo_dir.path(), &discovered).unwrap();
        let wt = db
            .list_worktrees(repo_dir.path())
            .unwrap()
            .into_iter()
            .next()
            .expect("the only row");
        assert!(!wt.is_main, "test setup: this repo must have no main row");

        let view = worktree_view(&db, wt);
        assert!(
            view.ide.extensions.is_empty(),
            "no resolvable main checkout must fail closed, not fall back to this \
             worktree's own (and clearly-untrusted-by-construction) declarations"
        );
    }

    // -- Process-global guards ----------------------------------------------

    #[test]
    fn single_flight_admits_one_and_releases_on_drop() {
        let gate = SingleFlight::new();
        let first = gate.try_enter().expect("nobody is inside yet");
        assert!(
            gate.try_enter().is_none(),
            "a second entrant must be refused while the first holds it"
        );
        drop(first);
        assert!(
            gate.try_enter().is_some(),
            "the gate must reopen when the guard drops"
        );
    }

    #[test]
    fn single_flight_releases_when_the_holder_unwinds() {
        // The property the handler actually relies on: every early return — and a
        // panic in a backend — has to leave the gate open, or the endpoint answers
        // 409 forever. `catch_unwind` is the only way to assert that here.
        let gate = SingleFlight::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _open = gate.try_enter().expect("free");
            panic!("a picker backend blew up");
        }));
        assert!(result.is_err(), "the panic must not be swallowed");
        assert!(
            gate.try_enter().is_some(),
            "unwinding past the guard must still release it"
        );
    }

    #[test]
    fn debounce_serves_the_memo_inside_the_window_and_nothing_outside_it() {
        use std::time::Duration;
        let debounce: Debounce<u32> = Debounce::new();
        assert_eq!(
            debounce.fresh_within(Duration::from_secs(60)),
            None,
            "nothing recorded yet is a miss, not a stale hit"
        );

        debounce.record(7);
        assert_eq!(
            debounce.fresh_within(Duration::from_secs(60)),
            Some(7),
            "a value recorded now is inside any real window"
        );
        // A zero window is "always due" — the same branch a two-second-old entry
        // takes, without making the test sleep for it.
        assert_eq!(debounce.fresh_within(Duration::ZERO), None);

        debounce.record(9);
        assert_eq!(
            debounce.fresh_within(Duration::from_secs(60)),
            Some(9),
            "recording again replaces the memo and restarts the window"
        );
    }

    #[test]
    fn debounce_hands_every_caller_the_same_answer() {
        // Why the memo exists: concurrent pollers inside one window must not each
        // compute their own availability, because the cheap check and the git
        // reconcile disagree exactly when a repo is in trouble.
        use std::collections::HashMap;
        use std::time::Duration;
        let debounce: Debounce<HashMap<String, bool>> = Debounce::new();
        debounce.record(HashMap::from([("/repo".to_string(), false)]));
        let a = debounce.fresh_within(Duration::from_secs(60));
        let b = debounce.fresh_within(Duration::from_secs(60));
        assert_eq!(a, b);
        assert_eq!(a.and_then(|m| m.get("/repo").copied()), Some(false));
    }

    #[test]
    fn porcelain_parsing_marks_main_and_detached() {
        let out = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree /wts/chk\nHEAD def\nbranch refs/heads/feat/checkout-v2\n\n\
                   worktree /wts/spike\nHEAD 123\ndetached\n";
        let wts = parse_worktree_list(out);
        assert_eq!(wts.len(), 3);
        assert!(wts[0].is_main);
        assert_eq!(wts[0].branch, "main");
        assert!(!wts[1].is_main);
        assert_eq!(wts[1].branch, "feat/checkout-v2");
        assert_eq!(wts[2].branch, "(detached)");
    }

    #[test]
    fn porcelain_parsing_skips_bare_but_keeps_first_flag() {
        // A bare main entry is skipped and must NOT shift the main flag onto
        // the first real worktree.
        let out = "worktree /repo.git\nbare\n\n\
                   worktree /wts/a\nHEAD abc\nbranch refs/heads/a\n";
        let wts = parse_worktree_list(out);
        assert_eq!(wts.len(), 1);
        assert!(!wts[0].is_main);
    }

    #[test]
    fn porcelain_parsing_skips_prunable_entries() {
        // Git keeps a worktree's admin entry under `.git/worktrees/<n>/` after the
        // checkout is gone and reports it as `prunable` until `git worktree prune`
        // runs — whose default expiry is `gc.worktreePruneExpire`, three months.
        // Treating it as discovered kept the row alive for that whole window, so a
        // worktree deleted outside veld sat in the rail pointing at nothing.
        // Verified against real `git worktree list --porcelain` output (git 2.50).
        let out = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree /wts/gone\nHEAD def\nbranch refs/heads/gone\n\
                   prunable gitdir file points to non-existent location\n\n\
                   worktree /wts/live\nHEAD 123\nbranch refs/heads/live\n";
        let wts = parse_worktree_list(out);
        assert_eq!(wts.len(), 2);
        assert_eq!(wts[0].path, "/repo");
        assert!(wts[0].is_main);
        assert_eq!(wts[1].path, "/wts/live");
        assert!(!wts[1].is_main, "the skip must not promote a worktree");
    }

    #[test]
    fn porcelain_parsing_skips_a_prunable_main_without_promoting() {
        // Same rule as the bare case: consuming `first` before the skip is what
        // stops the next worktree inheriting main-ness.
        let out = "worktree /repo\nHEAD abc\nprunable gitdir file points nowhere\n\n\
                   worktree /wts/a\nHEAD abc\nbranch refs/heads/a\n";
        let wts = parse_worktree_list(out);
        assert_eq!(wts.len(), 1);
        assert!(!wts[0].is_main);
    }

    /// The reported shape: a worktree blocked from deletion carries a mix of
    /// tracked modifications, staged additions, deletions, untracked files and
    /// renames. `-z` records are NUL-separated, so the fixture is built with the
    /// literal `\0`, and the rename's origin (`src.txt`) must not be rendered as
    /// a file of its own.
    #[test]
    fn git_status_parses_the_files_that_block_removal() {
        let out = [
            " M a.txt",       // unstaged modification
            "M  staged.txt",  // staged modification
            "A  new.txt",     // staged addition
            " D gone.txt",    // unstaged deletion
            "RM renamed.txt", // staged+unstaged rename; origin follows
            "src.txt",        // the rename's origin — must be skipped
            "?? untracked/",  // untracked directory
        ]
        .join("\0");
        let files = parse_git_status(&out);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "a.txt",
                "staged.txt",
                "new.txt",
                "gone.txt",
                "renamed.txt",
                "untracked/"
            ],
            "the rename origin must not appear as a file"
        );
        let kinds: Vec<&str> = files.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                "modified",
                "modified",
                "added",
                "deleted",
                "renamed",
                "untracked"
            ]
        );
    }

    #[test]
    fn git_status_is_empty_for_a_clean_worktree() {
        assert!(parse_git_status("").is_empty());
        // A lone trailing NUL (git emits one after the last record) is not a file.
        assert!(parse_git_status("\0").is_empty());
    }

    #[test]
    fn git_status_labels_conflicts_distinct_from_modifications() {
        // All the unmerged codes carry a `U` in one position; the test pins that
        // they are not misread as plain modifications.
        for code in ["UU", "AA", "DD", "AU", "UD", "UA", "DU"] {
            assert_eq!(
                parse_git_status(&format!("{code} f\0"))[0].kind,
                "conflicted",
                "unmerged code {code} must be labelled conflicted"
            );
        }
    }

    #[test]
    fn stable_hash_is_pinned_against_silent_drift() {
        // This exact value is the contract, not an implementation detail — it
        // decides which folder a repository's worktrees keep landing in
        // forever. An algorithm swap, or `project_slug` changing which 32
        // bits it truncates to, must fail a test rather than silently
        // re-bucketing every project already in use, which a mere
        // determinism-within-one-run test (below) cannot catch: the old and
        // new algorithm would each agree with themselves.
        assert_eq!(stable_hash(b"/veld-review-fixture"), 1772533236475490894);
    }

    #[test]
    fn project_slug_falls_back_to_repo_when_slugify_empties_the_basename() {
        // A basename with no ASCII alphanumerics (e.g. entirely CJK) slugifies
        // to "" (see `url::slugify`) — the same degenerate case the
        // `file_name() == None` fallback exists for, just reached a different
        // way. Unhandled, the bucket name would start with the hash's `-`.
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("日本語");
        std::fs::create_dir_all(&repo).unwrap();
        let slug = project_slug(&repo);
        assert!(
            slug.starts_with("repo-"),
            "expected a `repo-` fallback, got {slug:?}"
        );
    }

    #[test]
    fn project_slug_is_deterministic_even_when_canonicalize_fails() {
        // `canonicalize()` fails when the path does not exist — exercised here
        // by never creating it. The repository could vanish between import and
        // a later worktree create (an unmounted network volume, a moved
        // directory); the fallback must still produce a stable answer rather
        // than panicking or silently re-bucketing on every call.
        let ghost = FsPath::new("/definitely/does/not/exist/veld-review-fixture");
        let first = project_slug(ghost);
        assert_eq!(first, project_slug(ghost));
    }

    #[test]
    fn checkout_inside_repo_refuses_a_path_under_the_working_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_root = dir.path().join("proj");
        std::fs::create_dir_all(&repo_root).unwrap();

        // The exact shape review found: a storage root nested inside the repo
        // it stores worktrees for.
        let nested = repo_root.join("store").join("proj-abc123").join("feat");
        assert!(checkout_inside_repo(&nested, &repo_root));

        // A true sibling — today's default shape — must not trip it.
        let sibling = dir
            .path()
            .join("_worktrees")
            .join("proj-abc123")
            .join("feat");
        assert!(!checkout_inside_repo(&sibling, &repo_root));
    }

    #[test]
    fn checkout_inside_repo_catches_a_dot_dot_bypass() {
        // `ghost` never exists, so plain `canonicalize()` stops at the temp
        // root and leaves `ghost/../proj/...` as the unresolved suffix —
        // exactly what `create_dir_all` would later turn real by literally
        // creating `ghost`, at which point `..` resolves and the checkout
        // always was inside `proj`. The comparison has to apply that `..`
        // itself, since nothing on disk has yet.
        let dir = tempfile::TempDir::new().unwrap();
        let repo_root = dir.path().join("proj");
        std::fs::create_dir_all(&repo_root).unwrap();

        let sneaky = dir
            .path()
            .join("ghost")
            .join("..")
            .join("proj")
            .join("store")
            .join("feat");
        assert!(checkout_inside_repo(&sneaky, &repo_root));
    }

    #[test]
    fn pick_directory_prompt_is_a_fixed_set_never_free_text() {
        assert_eq!(pick_directory_prompt(None), "Choose a git repository");
        assert_eq!(
            pick_directory_prompt(Some("worktree-storage")),
            "Choose a folder for worktree checkouts"
        );
        // Anything unrecognised — including an attempt to smuggle a quote or
        // a newline through the query string — degrades to the original
        // default rather than being echoed anywhere.
        assert_eq!(
            pick_directory_prompt(Some("\") -- pwned")),
            "Choose a git repository"
        );
    }

    #[test]
    fn project_slug_disambiguates_same_named_repos_in_different_places() {
        // The exact scenario a bare basename cannot handle: two different
        // directories that happen to share a name — the failure mode a
        // shared custom storage directory turns from theoretical into common.
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        let repo_a = a.path().join("backend");
        let repo_b = b.path().join("backend");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();

        let slug_a = project_slug(&repo_a);
        let slug_b = project_slug(&repo_b);
        assert_ne!(slug_a, slug_b, "different repos must not share a bucket");
        // Both still start with the human-readable basename — only the
        // machine-plumbing suffix differs.
        assert!(slug_a.starts_with("backend-"));
        assert!(slug_b.starts_with("backend-"));

        // Deterministic: the same repo must land in the same bucket every
        // time, across process restarts and daemon upgrades — new worktrees
        // for a project already in use must keep joining it.
        assert_eq!(slug_a, project_slug(&repo_a));
    }

    /// The generated-status path, run against **real git**, not a hand-built
    /// fixture: ``git_status`` is what the endpoint serves, and this pins that a
    /// plain unstaged edit is reported just as reliably as a staged one (a
    /// regression would show only staged changes, because those have no leading
    /// space in the porcelain code).
    #[tokio::test]
    async fn git_status_reports_plain_edits_and_staged_changes_against_real_git() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = FsPath::new(dir.path());
        // A tracked file, two commits apart is irrelevant — one commit suffices.
        run_git(root, &["init", "-q"]).await;
        run_git(root, &["config", "user.email", "t@t"]).await;
        run_git(root, &["config", "user.name", "t"]).await;
        std::fs::write(root.join("tracked.txt"), "one").unwrap();
        run_git(root, &["add", "tracked.txt"]).await;
        run_git(root, &["commit", "-qm", "init"]).await;

        // Clean: nothing in the way.
        assert!(git_status(root).await.unwrap().is_empty());

        // A plain edit (unstaged, ` M`) — the case reported as not detected.
        std::fs::write(root.join("tracked.txt"), "one-two").unwrap();
        let unstaged = git_status(root).await.unwrap();
        assert_eq!(unstaged.len(), 1, "a plain edit must be reported dirty");
        assert_eq!(unstaged[0].path, "tracked.txt");
        assert_eq!(unstaged[0].kind, "modified");

        // Same file staged (`M `), still one file.
        run_git(root, &["add", "tracked.txt"]).await;
        let staged = git_status(root).await.unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].kind, "modified");

        // A brand-new file nobody has `git add`ed (`??`) joins the list; the
        // staged `tracked.txt` is still there, so the total is two.
        std::fs::write(root.join("untracked.txt"), "new").unwrap();
        let untracked = git_status(root).await.unwrap();
        assert!(
            untracked
                .iter()
                .any(|f| f.path == "untracked.txt" && f.kind == "untracked")
        );
    }

    /// The destructive half, pinned against real git: after a revert the
    /// worktree must be clean enough for `git worktree remove`, with staged,
    /// unstaged and untracked changes all gone — but **ignored** files kept.
    #[tokio::test]
    async fn revert_git_changes_discards_uncommitted_but_keeps_ignored() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = FsPath::new(dir.path());
        run_git(root, &["init", "-q"]).await;
        run_git(root, &["config", "user.email", "t@t"]).await;
        run_git(root, &["config", "user.name", "t"]).await;
        std::fs::write(root.join("tracked.txt"), "one").unwrap();
        std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        run_git(root, &["add", "."]).await;
        run_git(root, &["commit", "-qm", "init"]).await;

        // Make it dirty in every way a deletion can be blocked: unstaged edit,
        // staged addition, and an untracked directory.
        std::fs::write(root.join("tracked.txt"), "one-two").unwrap(); // unstaged
        std::fs::write(root.join("staged.txt"), "s").unwrap();
        run_git(root, &["add", "staged.txt"]).await; // staged addition
        std::fs::create_dir(root.join("scratch")).unwrap();
        std::fs::write(root.join("scratch/x"), "x").unwrap(); // untracked
        std::fs::write(root.join("keep.log"), "k").unwrap(); // ignored — survives

        assert_eq!(git_status(root).await.unwrap().len(), 3);
        revert_git_changes(root)
            .await
            .expect("revert should succeed");
        assert!(
            git_status(root).await.unwrap().is_empty(),
            "revert must leave nothing blocking a delete"
        );
        assert!(
            root.join("keep.log").exists(),
            "ignored files are not blocked by remove and must survive"
        );
    }

    /// A rename's `<original>` field is not a file, and the guard that skips it
    /// tested the wrong column: git detects renames against the **index**, so
    /// the code is `R ` and byte 1 is a space.
    ///
    /// Most origin paths were rejected by accident anyway — the
    /// malformed-record guard needs a space at **byte 2**, i.e. a
    /// two-character prefix. The ones that got through reported a file that
    /// does not exist, in the trash dialog's "what is in the way" list and in
    /// the count a spin-off reports back.
    ///
    /// **Every assertion here uses a name that actually gets through**, and
    /// that is not a detail: an earlier version of this test used
    /// `IMG 1234.jpg`, whose space is at byte *3*, so the guard rejected it and
    /// the assertion passed against the bug it was written for. A review round
    /// caught that by compiling the pre-fix parser and running it. Keep the
    /// prefixes two characters long, or this test stops testing anything.
    #[test]
    fn a_rename_origin_is_never_reported_as_a_file_of_its_own() {
        // `git mv "PR review notes.md" notes.md` — an ordinary filename whose
        // third character is a space, which is all it takes.
        let got = parse_git_status("R  notes.md\0PR review notes.md\0");
        assert_eq!(
            got,
            vec![DirtyFile {
                path: "notes.md".to_owned(),
                kind: "renamed",
            }],
            "the origin path must not become a second file"
        );
        // A copy, same shape.
        assert_eq!(
            // `\u{0}` rather than `\0` here only because the origin path
            // starts with a digit, and `"\001 Track.mp3"` reads like an octal
            // escape to a human even though Rust has none.
            parse_git_status("C  new.txt\u{0}01 Track.mp3\0").len(),
            1,
            "a copy's origin is not a file either"
        );
        // Renamed *and* then modified in the worktree — `R` in the index
        // column, `M` in the worktree column, still one path.
        assert_eq!(
            parse_git_status("RM notes.md\0PR review notes.md\0").len(),
            1,
            "a renamed-then-edited file is still one path"
        );
        // The accidentally-safe shapes stay safe: a short origin path, and a
        // longer prefix whose space falls past byte 2.
        assert_eq!(
            parse_git_status("R  notes.md\0a.md\0").len(),
            1,
            "a short origin path was already skipped, and must stay skipped"
        );
        assert_eq!(
            parse_git_status("R  photo.jpg\0IMG 1234.jpg\0").len(),
            1,
            "and a three-character prefix never reached the bug in the first \
             place — kept so the byte-2 boundary is written down"
        );
    }

    /// Wrapper so the integration test reads as assertions rather than shell.
    async fn run_git(dir: &FsPath, args: &[&str]) {
        git(dir, args).await.expect("git should succeed");
    }

    // -----------------------------------------------------------------------
    // Branch listing
    // -----------------------------------------------------------------------

    #[test]
    fn local_branches_report_which_checkout_holds_them() {
        // Tab-separated `<short>\t<upstream>\t<worktreepath>`, and the path is
        // last so a directory with a tab in its name cannot shift the fields.
        // The real shape: `%00`-terminated records, each followed by
        // `for-each-ref`'s own newline. `odd` holds both a tab *and* a newline
        // in its path — the two characters a refname cannot contain and a path
        // can, and the reason for both the field order and the record
        // separator.
        let out = "main\torigin/main\t/repo\0\n\
                   feat/x\torigin/feat/x\t\0\n\
                   local-only\t\t\0\n\
                   odd\t\t/repo/we\tir\nd\0\n";
        let got = parse_local_branches(out);
        assert_eq!(
            got,
            vec![
                LocalBranchView {
                    name: "main".into(),
                    checked_out_in: Some("/repo".into()),
                    upstream: Some("origin/main".into()),
                },
                LocalBranchView {
                    name: "feat/x".into(),
                    checked_out_in: None,
                    upstream: Some("origin/feat/x".into()),
                },
                LocalBranchView {
                    name: "local-only".into(),
                    checked_out_in: None,
                    upstream: None,
                },
                LocalBranchView {
                    name: "odd".into(),
                    checked_out_in: Some("/repo/we\tir\nd".into()),
                    upstream: None,
                },
            ]
        );
    }

    #[test]
    fn remote_branches_skip_the_symref_and_keep_slashes_in_the_local_name() {
        // `refs/remotes/origin/HEAD` shortens to the bare remote name and
        // carries a symref — it is not a branch and must not become a row.
        let out = "origin\trefs/remotes/origin/main\0\n\
                   origin/main\t\0\n\
                   origin/feat/x\t\0\n\
                   upstream/main\t\0\n";
        let local = vec![LocalBranchView {
            name: "main".into(),
            checked_out_in: None,
            upstream: None,
        }];
        let got = parse_remote_branches(out, &local);
        assert_eq!(
            got,
            vec![
                RemoteBranchView {
                    name: "origin/main".into(),
                    // A local `main` exists, so creating it from the remote ref
                    // would fail — the picker needs to know before the click.
                    local_name: "main".into(),
                    has_local: true,
                },
                RemoteBranchView {
                    // `split_once`, not `rsplit_once`: `feat/x` must survive
                    // whole rather than becoming `x`.
                    name: "origin/feat/x".into(),
                    local_name: "feat/x".into(),
                    has_local: false,
                },
                RemoteBranchView {
                    name: "upstream/main".into(),
                    local_name: "main".into(),
                    has_local: true,
                },
            ]
        );
    }

    // -----------------------------------------------------------------------
    // Spin-off carry-over
    // -----------------------------------------------------------------------

    /// Build a repo whose dirty state covers every combination the two-tree
    /// model has to represent, and return its path.
    async fn dirty_repo(root: &FsPath) {
        run_git(root, &["init", "-q"]).await;
        run_git(root, &["config", "user.email", "t@t"]).await;
        run_git(root, &["config", "user.name", "t"]).await;
        std::fs::write(root.join("edited.txt"), "one").unwrap();
        std::fs::write(root.join("binary.bin"), [0u8, 1, 2, 255]).unwrap();
        std::fs::write(root.join("run.sh"), "#!/bin/sh\n").unwrap();
        // Actually executable, so git records mode 100755 and the assertion
        // below is testing the mechanism rather than a 0644 file it created.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        std::fs::write(root.join("gone.txt"), "bye").unwrap();
        std::fs::write(root.join("unstaged-delete.txt"), "bye too").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("edited.txt", root.join("link.txt")).unwrap();
        run_git(root, &["add", "-A"]).await;
        run_git(root, &["commit", "-qm", "init"]).await;

        // Unstaged edit, including a binary one.
        std::fs::write(root.join("edited.txt"), "one-two").unwrap();
        std::fs::write(root.join("binary.bin"), [255u8, 254, 0, 7]).unwrap();
        // Staged addition.
        std::fs::write(root.join("staged.txt"), "s").unwrap();
        run_git(root, &["add", "staged.txt"]).await;
        // Staged deletion whose file is still on disk, so it reads as a staged
        // delete *and* an untracked file.
        run_git(root, &["rm", "-q", "--cached", "gone.txt"]).await;
        // Unstaged deletion.
        std::fs::remove_file(root.join("unstaged-delete.txt")).unwrap();
        // Untracked, one of them nested.
        std::fs::write(root.join("untracked.txt"), "u").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/deep.txt"), "d").unwrap();
        // Ignored — must NOT come across. This is the whole reason the capture
        // is `git add` and not a directory copy.
        std::fs::write(root.join("ignored.txt"), "never").unwrap();
    }

    /// The mechanism, end to end against real git: a spin-off's checkout must
    /// come out with the **same `git status`** as its source — the
    /// staged/unstaged split included — while the source is left byte-identical.
    #[tokio::test]
    async fn a_spin_off_reproduces_every_shape_of_uncommitted_work() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("main");
        std::fs::create_dir(&src).unwrap();
        let src = FsPath::new(&src);
        dirty_repo(src).await;

        let before = git_status(src).await.unwrap();
        assert!(
            !before.iter().any(|f| f.path == "ignored.txt"),
            "the ignored file must not even be dirty in the source"
        );

        // The source's index file, before the capture reads it.
        let git_dir = PathBuf::from(
            git(src, &["rev-parse", "--absolute-git-dir"])
                .await
                .unwrap(),
        );
        let index_before = std::fs::read(git_dir.join("index")).unwrap();

        let work = capture_uncommitted(src).await.expect("capture should work");

        // **The source is untouched.** This is the constraint that rules out
        // `git stash create`, which rewrites this file under its lock.
        assert_eq!(
            std::fs::read(git_dir.join("index")).unwrap(),
            index_before,
            "the capture must not write the source's index"
        );
        assert_eq!(
            git_status(src).await.unwrap(),
            before,
            "the capture must not change the source's status"
        );

        let dest = dir.path().join("spin");
        run_git(
            src,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "spin",
                "--",
                dest.to_str().unwrap(),
                &work.head,
            ],
        )
        .await;
        let dest = FsPath::new(&dest);
        apply_captured(dest, &work)
            .await
            .expect("apply should work");

        assert_eq!(
            git_status(dest).await.unwrap(),
            before,
            "the spin-off's status must match the source's, staged/unstaged split included"
        );
        // Spot-checks that a matching status alone would not prove.
        assert_eq!(
            std::fs::read(dest.join("binary.bin")).unwrap(),
            vec![255u8, 254, 0, 7],
            "binary contents must survive"
        );
        assert!(
            !dest.join("ignored.txt").exists(),
            "an ignored file must never be carried across"
        );
        assert!(
            !dest.join("unstaged-delete.txt").exists(),
            "a file deleted in the source must be absent here too"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("sub/deep.txt")).unwrap(),
            "d",
            "a nested untracked file must arrive"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::read_link(dest.join("link.txt")).unwrap(),
                std::path::Path::new("edited.txt"),
                "a symlink must arrive as a symlink, not as its target's bytes"
            );
            assert!(
                std::fs::metadata(dest.join("run.sh"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111
                    != 0,
                "the executable bit must survive"
            );
        }
        assert!(!work.drifted, "nothing wrote the source during the capture");
    }

    /// A clean source is a legitimate spin-off: the capture must succeed and
    /// the new checkout must come out clean, not emptied.
    #[tokio::test]
    async fn a_spin_off_of_a_clean_checkout_carries_nothing_and_breaks_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("main");
        std::fs::create_dir(&src).unwrap();
        let src = FsPath::new(&src);
        run_git(src, &["init", "-q"]).await;
        run_git(src, &["config", "user.email", "t@t"]).await;
        run_git(src, &["config", "user.name", "t"]).await;
        std::fs::write(src.join("a.txt"), "a").unwrap();
        run_git(src, &["add", "-A"]).await;
        run_git(src, &["commit", "-qm", "init"]).await;

        let work = capture_uncommitted(src).await.expect("capture should work");
        let dest = dir.path().join("spin");
        run_git(
            src,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "spin",
                "--",
                dest.to_str().unwrap(),
                &work.head,
            ],
        )
        .await;
        let dest = FsPath::new(&dest);
        apply_captured(dest, &work)
            .await
            .expect("apply should work");
        assert!(
            git_status(dest).await.unwrap().is_empty(),
            "a clean source must produce a clean spin-off"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("a.txt")).unwrap(),
            "a",
            "and must not have emptied the checkout"
        );
    }

    /// The drift signal must actually fire, and the case it has to catch is the
    /// one a `git status` comparison cannot see: the **contents** of a file
    /// that was already modified changing under the capture. Porcelain status
    /// is byte-identical across that, which is how the first version of this
    /// signal came out `false` in exactly the situation it existed for.
    ///
    /// Driven through [`stage_everything`] — the seam `capture_uncommitted`
    /// compares — rather than by racing a writer against a real capture. A race
    /// would be flaky in both directions, and a non-atomic rewrite makes `git
    /// add` fail with `short read while indexing` instead of producing a
    /// different tree, so the race tests something other than the comparison.
    #[tokio::test]
    async fn drift_is_detected_when_an_already_modified_file_changes_under_the_capture() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = FsPath::new(dir.path());
        // The scratch index lives OUTSIDE the checkout on purpose — inside, it
        // would itself be an untracked file that `add -A` captures, so the
        // second staging would differ from the first every time and `drifted`
        // would be permanently true. (Measured; it is the shape of the bug this
        // test would otherwise have hidden.)
        let scratch = tempfile::TempDir::new().unwrap();
        let index = scratch.path().join("full.index");

        run_git(root, &["init", "-q"]).await;
        run_git(root, &["config", "user.email", "t@t"]).await;
        run_git(root, &["config", "user.name", "t"]).await;
        std::fs::write(root.join("f.txt"), "one").unwrap();
        run_git(root, &["add", "-A"]).await;
        run_git(root, &["commit", "-qm", "init"]).await;
        std::fs::write(root.join("f.txt"), "two").unwrap();

        let git_dir = PathBuf::from(
            git(root, &["rev-parse", "--absolute-git-dir"])
                .await
                .unwrap(),
        );
        std::fs::copy(git_dir.join("index"), &index).unwrap();
        let index = FsPath::new(&index);

        let first = stage_everything(root, index).await.unwrap();

        // A quiet source must produce the same tree twice, or the signal is
        // noise on every create.
        assert_eq!(
            stage_everything(root, index).await.unwrap(),
            first,
            "a source nobody is writing must not read as drifted"
        );

        // An mtime change with no content change must not either — otherwise a
        // build that touches files without changing them flags every spin-off.
        run_git(root, &["status", "--porcelain"]).await;
        std::fs::write(root.join("f.txt"), "two").unwrap();
        assert_eq!(
            stage_everything(root, index).await.unwrap(),
            first,
            "a rewrite with identical content must not read as drifted"
        );

        // **The premise**, pinned: the status of the two moments is identical,
        // which is why this signal is not a status comparison.
        let before = git_raw(root, &["status", "--porcelain=v1", "-z"])
            .await
            .unwrap();
        std::fs::write(root.join("f.txt"), "three-and-then-some-more").unwrap();
        let after = git_raw(root, &["status", "--porcelain=v1", "-z"])
            .await
            .unwrap();
        assert_eq!(
            before, after,
            "porcelain status cannot see a content-only change to an \
             already-modified file — if this ever fails, the drift signal could \
             go back to being a status comparison"
        );

        // And the tree can.
        assert_ne!(
            stage_everything(root, index).await.unwrap(),
            first,
            "a content change to an already-modified file must read as drifted"
        );

        // End to end: a capture of a quiet checkout reports no drift.
        assert!(
            !capture_uncommitted(root).await.unwrap().drifted,
            "capture_uncommitted must not report drift on a quiet source"
        );
    }

    /// The sparse-checkout refusal, which is a documented promise and was the
    /// only guard of the two without a test. Its cost if it ever regresses is
    /// the worst in this feature: `add -A` reads out-of-cone files as
    /// deletions, so the captured tree would tell the new checkout to delete
    /// every path outside the cone.
    #[tokio::test]
    async fn a_sparse_checkout_refuses_the_carry_over_rather_than_emptying_the_new_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = FsPath::new(dir.path());
        run_git(root, &["init", "-q"]).await;
        run_git(root, &["config", "user.email", "t@t"]).await;
        run_git(root, &["config", "user.name", "t"]).await;
        std::fs::create_dir(root.join("kept")).unwrap();
        std::fs::create_dir(root.join("dropped")).unwrap();
        std::fs::write(root.join("kept/a.txt"), "a").unwrap();
        std::fs::write(root.join("dropped/b.txt"), "b").unwrap();
        run_git(root, &["add", "-A"]).await;
        run_git(root, &["commit", "-qm", "init"]).await;

        // Clean first: the guard must be the thing that refuses, not the
        // absence of anything to carry.
        assert!(capture_uncommitted(root).await.is_ok());

        run_git(root, &["sparse-checkout", "set", "kept"]).await;
        assert!(
            !root.join("dropped/b.txt").exists(),
            "the setup must actually have removed the out-of-cone file"
        );
        let e = capture_uncommitted(root)
            .await
            .expect_err("a sparse checkout must be refused");
        assert!(
            e.contains("sparse"),
            "the refusal must name the reason: {e}"
        );
    }

    /// The reported count is **paths, not porcelain fields**, and a rename is
    /// two fields for one path. Driven through real git rather than a fixture,
    /// because the bug was a hand-rolled count that a fixture written by the
    /// same hand would have agreed with.
    #[tokio::test]
    async fn the_carried_count_is_paths_not_status_fields() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = FsPath::new(dir.path());
        run_git(root, &["init", "-q", "-b", "main"]).await;
        run_git(root, &["config", "user.email", "t@t"]).await;
        run_git(root, &["config", "user.name", "t"]).await;
        std::fs::write(root.join("a.txt"), "a").unwrap();
        run_git(root, &["add", "-A"]).await;
        run_git(root, &["commit", "-qm", "init"]).await;

        // One rename (two fields) plus one untracked file (one field): three
        // NUL-delimited fields, two changed paths.
        run_git(root, &["mv", "a.txt", "b.txt"]).await;
        std::fs::write(root.join("c.txt"), "c").unwrap();
        let raw = git_raw(root, &["status", "--porcelain=v1", "-z", "-uall"])
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&raw)
                .split('\0')
                .filter(|f| !f.is_empty())
                .count(),
            3,
            "the premise: git emits a rename as two fields, so a raw field \
             count is not a path count"
        );
        assert_eq!(
            carried_file_count(root).await,
            Some(2),
            "a renamed file is one carried path, not two"
        );

        // And the untracked-directory expansion the flag exists for, in the
        // same breath — one record from plain porcelain, three paths here.
        std::fs::create_dir(root.join("newdir")).unwrap();
        for f in ["x", "y", "z"] {
            std::fs::write(root.join("newdir").join(f), f).unwrap();
        }
        assert_eq!(
            carried_file_count(root).await,
            Some(5),
            "an untracked directory counts as its files, not as one entry"
        );
    }

    /// `--track` on the remote-branch argv is not decoration, and nothing
    /// exercised it: a contributor reading `git worktree add -b x … origin/x`
    /// would reasonably conclude git infers the upstream anyway. It does — but
    /// only while `branch.autoSetupMerge` is at its default, which is a user
    /// setting. Pinned against real git with that setting turned off, which is
    /// the configuration where dropping the flag actually loses the upstream.
    #[tokio::test]
    async fn a_remote_branch_checkout_tracks_its_remote_even_with_autosetupmerge_off() {
        let dir = tempfile::TempDir::new().unwrap();
        let origin = dir.path().join("origin.git");
        let main = dir.path().join("main");
        let spin = dir.path().join("spun");
        run_git(
            FsPath::new(dir.path()),
            &["init", "-q", "--bare", origin.to_str().unwrap()],
        )
        .await;
        std::fs::create_dir(&main).unwrap();
        let main = FsPath::new(&main);
        run_git(main, &["init", "-q", "-b", "main"]).await;
        run_git(main, &["config", "user.email", "t@t"]).await;
        run_git(main, &["config", "user.name", "t"]).await;
        // The setting that makes the flag load-bearing.
        run_git(main, &["config", "branch.autoSetupMerge", "false"]).await;
        std::fs::write(main.join("a.txt"), "a").unwrap();
        run_git(main, &["add", "-A"]).await;
        run_git(main, &["commit", "-qm", "init"]).await;
        run_git(main, &["remote", "add", "origin", origin.to_str().unwrap()]).await;
        run_git(main, &["push", "-q", "origin", "main:feat/remote-only"]).await;
        run_git(main, &["fetch", "-q", "origin"]).await;

        // The existence check the handler performs before it fetches — the
        // guard that stops `git fetch <name>` being handed something git would
        // treat as a URL. Both directions, since only the negative one is
        // load-bearing and only the positive one keeps the feature working.
        assert!(
            git(
                main,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/remotes/origin/feat/remote-only"
                ]
            )
            .await
            .is_ok(),
            "the fetched ref must be verifiable, or the handler would refuse a valid create"
        );
        assert!(
            git(
                main,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/remotes/nosuch/branch"
                ]
            )
            .await
            .is_err(),
            "an unknown ref must fail the check rather than reaching `git fetch`"
        );

        // The handler's argv for this source, verbatim.
        run_git(
            main,
            &[
                "worktree",
                "add",
                "-q",
                "--track",
                "-b",
                "feat/remote-only",
                "--",
                spin.to_str().unwrap(),
                "origin/feat/remote-only",
            ],
        )
        .await;
        let upstream = git(FsPath::new(&spin), &["rev-parse", "--abbrev-ref", "@{u}"])
            .await
            .expect("the new branch must have an upstream");
        assert_eq!(
            upstream, "origin/feat/remote-only",
            "the checkout must track the remote branch it came from"
        );
    }

    /// A worktree path can contain a newline, and `for-each-ref` records used
    /// to be split on lines — which produced a phantom branch row named after
    /// the path's second half. Driven through **real git output** rather than a
    /// fixture, because the fixture is exactly the thing that was wrong.
    #[tokio::test]
    async fn a_worktree_path_containing_a_newline_does_not_invent_a_branch() {
        let dir = tempfile::TempDir::new().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir(&main).unwrap();
        let main = FsPath::new(&main);
        run_git(main, &["init", "-q", "-b", "main"]).await;
        run_git(main, &["config", "user.email", "t@t"]).await;
        run_git(main, &["config", "user.name", "t"]).await;
        std::fs::write(main.join("a.txt"), "a").unwrap();
        run_git(main, &["add", "-A"]).await;
        run_git(main, &["commit", "-qm", "init"]).await;

        let odd = dir.path().join("we\nird");
        run_git(
            main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/odd",
                "--",
                odd.to_str().unwrap(),
            ],
        )
        .await;

        let raw = git(
            main,
            &[
                "for-each-ref",
                "--format=%(refname:short)\t%(upstream:short)\t%(worktreepath)%00",
                "refs/heads",
            ],
        )
        .await
        .unwrap();
        let got = parse_local_branches(&raw);
        let names: Vec<&str> = got.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["feat/odd", "main"],
            "exactly the two real branches — a newline in a checkout path must \
             not split one record into two"
        );
        let odd_row = got.iter().find(|b| b.name == "feat/odd").unwrap();
        assert!(
            odd_row
                .checked_out_in
                .as_deref()
                .is_some_and(|p| p.contains('\n')),
            "and the path must arrive whole, newline included: {:?}",
            odd_row.checked_out_in
        );
    }

    /// `write-tree` cannot represent an unmerged index, so the capture refuses
    /// **before** anything is created rather than failing halfway.
    #[tokio::test]
    async fn a_checkout_mid_merge_refuses_the_carry_over_instead_of_half_doing_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = FsPath::new(dir.path());
        run_git(root, &["init", "-q", "-b", "main"]).await;
        run_git(root, &["config", "user.email", "t@t"]).await;
        run_git(root, &["config", "user.name", "t"]).await;
        std::fs::write(root.join("f.txt"), "base").unwrap();
        run_git(root, &["add", "-A"]).await;
        run_git(root, &["commit", "-qm", "base"]).await;
        run_git(root, &["checkout", "-q", "-b", "side"]).await;
        std::fs::write(root.join("f.txt"), "side").unwrap();
        run_git(root, &["commit", "-qam", "side"]).await;
        run_git(root, &["checkout", "-q", "main"]).await;
        std::fs::write(root.join("f.txt"), "mainline").unwrap();
        run_git(root, &["commit", "-qam", "mainline"]).await;
        // Expected to fail — that is what leaves the conflict stages behind.
        let _ = git(root, &["merge", "side"]).await;
        assert!(
            !git(root, &["ls-files", "--unmerged"])
                .await
                .unwrap()
                .is_empty(),
            "the setup must actually have produced an unmerged index"
        );

        let e = capture_uncommitted(root)
            .await
            .expect_err("a mid-merge checkout must be refused");
        assert!(
            e.contains("middle of a merge"),
            "the refusal must say why, not surface git's `error building trees`: {e}"
        );
    }

    #[test]
    fn branch_validation() {
        assert!(validate_branch("feat/checkout-v2").is_ok());
        assert!(validate_branch("-oops").is_err());
        assert!(validate_branch("a b").is_err());
        assert!(validate_branch("a..b").is_err());
        assert!(validate_branch("").is_err());
    }

    #[test]
    fn alias_validation_rejects_dot_dirs() {
        assert!(validate_alias("chk").is_ok());
        assert!(validate_alias("checkout-v2").is_ok());
        assert!(validate_alias(".").is_err());
        assert!(validate_alias("..").is_err());
        assert!(validate_alias("a/b").is_err());
        assert!(validate_alias("").is_err());
    }

    #[test]
    fn display_name_validation_bounds_characters_not_bytes() {
        // Empty is legal and load-bearing: it is the "no separate name" sentinel,
        // and the only way back to rendering the alias.
        assert!(validate_display_name("").is_ok());
        assert!(validate_display_name("Checkout V2 (final)").is_ok());

        // **Characters, not bytes.** `len()` here would pass every ASCII case
        // above and silently give a German or Japanese name a third of the cap.
        assert!(validate_display_name(&"ü".repeat(MAX_DISPLAY_NAME_LEN)).is_ok());
        assert!(validate_display_name(&"😀".repeat(MAX_DISPLAY_NAME_LEN)).is_ok());
        assert!(validate_display_name(&"x".repeat(MAX_DISPLAY_NAME_LEN)).is_ok());
        assert!(validate_display_name(&"x".repeat(MAX_DISPLAY_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn display_name_validation_rejects_characters_that_misrender_their_neighbours() {
        // `char::is_control` is Cc only, so every one of these but the first two
        // passed it. Each changes how the characters *around* it render.
        assert!(validate_display_name("a\nb").is_err(), "newline");
        assert!(validate_display_name("a\tb").is_err(), "tab");
        assert!(
            validate_display_name("a\u{2028}b").is_err(),
            "line separator"
        );
        assert!(
            validate_display_name("a\u{2029}b").is_err(),
            "paragraph separator"
        );
        assert!(
            validate_display_name("\u{202E}tset olleH").is_err(),
            "bidi override"
        );
        assert!(
            validate_display_name("\u{2066}x\u{2069}").is_err(),
            "bidi isolate"
        );
        assert!(validate_display_name("\u{FEFF}name").is_err(), "BOM");
    }

    #[test]
    fn display_name_validation_requires_one_visible_character() {
        // The rule is "the name renders as *something*", not a blocklist of every
        // invisible character. `worktreeLabel` falls back to the alias on exactly
        // `""`, so a non-empty name of nothing but zero-width characters is a rail
        // row, a window title and a tray item that come out blank with nothing
        // identifying the checkout.
        assert!(validate_display_name("").is_ok(), "the clear sentinel");
        assert!(
            validate_display_name("\u{200B}").is_err(),
            "zero-width space"
        );
        assert!(validate_display_name("\u{200D}").is_err(), "lone joiner");
        assert!(
            validate_display_name("\u{00AD}\u{200B}").is_err(),
            "two of them"
        );

        // **A zero-width joiner beside a visible character is legal, and must stay
        // so.** It is the glue in every multi-person, profession and flag emoji, so
        // rejecting it 400s a name the user can see perfectly well — on the one
        // free-text field this whole feature exists to provide.
        assert!(
            validate_display_name("\u{1F469}\u{200D}\u{1F4BB} dashboard").is_ok(),
            "profession emoji"
        );
        assert!(
            validate_display_name("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}").is_ok(),
            "family emoji"
        );
        assert!(
            validate_display_name("\u{1F3F3}\u{FE0F}\u{200D}\u{1F308} pride").is_ok(),
            "flag emoji"
        );
        // U+200C is orthographically required in Persian and Hindi.
        assert!(
            validate_display_name("\u{645}\u{6CC}\u{200C}\u{62E}").is_ok(),
            "zero-width non-joiner"
        );

        // Visible whitespace and punctuation stay legal — the rule is "renders as
        // something", not "is alphanumeric".
        assert!(
            validate_display_name("a\u{00A0}b").is_ok(),
            "no-break space"
        );
        assert!(validate_display_name("\u{2192} \u{2713} (!)").is_ok());
    }

    #[test]
    fn db_write_errors_map_to_client_errors_not_500s() {
        use veld_core::db::DbError;
        // Both variants are rejected *values*, not database failures. Mapping
        // either through `db_err` would report a 500 for what the user can fix,
        // and would make the handler-side pre-checks look redundant.
        let msg = |e: DbError| {
            let (code, Json(body)) = write_err(e);
            (code, body["error"].as_str().unwrap_or_default().to_owned())
        };

        let (code, body) = msg(DbError::AliasTaken("chk".into()));
        assert_eq!(code, StatusCode::CONFLICT);
        assert!(body.contains("chk"), "must name the taken alias: {body}");

        let (code, body) = msg(DbError::InvalidEmoji("🍕".into()));
        assert_eq!(code, StatusCode::BAD_REQUEST);
        // The rejected emoji is deliberately NOT echoed (unbounded input).
        assert!(!body.contains('🍕'), "{body}");

        // A genuine database failure stays a 500.
        assert_eq!(msg(DbError::NoDataDir).0, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn emoji_validation_is_an_allowlist() {
        assert!(validate_emoji(veld_core::db::WORKTREE_EMOJI[0]).is_ok());
        assert!(validate_emoji("").is_err());
        // Not in the curated set, though a perfectly valid emoji.
        assert!(validate_emoji("🍕").is_err());
        // Multi-codepoint sequences and zero-width payloads stay out.
        assert!(validate_emoji("🦊🦊").is_err());
        assert!(validate_emoji("👨‍👩‍👧").is_err());
        assert!(validate_emoji("🦊\u{200b}").is_err());
    }

    // Handler-level guards. These paths reject before any database access, so
    // they run against the real router with no test DB.
    mod handler_guards {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        fn req(method: &str, uri: &str, csrf: bool, body: &str) -> Request<Body> {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json");
            if csrf {
                b = b.header("x-veld-request", "1");
            }
            b.body(Body::from(body.to_owned())).unwrap()
        }

        /// **The gate that carries the authorization story for the most
        /// destructive route in this router, and it had no test.**
        ///
        /// This router is same-origin with any page a veld run serves, so the CSRF
        /// header is not a barrier there — the two things standing between such a
        /// page and a replaced database are this precondition and a native dialog.
        /// The dialog cannot be driven from a test; this half can, and it is also
        /// the half that keeps the endpoint inert on every healthy machine.
        ///
        /// A clean process has no recorded corruption, so the refusal must come
        /// *before* anything is moved and before any dialog is raised — if this
        /// ever starts returning 200, a prompt is appearing on people's screens.
        #[tokio::test]
        async fn restoring_without_recorded_damage_is_refused() {
            assert!(
                !crate::dbhealth::corruption_recorded(),
                "a fresh test process must not start out believing the database is damaged"
            );
            let res = super::super::routes()
                .oneshot(req("POST", "/api/db-health/restore", true, ""))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::CONFLICT);
        }

        /// The claim endpoint takes the closed set and nothing else — the id
        /// becomes a key in a file the daemon rewrites whole and re-parses on
        /// every health poll, reachable from any same-origin page.
        #[tokio::test]
        async fn a_notification_id_outside_the_closed_set_is_refused() {
            for body in [
                r#"{"id":"bogus"}"#,
                r#"{"id":"../../etc/passwd"}"#,
                r#"{"id":""}"#,
                r#"{"id":"CORRUPT"}"#,
            ] {
                let res = super::super::routes()
                    .oneshot(req("POST", "/api/db-health/notified", true, body))
                    .await
                    .unwrap();
                assert_eq!(
                    res.status(),
                    StatusCode::BAD_REQUEST,
                    "{body} must not reach the marker file"
                );
            }
        }

        #[tokio::test]
        async fn mutations_without_csrf_header_are_403() {
            // The csrf_layer covers every non-GET route by construction; this
            // list exercises each mutating route anyway so a routing change
            // (e.g. moving one off the layered router) can't ship silently.
            // Keep it in sync with routes().
            for (method, uri, body) in [
                ("POST", "/api/repos/refresh", ""),
                ("POST", "/api/db-health/notified", r#"{"id":"corrupt"}"#),
                ("POST", "/api/db-health/restore", ""),
                ("POST", "/api/repos/import", r#"{"path":"/tmp"}"#),
                ("POST", "/api/repos/revert-root", r#"{"root":"/tmp"}"#),
                ("DELETE", "/api/repos", r#"{"root":"/tmp"}"#),
                (
                    "POST",
                    "/api/worktrees",
                    r#"{"repo_root":"/tmp","branch":"b"}"#,
                ),
                ("PATCH", "/api/worktrees/1", r#"{"alias":"a"}"#),
                ("DELETE", "/api/worktrees/1", ""),
                ("POST", "/api/worktrees/1/start", "{}"),
                ("POST", "/api/worktrees/1/restore", ""),
                ("POST", "/api/worktrees/1/delete", ""),
                ("DELETE", "/api/trash?repo_root=/tmp", ""),
                ("DELETE", "/api/worktrees/1/trash-error", ""),
                (
                    "POST",
                    "/api/worktree-order",
                    r#"{"repo_root":"/tmp","order":[]}"#,
                ),
                ("POST", "/api/repo-order", r#"{"order":[]}"#),
                ("POST", "/api/lanes", r#"{"repo_root":"/tmp","name":"x"}"#),
                (
                    "POST",
                    "/api/lane-order",
                    r#"{"repo_root":"/tmp","order":[]}"#,
                ),
                (
                    "PATCH",
                    "/api/lanes/x",
                    r#"{"repo_root":"/tmp","name":"y"}"#,
                ),
                ("DELETE", "/api/lanes/x?repo_root=/tmp", ""),
                ("POST", "/api/pick-directory", ""),
                ("POST", "/api/open-worktree-storage-dir", ""),
                // Both of these execute a project-declared command, so a
                // missing header must never reach them. Note that this proves
                // nothing about the routes *existing* — `csrf_layer` wraps the
                // whole router and answers before routing, so a misspelled path
                // passes here too. `extension_routes_are_reachable` is the check
                // for that half.
                ("POST", "/api/worktrees/1/extensions/status", ""),
                (
                    "POST",
                    "/api/worktrees/1/extensions/activate",
                    r#"{"id":"pr"}"#,
                ),
            ] {
                let res = super::super::routes()
                    .oneshot(req(method, uri, false, body))
                    .await
                    .unwrap();
                assert_eq!(
                    res.status(),
                    StatusCode::FORBIDDEN,
                    "{method} {uri} must require the CSRF header"
                );
            }
        }

        /// The create response must stay **wire-compatible** with the plain
        /// `WorktreeView` every caller read before this change: the worktree's
        /// own fields flattened to the top level, and no `carry_over` key at
        /// all when there was no carry-over. A field named `carry_over` added
        /// to `WorktreeView` later would emit a duplicate key that serde does
        /// not warn about, so the shape is asserted rather than assumed.
        #[test]
        fn the_create_response_flattens_the_worktree_and_omits_an_absent_carry_over() {
            use serde_json::Value;
            // `wt_view` is this module's existing fixture, so the shape under
            // test is the same one every other view test uses.
            let view = super::super::CreatedWorktreeView {
                worktree: super::wt_view(7, false, vec![]),
                carry_over: None,
            };
            let json = serde_json::to_value(&view).expect("the response must serialize");
            let obj = json.as_object().expect("an object");
            assert_eq!(
                obj.get("alias"),
                Some(&Value::from("wt7")),
                "the worktree's fields must stay top-level, not nested under a key"
            );
            assert!(
                !obj.contains_key("carry_over"),
                "an absent carry-over must not appear as null — a client reading \
                 this object must see exactly what it always saw"
            );
            assert!(
                obj.contains_key("deleting") && obj.contains_key("presets"),
                "the WorktreeView half must flatten too, not only the record"
            );
        }

        /// **The default the maintainer asked to keep.** A request with no
        /// `source` at all and `create_branch: true` — every client written
        /// before the enum existed — must still resolve to `NewBranch`, and
        /// `create_branch: false` to `LocalBranch`. Asserted on the resolution
        /// itself, because the HTTP path cannot reach it without a registered
        /// repo and the thing worth pinning is the fallback, not the plumbing.
        #[test]
        fn a_request_with_no_source_still_means_what_create_branch_said() {
            use super::super::{CreateFrom, CreateWorktreeBody};
            // Deserialized from the literal wire text rather than built
            // field-by-field, so this covers serde's `default`s too — and it is
            // the *real* `create_from`, not a copy of it.
            let body = |json: &str| {
                serde_json::from_str::<CreateWorktreeBody>(json)
                    .expect("the request body must deserialize")
            };
            assert!(
                matches!(
                    body(r#"{"repo_root":"/r","branch":"b","create_branch":true}"#).create_from(),
                    CreateFrom::NewBranch
                ),
                "create_branch: true with no source must still cut a new branch"
            );
            assert!(
                matches!(
                    body(r#"{"repo_root":"/r","branch":"b","create_branch":false}"#).create_from(),
                    CreateFrom::LocalBranch
                ),
                "create_branch: false with no source must still check one out"
            );
            // Omitted entirely — `create_branch` is `#[serde(default)]`, so
            // this is the `false` arm and must not silently become the default
            // create.
            assert!(
                matches!(
                    body(r#"{"repo_root":"/r","branch":"b"}"#).create_from(),
                    CreateFrom::LocalBranch
                ),
                "an absent create_branch must not read as true"
            );
            // Precedence, which the decoy could not pin either: `source` wins.
            assert!(
                matches!(
                    body(
                        r#"{"repo_root":"/r","branch":"b","create_branch":true,
                            "source":{"kind":"local_branch"}}"#
                    )
                    .create_from(),
                    CreateFrom::LocalBranch
                ),
                "an explicit source must win over create_branch"
            );
        }

        /// The branch list is a GET, so it must answer without the CSRF header
        /// — and it must answer about *registered* repos only. A 404 here (not
        /// a 403, and not a 200) pins both halves: the route is reachable
        /// unauthenticated-by-header, and `/tmp` is not a repo veld manages.
        #[tokio::test]
        async fn listing_branches_is_a_get_scoped_to_registered_repos() {
            let res = super::super::routes()
                .oneshot(req("GET", "/api/repos/branches?repo_root=/tmp", false, ""))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND);
        }

        /// An unrecognised `source.kind` must be a deserialization failure, not
        /// a silent fall-through to the default create. The tagged enum is what
        /// buys this; a set of flat optional fields would have accepted the
        /// typo and quietly cut a branch from origin instead.
        #[tokio::test]
        async fn an_unknown_create_source_is_rejected_rather_than_defaulted() {
            let res = super::super::routes()
                .oneshot(req(
                    "POST",
                    "/api/worktrees",
                    true,
                    r#"{"repo_root":"/tmp","branch":"b","source":{"kind":"worktee"}}"#,
                ))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }

        #[tokio::test]
        async fn misspelled_patch_field_is_rejected_not_silently_ignored() {
            // Every field is optional, so without `deny_unknown_fields` a
            // client typo would 200 having changed nothing. axum's Json
            // extractor rejects at deserialization, hence 422 rather than
            // the 400 the hand-written guards return.
            let res = super::super::routes()
                .oneshot(req("PATCH", "/api/worktrees/1", true, r#"{"emojii":"🦊"}"#))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }

        #[tokio::test]
        async fn worktree_emoji_is_a_public_get_returning_the_curated_set() {
            // Pins the route, the CSRF exemption, and the `emoji` key the UI
            // destructures (`api.ts` declares `{ emoji: string[] }`) —
            // renaming either side would otherwise fail silently at runtime.
            let res = super::super::routes()
                .oneshot(req("GET", "/api/worktree-emoji", false, ""))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let list = json["emoji"].as_array().expect("`emoji` array");
            assert_eq!(list.len(), veld_core::db::WORKTREE_EMOJI.len());
            assert!(veld_core::db::is_worktree_emoji(list[0].as_str().unwrap()));
        }

        #[tokio::test]
        async fn invalid_inputs_are_400_before_side_effects() {
            for (method, uri, body) in [
                // relative import path
                ("POST", "/api/repos/import", r#"{"path":"not/absolute"}"#),
                // option-injection branch name
                (
                    "POST",
                    "/api/worktrees",
                    r#"{"repo_root":"/tmp","branch":"-oops"}"#,
                ),
                // dot alias
                ("PATCH", "/api/worktrees/1", r#"{"alias":".."}"#),
                // emoji outside the curated set
                ("PATCH", "/api/worktrees/1", r#"{"emoji":"🍕"}"#),
                // a valid alias must not smuggle an invalid emoji past
                // validation — both are checked before any write
                (
                    "PATCH",
                    "/api/worktrees/1",
                    r#"{"alias":"ok","emoji":"nope"}"#,
                ),
                // option-injection remote ref — `remote_ref` reaches git as a
                // start point AND as the argument `git fetch` would treat as a
                // URL, so it must be rejected on the same terms as `branch`.
                (
                    "POST",
                    "/api/worktrees",
                    r#"{"repo_root":"/tmp","branch":"ok","source":{"kind":"remote_branch","remote_ref":"-oops"}}"#,
                ),
                // empty patch
                ("PATCH", "/api/worktrees/1", "{}"),
            ] {
                let res = super::super::routes()
                    .oneshot(req(method, uri, true, body))
                    .await
                    .unwrap();
                assert_eq!(
                    res.status(),
                    StatusCode::BAD_REQUEST,
                    "{method} {uri} must reject invalid input"
                );
            }
        }

        /// The extension endpoints are actually mounted.
        ///
        /// Needed as its own test because the CSRF enumeration above cannot see
        /// it: that layer wraps the whole router and rejects before routing, so a
        /// typo in a path is invisible there. Both an unrouted path and this
        /// handler's own "worktree not found" answer 404, so the assertion is on
        /// the **body** — axum's own 404 is empty, while anything that reached a
        /// handler carries the JSON error shape.
        #[tokio::test]
        async fn extension_routes_are_reachable() {
            for (uri, body) in [
                ("/api/worktrees/999999/extensions/status", ""),
                (
                    "/api/worktrees/999999/extensions/activate",
                    r#"{"id":"nope"}"#,
                ),
            ] {
                let res = super::super::routes()
                    .oneshot(req("POST", uri, true, body))
                    .await
                    .unwrap();
                let status = res.status();
                let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
                    .await
                    .expect("body");
                assert!(
                    !bytes.is_empty(),
                    "POST {uri} produced an empty {status} body, which is axum's \
                     unrouted answer — the route is not mounted"
                );
            }
        }
    }

    /// Every shape `%(upstream:track,nobracket)` emits, and the two that must not
    /// collapse into each other.
    ///
    /// **"never pushed" and "pushed, then deleted upstream" both come back with no
    /// counts**, and they are opposite facts about whether the work got anywhere:
    /// one is a branch that has never left the machine, the other is a branch whose
    /// remote copy was removed (usually because its pull request landed). If the
    /// parser folded either into "in sync", the rail would show nothing for the
    /// case the feature exists to show.
    #[test]
    fn upstream_track_distinguishes_no_upstream_from_a_deleted_one() {
        let none = parse_upstream_track("", "");
        assert_eq!(none.upstream, None);
        assert_eq!(none.ahead, None, "nothing to be zero commits away from");
        assert_eq!(none.behind, None);
        assert!(!none.upstream_gone);
        assert!(none.is_empty(), "no upstream and no dirt is no glyph");

        let gone = parse_upstream_track("origin/feat-x", "gone");
        assert_eq!(gone.upstream.as_deref(), Some("origin/feat-x"));
        assert!(gone.upstream_gone);
        assert_eq!(
            gone.ahead, None,
            "git reports no counts for a gone upstream"
        );
        assert_eq!(gone.behind, None);
        assert!(!gone.is_empty(), "a gone upstream is worth sending");
    }

    /// An upstream that exists is comparable, so its counts are `Some(0)` — a real
    /// answer — where a branch with no upstream has `None`. Collapsing those is how
    /// "in sync" and "unknown" become the same pixel.
    #[test]
    fn upstream_track_reads_the_counts_and_zero_is_an_answer() {
        let synced = parse_upstream_track("origin/main", "");
        assert_eq!(synced.ahead, Some(0));
        assert_eq!(synced.behind, Some(0));
        assert!(!synced.upstream_gone);

        assert_eq!(
            parse_upstream_track("origin/main", "ahead 3").ahead,
            Some(3)
        );
        assert_eq!(
            parse_upstream_track("origin/main", "behind 2").behind,
            Some(2)
        );

        let both = parse_upstream_track("origin/main", "ahead 3, behind 2");
        assert_eq!(both.ahead, Some(3));
        assert_eq!(both.behind, Some(2));
    }

    /// `repo_upstreams` against real git, through the exact sequence this repo's own
    /// workflow produces: branch, push, squash-merge into main, delete the remote
    /// branch, `fetch --prune`.
    ///
    /// This is the test that pins the *premise* of the whole "already merged" glyph.
    /// `git merge-base --is-ancestor` returns false after a squash merge and the
    /// branch's own commits still exist, so `ahead` stays non-zero — `[gone]` is the
    /// only thing git says that changes, and it only says it because the fetch
    /// pruned. Drop `--prune` from `maybe_fetch` and this test is what fails.
    #[tokio::test]
    async fn repo_upstreams_reports_gone_after_a_squash_merge_and_branch_delete() {
        use std::process::Command;

        /// Runs git and returns stdout — the fixture needs `worktree list`'s output,
        /// not only its exit status.
        fn git(cwd: &std::path::Path, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let origin = dir.path().join("origin");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "t@t"]);
        git(&work, &["config", "user.name", "t"]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-m", "A"]);

        let out = Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&origin)
            .output()
            .unwrap();
        assert!(out.status.success(), "bare init failed");
        git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&work, &["push", "-u", "origin", "main"]);

        // A feature branch in its own worktree — created the way `create_worktree`
        // creates one: `-b <branch> <path> origin/<default>`. **The start point
        // being a remote-tracking ref is what gives the branch an upstream**, and
        // therefore what makes `ahead` a number at all. A branch cut from the local
        // `main` instead gets no upstream, `ahead` is `None`, and the "not pushed"
        // glyph correctly says nothing — which is why this fixture uses the
        // production start point rather than the shorter local one.
        let feature = dir.path().join("feature");
        git(
            &work,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                feature.to_str().unwrap(),
                "origin/main",
            ],
        );
        std::fs::write(feature.join("b.txt"), "b").unwrap();
        git(&feature, &["add", "b.txt"]);
        git(&feature, &["commit", "-m", "B"]);

        // **The key is the path git reports, not the path we passed to `worktree
        // add`.** Both `%(worktreepath)` and `git worktree list --porcelain`
        // resolve symlinks, so on macOS a `/var/folders/...` tempdir comes back as
        // `/private/var/folders/...`. That is exactly why the map lines up with the
        // database in production — `wt.path` is stored from `parse_worktree_list`,
        // i.e. from the porcelain — and this reads it the same way rather than
        // assuming the two spellings agree.
        let listed = git(&work, &["worktree", "list", "--porcelain"]);
        let key = listed
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .find(|path| path.ends_with("/feature"))
            .expect("git must list the feature worktree")
            .to_owned();

        let by_path = repo_upstreams(FsPath::new(&work)).await;
        let before = by_path.get(&key).expect("the feature worktree's branch");
        assert_eq!(
            before.ahead,
            Some(1),
            "one commit that its upstream (origin/main, from the start point) lacks"
        );
        assert_eq!(
            before.upstream.as_deref(),
            Some("origin/main"),
            "a branch cut from a remote-tracking ref tracks it"
        );
        assert!(!before.upstream_gone);

        // Push it, then squash-merge and delete the remote branch — `/ship`'s own
        // `gh pr merge --squash --delete-branch`, without the forge.
        git(&feature, &["push", "-u", "origin", "feature"]);
        git(&work, &["merge", "--squash", "feature"]);
        git(&work, &["commit", "-m", "B (#1)"]);
        git(&work, &["push", "origin", "main"]);
        git(&work, &["push", "origin", "--delete", "feature"]);
        git(&work, &["fetch", "--prune", "origin"]);

        let after = repo_upstreams(FsPath::new(&work)).await;
        let gone = after.get(&key).expect("the feature worktree's branch");
        assert!(
            gone.upstream_gone,
            "a squash-merged, remote-deleted branch must report a gone upstream"
        );
        assert_eq!(
            gone.upstream.as_deref(),
            Some("origin/feature"),
            "the upstream is still configured — it is the remote ref that is gone"
        );

        // And every checked-out branch is keyed by its own worktree path, so the
        // caller never has to match branch names itself.
        let main_path = listed
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .find(|path| path.ends_with("/work"))
            .expect("git must list the main worktree");
        assert!(
            after.contains_key(main_path),
            "every checked-out branch must be keyed by its worktree path"
        );
    }

    /// `git_is_dirty` against real git, over the three kinds of change that all
    /// mean "work exists only here" — and the flag that stops the daemon's own
    /// reading of it from writing to the repo.
    #[tokio::test]
    async fn git_is_dirty_sees_every_kind_of_change_without_touching_the_index() {
        use std::process::Command;

        fn git(cwd: &std::path::Path, args: &[&str]) {
            let out = Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        }

        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().to_path_buf();
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "t@t"]);
        git(&work, &["config", "user.name", "t"]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-m", "A"]);

        assert_eq!(git_is_dirty(FsPath::new(&work)).await, Some(false));

        // **The `--no-optional-locks` half.** This runs unprompted in every
        // registered checkout, so a `git status` that refreshed and rewrote
        // `.git/index` would contend with the developer's own git and turn the
        // daemon's read into a filesystem event their dev server rebuilds on.
        let index = work.join(".git/index");
        let before = std::fs::metadata(&index).unwrap();
        let (before_mtime, before_len) = (before.modified().unwrap(), before.len());
        // A plain edit is the case that needs the index refreshed to be seen, so it
        // is the one where a status *would* want to write. Touch the file's mtime
        // out of step with its content the way an editor does.
        std::fs::write(work.join("a.txt"), "changed").unwrap();
        assert_eq!(
            git_is_dirty(FsPath::new(&work)).await,
            Some(true),
            "an unstaged edit is work that exists only in this checkout"
        );
        let after = std::fs::metadata(&index).unwrap();
        assert_eq!(
            (after.modified().unwrap(), after.len()),
            (before_mtime, before_len),
            "reading dirtiness must not rewrite .git/index"
        );

        // An untracked file, alone: it is what `git worktree remove` refuses on, so
        // the glyph has to agree with the dialog that blocks the delete.
        git(&work, &["checkout", "--", "a.txt"]);
        assert_eq!(git_is_dirty(FsPath::new(&work)).await, Some(false));
        std::fs::write(work.join("scratch.md"), "notes").unwrap();
        assert_eq!(
            git_is_dirty(FsPath::new(&work)).await,
            Some(true),
            "an untracked file is work too — the delete dialog treats it as such"
        );

        // Not a repo at all: `None`, which the row renders as no glyph. Distinct
        // from `Some(false)`, which would claim the checkout is clean.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(git_is_dirty(FsPath::new(empty.path())).await, None);
    }

    /// A templated quicklink resolves per worktree, and the resolution is
    /// URL-encoded rather than pasted in raw.
    #[test]
    fn a_templated_quicklink_resolves_against_the_worktree() {
        // The same minimal config `pty`'s own builtins tests use, deserialized
        // rather than built field-by-field: `VeldConfig` has no `Default`, and a
        // hand-built one would drift from what a real `veld.json` produces.
        let cfg: veld_core::config::VeldConfig = serde_json::from_value(serde_json::json!({
            "schemaVersion": "3",
            "name": "veld",
            "nodes": {},
        }))
        .expect("minimal config");
        let resolve = |url: &str, branch: &str| {
            resolved_quicklinks(
                vec![veld_core::ide::Quicklink {
                    label: "GitHub".to_owned(),
                    url: url.to_owned(),
                }],
                "/repo/wt",
                branch,
                &cfg,
            )
        };

        let out = resolve(
            "https://github.com/o/r/tree/${veld.branch_raw}",
            "git-status-visual",
        );
        assert_eq!(out[0].url, "https://github.com/o/r/tree/git-status-visual");

        // **A branch's slashes are path, not a segment to escape.** `feat%2Ffoo` is
        // a 404 on every code host, which is the whole reason `encode_in_url` exists
        // beside `encode_component` rather than reusing it.
        let out = resolve("https://github.com/o/r/tree/${veld.branch_raw}", "feat/foo");
        assert_eq!(out[0].url, "https://github.com/o/r/tree/feat/foo");

        // ...and the character that made encoding necessary at all: git permits `#`
        // in a refname, and a raw one truncates the URL at a fragment, so the link
        // would silently open the repo's front page instead of the branch.
        let out = resolve("https://github.com/o/r/tree/${veld.branch_raw}", "feat#2");
        assert_eq!(out[0].url, "https://github.com/o/r/tree/feat%232");

        // `${veld.branch}` is the slugified name, as everywhere else — which is why
        // a URL wants `branch_raw`: this addresses a branch that does not exist.
        let out = resolve("https://x/t/${veld.branch}", "feat/foo");
        assert_eq!(out[0].url, "https://x/t/feat-foo");
    }

    /// An unresolvable reference **drops the link** rather than shipping a
    /// half-substituted URL, and an untemplated list is returned untouched.
    #[test]
    fn a_quicklink_that_cannot_resolve_is_omitted_not_mangled() {
        // The same minimal config `pty`'s own builtins tests use, deserialized
        // rather than built field-by-field: `VeldConfig` has no `Default`, and a
        // hand-built one would drift from what a real `veld.json` produces.
        let cfg: veld_core::config::VeldConfig = serde_json::from_value(serde_json::json!({
            "schemaVersion": "3",
            "name": "veld",
            "nodes": {},
        }))
        .expect("minimal config");
        let links = vec![
            veld_core::ide::Quicklink {
                label: "Plain".to_owned(),
                url: "https://staging.example.com".to_owned(),
            },
            veld_core::ide::Quicklink {
                label: "Branch".to_owned(),
                url: "https://x/t/${veld.branch_raw}".to_owned(),
            },
        ];

        // A branch starting with `-`: `worktree_builtins` omits `branch_raw` for it
        // on purpose (an argument-injection guard for `argv`), and this inherits the
        // omission because the meanings have one owner. A missing bookmark beats one
        // pointing somewhere real and wrong.
        let out = resolved_quicklinks(links.clone(), "/repo/wt", "-foo", &cfg);
        assert_eq!(
            out.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(),
            vec!["Plain"],
            "the templated link is dropped; its untemplated neighbour is not"
        );

        // No `${` anywhere: the same values back, without a context being built per
        // worktree per poll for every project that templates nothing.
        let untemplated = vec![links[0].clone()];
        assert_eq!(
            resolved_quicklinks(untemplated.clone(), "/repo/wt", "-foo", &cfg),
            untemplated
        );
    }

    /// The staleness computation, against a real git repo: the direction of
    /// `rev-list --count <local>..origin/<local>` is the easy thing to get
    /// backwards (flipping it reports how far *ahead* the main checkout is,
    /// which makes an updated main read as "behind"). `origin` here is a bare
    /// clone of `work` with one extra commit, so `main` is exactly 1 behind.
    #[tokio::test]
    async fn repo_git_status_counts_commits_behind_origin() {
        use std::process::Command;

        fn git(cwd: &std::path::Path, args: &[&str]) {
            let out = Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let origin = dir.path().join("origin");
        std::fs::create_dir_all(&work).unwrap();

        // `work`: main with one commit.
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "t@t"]);
        git(&work, &["config", "user.name", "t"]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-m", "A"]);

        // `origin`: a bare clone, then a second commit pushed through a scratch
        // clone (a bare repo has no working tree to commit in).
        let out = Command::new("git")
            .arg("clone")
            .arg("--bare")
            .arg(&work)
            .arg(&origin)
            .output()
            .unwrap();
        assert!(out.status.success(), "bare clone failed");
        let scratch = dir.path().join("scratch");
        let out = Command::new("git")
            .arg("clone")
            .arg(&origin)
            .arg(&scratch)
            .output()
            .unwrap();
        assert!(out.status.success(), "scratch clone failed");
        git(&scratch, &["config", "user.email", "t@t"]);
        git(&scratch, &["config", "user.name", "t"]);
        std::fs::write(scratch.join("b.txt"), "b").unwrap();
        git(&scratch, &["add", "b.txt"]);
        git(&scratch, &["commit", "-m", "B"]);
        git(&scratch, &["push", "origin", "main"]);

        // `work` learns about the new commit.
        git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&work, &["fetch", "origin"]);

        // Reconcile worktree rows so `repo_git_status` can find the main branch.
        // The worktrees table is keyed by an imported repo, so import first (the
        // same order `import_repo` uses).
        let db_dir = tempfile::tempdir().unwrap();
        let db = veld_core::db::Db::open_at(&db_dir.path().join("veld.db")).unwrap();
        db.upsert_repo(&work, "test").unwrap();
        sync_repo_worktrees(&db, &work).await.unwrap();

        let status = repo_git_status(&db, &work).await;
        assert_eq!(status.default_branch.as_deref(), Some("main"));
        assert_eq!(
            status.behind,
            Some(1),
            "main is one commit behind origin/main"
        );
        // The newest missing commit is the one we just pushed, so it must be
        // present and recent — the age is what the UI colours the pill with.
        let latest = status.latest_commit.expect("newest missing commit");
        let age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - latest;
        assert!(
            age < 600,
            "commit B was just made, so the newest missing commit should be recent (age {age}s)"
        );
    }

    /// `checked_default_branch` must refuse a repo root that isn't on the
    /// registered default branch, and must do so *before* the caller does
    /// anything destructive — `revert_repo_root` relies on this running ahead
    /// of `revert_git_changes` so a wrong-branch repo root never has its
    /// uncommitted work discarded for a fast-forward that was going to refuse
    /// anyway.
    #[tokio::test]
    async fn checked_default_branch_refuses_a_repo_root_on_the_wrong_branch() {
        fn git(cwd: &std::path::Path, args: &[&str]) {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "t@t"]);
        git(&work, &["config", "user.name", "t"]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-m", "A"]);

        let db_dir = tempfile::tempdir().unwrap();
        let db = veld_core::db::Db::open_at(&db_dir.path().join("veld.db")).unwrap();
        db.upsert_repo(&work, "test").unwrap();
        sync_repo_worktrees(&db, &work).await.unwrap();

        // On "main" (the registered default branch): the gate passes.
        assert_eq!(checked_default_branch(&db, &work).await.unwrap(), "main");

        // Switched to a feature branch: the gate must refuse rather than let a
        // caller proceed to something destructive.
        git(&work, &["checkout", "-qb", "feature"]);
        let refusal = checked_default_branch(&db, &work).await.unwrap_err();
        assert_eq!(refusal.0, StatusCode::CONFLICT);
    }
}
