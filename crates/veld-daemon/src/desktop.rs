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
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::warn;
use veld_core::db::{Db, DiscoveredWorktree, RepoRecord, WorktreeRecord, default_alias};
use veld_core::user_path::resolve_user_path;

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
        .route("/api/repos/import", post(import_repo))
        .route("/api/worktrees", post(create_worktree))
        .route(
            "/api/worktrees/{id}",
            patch(patch_worktree).delete(delete_worktree),
        )
        .route("/api/worktrees/{id}/start", post(start_worktree_run))
        .route("/api/worktree-emoji", get(worktree_emoji))
        .route("/api/pick-directory", post(pick_directory))
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
        .env("PATH", resolve_user_path().await)
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

/// Open the OS folder picker and return the chosen absolute path. The daemon
/// runs in the user's GUI session (it already opens Terminal.app), so it can
/// host the dialog for the browser build too — the web platform itself never
/// exposes absolute paths. Responses: 200 `{path}`, 204 on cancel, 409 while
/// another pick is already open, 408 after the 10-minute timeout, 501 when no
/// picker backend exists, 500 when the backend fails (no GUI session, macOS
/// permission denial).
async fn pick_directory() -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Single-flight: dialogs are modal on the user's screen; N tabs (or a
    // scripted loop) must not stack N of them.
    static PICKER_OPEN: AtomicBool = AtomicBool::new(false);
    if PICKER_OPEN.swap(true, Ordering::SeqCst) {
        return Err(err(
            StatusCode::CONFLICT,
            "a directory picker is already open",
        ));
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            PICKER_OPEN.store(false, Ordering::SeqCst);
        }
    }
    let _reset = Reset;

    // 10 minutes: the request intentionally blocks while the dialog is open.
    let picked = tokio::time::timeout(std::time::Duration::from_secs(600), async {
        if cfg!(target_os = "macos") {
            // `choose folder` is a Standard Additions dialog — deliberately no
            // "System Events" activate (that is TCC-gated and a denial would
            // abort the script before the dialog ever shows).
            run_picker(
                "osascript",
                &[
                    "-e",
                    "POSIX path of (choose folder with prompt \"Choose a git repository\")",
                ],
            )
            .await
        } else {
            // Linux: try zenity, then kdialog.
            let mut last = Pick::Unavailable;
            for (cmd, args) in [
                (
                    "zenity",
                    &[
                        "--file-selection",
                        "--directory",
                        "--title=Choose a git repository",
                    ][..],
                ),
                ("kdialog", &["--getexistingdirectory", "."][..]),
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

// ---------------------------------------------------------------------------
// Error shape
// ---------------------------------------------------------------------------

/// JSON error body: worktree/git failures carry real diagnostics ("branch
/// already checked out at …") the UI must surface, unlike the bare status
/// codes of the older management endpoints.
type ApiError = (StatusCode, Json<serde_json::Value>);

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    (code, Json(serde_json::json!({ "error": msg.into() })))
}

fn db_err(e: impl std::fmt::Display) -> ApiError {
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
        other => db_err(other),
    }
}

fn open_desktop_db() -> Result<Db, ApiError> {
    open_db().map_err(|code| err(code, "failed to open the veld database"))
}

// ---------------------------------------------------------------------------
// Git plumbing
// ---------------------------------------------------------------------------

/// Run `git -C <dir> <args…>` with the user's login-shell PATH. Returns
/// trimmed stdout, or the trimmed stderr as the error message.
async fn git(dir: &FsPath, args: &[&str]) -> Result<String, String> {
    let path_env = resolve_user_path().await;
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("PATH", path_env)
        .output()
        .await
        .map_err(|e| format!("failed to run git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("git {} failed with {}", args.join(" "), output.status)
        } else {
            stderr
        })
    }
}

/// Parse `git worktree list --porcelain` output. The first entry is the main
/// checkout. Detached checkouts get the branch label `(detached)`; bare
/// entries are skipped (nothing to open or run there).
fn parse_worktree_list(porcelain: &str) -> Vec<DiscoveredWorktree> {
    let mut out = Vec::new();
    let mut first = true;
    for block in porcelain.split("\n\n") {
        let mut path: Option<&str> = None;
        let mut branch: Option<&str> = None;
        let mut bare = false;
        let mut detached = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p);
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b));
            } else if line == "bare" {
                bare = true;
            } else if line == "detached" {
                detached = true;
            }
        }
        let Some(path) = path else { continue };
        let is_main = std::mem::take(&mut first);
        if bare {
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
    let porcelain = git(repo_root, &["worktree", "list", "--porcelain"])
        .await
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let discovered = canonicalize_discovered(parse_worktree_list(&porcelain));
    db.sync_worktrees(repo_root, &discovered).map_err(db_err)
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

/// The glyphs `validate_emoji` accepts, for the UI's picker. Served rather
/// than duplicated in TypeScript so the two can never drift; static, so the
/// picker fetches it once on open instead of riding the 5s poll.
async fn worktree_emoji() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "emoji": veld_core::db::WORKTREE_EMOJI }))
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
    available: bool,
    worktrees: Vec<WorktreeView>,
}

