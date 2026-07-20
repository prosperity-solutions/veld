//! Veld Desktop APIs: the repo/worktree registry behind the `/v2` management
//! UI and its Electron shell.
//!
//! A "repo" is a git repository the user imported (keyed by its main checkout
//! root); worktrees are its `git worktree` checkouts. Run state is not
//! duplicated here — the UI joins a worktree to `/api/environments` by path
//! (every worktree with a veld.json is its own veld project root).
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
pub fn routes() -> Router {
    Router::new()
        .route("/api/repos", get(list_repos).delete(remove_repo))
        .route("/api/repos/import", post(import_repo))
        .route("/api/worktrees", post(create_worktree))
        .route(
            "/api/worktrees/{id}",
            patch(rename_worktree).delete(delete_worktree),
        )
        .route("/api/worktrees/{id}/start", post(start_worktree_run))
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

/// Discover a repo's worktrees on disk and reconcile the database rows.
async fn sync_repo_worktrees(db: &Db, repo_root: &FsPath) -> Result<Vec<WorktreeRecord>, ApiError> {
    let porcelain = git(repo_root, &["worktree", "list", "--porcelain"])
        .await
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let discovered = parse_worktree_list(&porcelain);
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
    if !is_safe_identifier(alias) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "alias must be 1-64 characters: letters, digits, '-', '_', '.'",
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
    worktrees: Vec<WorktreeView>,
}

#[derive(Serialize)]
struct WorktreeView {
    #[serde(flatten)]
    worktree: WorktreeRecord,
    /// Whether the checkout has a veld.json — drives whether the UI shows run
    /// controls for it.
    has_veld_config: bool,
    /// Preset names from the checkout's veld.json (empty without a config).
    presets: Vec<String>,
}

fn worktree_view(wt: WorktreeRecord) -> WorktreeView {
    let config_path = FsPath::new(&wt.path).join("veld.json");
    let has_veld_config = config_path.is_file();
    let mut presets: Vec<String> = if has_veld_config {
        veld_core::config::load_config(&config_path)
            .ok()
            .and_then(|c| c.presets)
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    presets.sort();
    WorktreeView {
        worktree: wt,
        has_veld_config,
        presets,
    }
}

async fn repo_view(db: &Db, repo: RepoRecord) -> Result<RepoView, ApiError> {
    let worktrees = db
        .list_worktrees(FsPath::new(&repo.root))
        .map_err(db_err)?
        .into_iter()
        .map(worktree_view)
        .collect();
    Ok(RepoView { repo, worktrees })
}

async fn list_repos() -> Result<Json<RepoList>, ApiError> {
    let db = open_desktop_db()?;
    let mut repos = Vec::new();
    for repo in db.list_repos().map_err(db_err)? {
        repos.push(repo_view(&db, repo).await?);
    }
    Ok(Json(RepoList { repos }))
}

#[derive(Deserialize)]
struct ImportBody {
    /// Any directory inside the repository — the main checkout root is
    /// resolved via git.
    path: String,
}

async fn import_repo(
    headers: axum::http::HeaderMap,
    Json(body): Json<ImportBody>,
) -> Result<Json<RepoView>, ApiError> {
    check_csrf(&headers).map_err(|c| err(c, "missing X-Veld-Request header"))?;

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
    let discovered = parse_worktree_list(&porcelain);
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
    Ok(Json(repo_view(&db, repo).await?))
}

#[derive(Deserialize)]
struct RemoveRepoBody {
    root: String,
}

async fn remove_repo(
    headers: axum::http::HeaderMap,
    Json(body): Json<RemoveRepoBody>,
) -> Result<StatusCode, ApiError> {
    check_csrf(&headers).map_err(|c| err(c, "missing X-Veld-Request header"))?;
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
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateWorktreeBody>,
) -> Result<Json<WorktreeView>, ApiError> {
    check_csrf(&headers).map_err(|c| err(c, "missing X-Veld-Request header"))?;
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
        .find(|w| FsPath::new(&w.path) == checkout_path.as_path())
        .ok_or_else(|| db_err("created worktree missing after sync"))?;
    // The sync derives the alias from the branch; apply an explicit custom one.
    let created = match &body.alias {
        Some(alias) if *alias != created.alias => {
            db.rename_worktree(created.id, alias).map_err(db_err)?;
            db.get_worktree(created.id)
                .map_err(db_err)?
                .ok_or_else(|| db_err("worktree vanished after rename"))?
        }
        _ => created,
    };
    Ok(Json(worktree_view(created)))
}

#[derive(Deserialize)]
struct RenameBody {
    alias: String,
}

async fn rename_worktree(
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<RenameBody>,
) -> Result<Json<WorktreeView>, ApiError> {
    check_csrf(&headers).map_err(|c| err(c, "missing X-Veld-Request header"))?;
    validate_alias(&body.alias)?;
    let db = open_desktop_db()?;
    if !db.rename_worktree(id, &body.alias).map_err(db_err)? {
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
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    check_csrf(&headers).map_err(|c| err(c, "missing X-Veld-Request header"))?;
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
    /// Run name; defaults to the worktree alias.
    #[serde(default)]
    run_name: Option<String>,
}

/// Start a veld run in a worktree by spawning `veld start` with the worktree
/// as cwd (the CLI resolves veld.json from there) — the same fire-and-forget
/// pattern as the management stop/restart endpoints. Returns 202; the UI
/// observes progress via `/api/environments`.
async fn start_worktree_run(
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<StartBody>,
) -> Result<StatusCode, ApiError> {
    check_csrf(&headers).map_err(|c| err(c, "missing X-Veld-Request header"))?;

    let db = open_desktop_db()?;
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    let wt_path = PathBuf::from(&wt.path);
    if !wt_path.join("veld.json").is_file() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "worktree has no veld.json — nothing to start",
        ));
    }

    let run_name = body.run_name.clone().unwrap_or_else(|| wt.alias.clone());
    validate_run_name(&run_name).map_err(|c| err(c, "invalid run name"))?;
    let mut args = vec!["start".to_owned(), "--name".to_owned(), run_name];
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
}