#[derive(Serialize)]
struct WorktreeView {
    #[serde(flatten)]
    worktree: WorktreeRecord,
    /// Whether the checkout has a root config — drives whether the UI shows run
    /// controls for it.
    has_veld_config: bool,
    /// Presets from the checkout's root config, in display order, with their
    /// keys and labels (empty without a config). The UI shows the label a human
    /// can read; `name` is what it sends back to start the run.
    presets: Vec<veld_core::presets::ResolvedPreset>,
    /// Startable nodes with their variants — the UI's custom-selection
    /// source when no preset fits (hidden nodes excluded).
    nodes: Vec<NodeOptionView>,
}

#[derive(Serialize)]
struct NodeOptionView {
    name: String,
    variants: Vec<String>,
    default_variant: Option<String>,
}

fn worktree_view(wt: WorktreeRecord) -> WorktreeView {
    let config_path = veld_core::config::root_config_in(FsPath::new(&wt.path));
    let has_veld_config = config_path.is_some();
    let cfg = config_path
        .as_deref()
        .and_then(|p| veld_core::config::parse_config(p).ok());
    // Display order comes from the resolver, not a sort here — the UI list and
    // the CLI picker must agree, or the key printed next to a preset in one
    // surface means something else in the other.
    let presets = cfg
        .as_ref()
        .map(veld_core::presets::resolve)
        .unwrap_or_default();
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
    WorktreeView {
        worktree: wt,
        has_veld_config,
        presets,
        nodes,
    }
}

async fn repo_view(db: &Db, repo: RepoRecord, available: bool) -> Result<RepoView, ApiError> {
    let worktrees = db
        .list_worktrees(FsPath::new(&repo.root))
        .map_err(db_err)?
        .into_iter()
        .map(worktree_view)
        .collect();
    Ok(RepoView {
        repo,
        available,
        worktrees,
    })
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
        repos.push(repo_view(&db, repo, available).await?);
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
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    /// Debounce clock + the availability each repo had at the last real sync.
    /// Memoizing availability keeps concurrent clients consistent: a non-due
    /// poll must not substitute a semantically-weaker check (is_dir) that can
    /// disagree with the due poll's git result during a failure.
    static LAST_SYNC: Mutex<Option<(Instant, HashMap<String, bool>)>> = Mutex::new(None);

    let memo = {
        let last = LAST_SYNC.lock().expect("refresh debounce mutex poisoned");
        match &*last {
            Some((t, memo)) if t.elapsed() < Duration::from_secs(2) => Some(memo.clone()),
            _ => None,
        }
    };

    let db = open_desktop_db()?;
    let mut repos = Vec::new();
    let mut availability = HashMap::new();
    for repo in db.list_repos().map_err(db_err)? {
        let root = PathBuf::from(&repo.root);
        let available = match &memo {
            // Repo imported inside the debounce window: not in the memo yet —
            // its rows were just written by import, dir-exists is fine.
            Some(memo) => memo.get(&repo.root).copied().unwrap_or(root.is_dir()),
            None => sync_repo_worktrees(&db, &root).await.is_ok(),
        };
        availability.insert(repo.root.clone(), available);
        repos.push(repo_view(&db, repo, available).await?);
    }
    if memo.is_none() {
        *LAST_SYNC.lock().expect("refresh debounce mutex poisoned") =
            Some((Instant::now(), availability));
    }
    Ok(Json(RepoList { repos }))
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
    Ok(Json(repo_view(&db, repo, true).await?))
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

#[derive(Deserialize)]
struct CreateWorktreeBody {
    repo_root: String,
    branch: String,
    /// Create `branch` (from the repo's current HEAD) instead of checking out
    /// an existing one.
    #[serde(default)]
    create_branch: bool,
    /// Custom alias; defaults to a slug of the branch name.
    #[serde(default)]
    alias: Option<String>,
    /// Custom checkout path; defaults to `<repo parent>/_worktrees/<alias>`.
    #[serde(default)]
    path: Option<String>,
}

async fn create_worktree(
    Json(body): Json<CreateWorktreeBody>,
) -> Result<Json<WorktreeView>, ApiError> {
    validate_branch(&body.branch)?;
    if let Some(ref alias) = body.alias {
        validate_alias(alias)?;
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
        None => {
            let parent = repo_root
                .parent()
                .ok_or_else(|| err(StatusCode::BAD_REQUEST, "repo root has no parent"))?;
            parent.join("_worktrees").join(&alias_hint)
        }
    };
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
    let git_args: Vec<&str> = if body.create_branch {
        vec!["worktree", "add", "-b", &body.branch, "--", &path_str]
    } else {
        vec!["worktree", "add", "--", &path_str, &body.branch]
    };
    git(&repo_root, &git_args)
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;

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
    Ok(Json(worktree_view(created)))
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
    #[serde(default)]
    emoji: Option<String>,
}

impl PatchWorktreeBody {
    /// Derived from the fields, so adding a third can't leave the
    /// "nothing to update" guard silently behind.
    fn is_empty(&self) -> bool {
        let Self { alias, emoji } = self;
        alias.is_none() && emoji.is_none()
    }
}

async fn patch_worktree(
    Path(id): Path<i64>,
    Json(body): Json<PatchWorktreeBody>,
) -> Result<Json<WorktreeView>, ApiError> {
    if body.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "nothing to update: send an alias, an emoji, or both",
        ));
    }
    // Validate everything before touching the database: a request carrying a
    // good alias and a bad emoji must change neither.
    if let Some(alias) = &body.alias {
        validate_alias(alias)?;
    }
    if let Some(emoji) = &body.emoji {
        validate_emoji(emoji)?;
    }

    let db = open_desktop_db()?;
    // One write for both columns, and the alias-collision check shares its
    // transaction — see `Db::patch_worktree`.
    let existed = db
        .patch_worktree(id, body.alias.as_deref(), body.emoji.as_deref())
        .map_err(write_err)?;
    if !existed {
        return Err(err(StatusCode::NOT_FOUND, "worktree not found"));
    }
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    Ok(Json(worktree_view(wt)))
}

#[derive(Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    force: bool,
}

async fn delete_worktree(
    Path(id): Path<i64>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    let db = open_desktop_db()?;
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    if wt.is_main {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "refusing to remove the main checkout",
        ));
    }

    let repo_root = PathBuf::from(&wt.repo_root);
    let mut args = vec!["worktree", "remove"];
    if q.force {
        args.push("--force");
    }
    args.push("--");
    args.push(&wt.path);
    match git(&repo_root, &args).await {
        Ok(_) => {}
        // Already gone from disk (removed manually): prune git's bookkeeping
        // and drop the row instead of failing.
        Err(_) if !FsPath::new(&wt.path).exists() => {
            let _ = git(&repo_root, &["worktree", "prune"]).await;
        }
        Err(e) => return Err(err(StatusCode::UNPROCESSABLE_ENTITY, e)),
    }
    db.remove_worktree(id).map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
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

    let code = spawn_veld(&wt_path, &args);
    if code == StatusCode::ACCEPTED {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(err(code, "failed to spawn veld start"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        #[tokio::test]
        async fn mutations_without_csrf_header_are_403() {
            // The csrf_layer covers every non-GET route by construction; this
            // list exercises each mutating route anyway so a routing change
            // (e.g. moving one off the layered router) can't ship silently.
            // Keep it in sync with routes().
            for (method, uri, body) in [
                ("POST", "/api/repos/refresh", ""),
                ("POST", "/api/repos/import", r#"{"path":"/tmp"}"#),
                ("DELETE", "/api/repos", r#"{"root":"/tmp"}"#),
                (
                    "POST",
                    "/api/worktrees",
                    r#"{"repo_root":"/tmp","branch":"b"}"#,
                ),
                ("PATCH", "/api/worktrees/1", r#"{"alias":"a"}"#),
                ("DELETE", "/api/worktrees/1", ""),
                ("POST", "/api/worktrees/1/start", "{}"),
                ("POST", "/api/pick-directory", ""),
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
    }
}
