//! The file pane's server half: reading a text file for the `/ide` bundle, and
//! `veld ide open`.
//!
//! # Why the text goes through the management origin
//!
//! The browser pane's bytes come from a second origin (see `files.rs`) because what
//! it renders is HTML an agent wrote, and that HTML must not be same-origin with the
//! management API. The file pane is the opposite case: it is React on the IDE
//! origin, and what it is handed is a **string** — markdown rendered with raw HTML
//! off and sanitised before it reaches the DOM, CSV parsed into cells, code
//! tokenised. Nothing in a file it shows is ever executed, so reading it same-origin
//! is safe, and it has to be: a cross-origin `fetch` would be opaque.
//!
//! # What a file pane may read
//!
//! For worktree W, a read is allowed when the **resolved** path is:
//!
//! 1. inside W — the worktree's own files, like the browser pane's grant;
//! 2. inside a folder the user listed in `files.extraFolders`;
//! 3. exactly a file in W's granted set (`Db::grant_file_read`) — one somebody in
//!    W opened by name: a click on a path in W's terminal, or `veld ide open` from
//!    W's shell.
//!
//! And in every case not [`files::is_sensitive`] (judged on the whole absolute
//! path outside W, so `~/.ssh/config` is refused whatever list it is on), outside
//! W not [`files::is_home_credential`] either (`~/.aws`, `~/.config/gh`, an
//! `auth.json` — a CLI's login, under a name no extension rule would catch), and a
//! kind [`files::readable_text_kind`] knows. Resolution comes first and the guards
//! judge its result, for the reason `files::resolve_servable` documents: a symlink
//! named `notes.md` pointing at `.env` is a `.env`. Outside W the guards judge the
//! path as it was **asked for** too, since the reverse holds as well: a dotfile
//! manager's `~/.docker/config.json` pointing into `~/dotfiles` is still a login.
//!
//! Every refusal is one 404, as on the file origin — "not allowed" and "not there"
//! are the same answer to a caller who should not learn which.

use std::path::{Path, PathBuf};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query},
    http::{HeaderMap, StatusCode},
    routing::{MethodRouter, get, post},
};
use serde::Deserialize;
use veld_core::db::Db;
use veld_core::files::{self, TextKind};

use super::desktop::{ApiError, db_err, err};
use super::management::{check_csrf, check_host, open_db};

/// Biggest text file the pane will be handed.
///
/// Far smaller than the file origin's cap: that one streams an image to a
/// renderer, while this one becomes a JS string, then a parse, then a DOM. Five
/// megabytes is a very long log or a large CSV, and past it the pane says so
/// rather than freezing the window.
pub const MAX_TEXT_BYTES: u64 = 5 * 1024 * 1024;

/// The 413 sentence. One decimal, because whole megabytes rounded down told a 5.5 MB
/// file it was "5 MB" against a 5 MB cap.
fn too_large(len: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    format!(
        "this file is {:.1} MB; a file pane shows up to {} MB",
        len as f64 / MB,
        MAX_TEXT_BYTES / (1024 * 1024)
    )
}

/// Longest path or target string accepted. See `pty::MAX_PATH_LEN`.
const MAX_PATH_LEN: usize = 4096;
/// Longest URL `veld ide open` will pass on. See `pty::MAX_URL_LEN`.
const MAX_URL_LEN: usize = 8 * 1024;

/// Routes on the management origin. All of them check CSRF, the GET included: a
/// read costs up to [`MAX_TEXT_BYTES`] of memory on a worktree id anyone can
/// guess, which is the same reasoning as `files::list_viewable`. And all of them
/// check `Host` ([`require_local_host`]), because what they answer with is a
/// file's contents.
pub fn routes() -> Router {
    route_table()
        .into_iter()
        .fold(Router::new(), |router, (path, method)| {
            router.route(path, method)
        })
}

/// Every route [`routes`] mounts, as a table, so the Host and CSRF tests walk the
/// same list the router is built from: a route added here fails those tests until
/// they have a sample request for it.
fn route_table() -> [(&'static str, MethodRouter); 3] {
    [
        ("/api/worktrees/{id}/file-text", get(file_text)),
        ("/api/worktrees/{id}/file-grants", post(grant_file)),
        ("/api/ide/open", post(ide_open)),
    ]
}

/// A file a pane of some worktree may read, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Readable {
    /// The canonical path the bytes come from.
    full: PathBuf,
    /// What the pane calls it: worktree-relative inside, absolute outside.
    display: String,
    inside: bool,
    kind: TextKind,
}

impl Readable {
    /// The canonical path the bytes come from — for `changes`, which reads them.
    pub(super) fn full(&self) -> &Path {
        &self.full
    }
}

/// Resolve `requested` (worktree-relative, or absolute) for a pane of `root`.
///
/// `extra` is `files.extraFolders`, already canonicalised; `granted` answers rule 3
/// for a canonical path. Pure apart from the filesystem, so the rules can be
/// tested without a database. See the module docs for the rules themselves.
///
/// `changes::file_base` asks too, with no folders and no grants, so the old text of
/// a file is refused exactly when its current text would be.
pub(super) fn resolve_readable(
    root: &Path,
    requested: &str,
    extra: &[PathBuf],
    granted: impl Fn(&Path) -> bool,
) -> Option<Readable> {
    if requested.len() > MAX_PATH_LEN || files::is_sensitive(requested) {
        return None;
    }
    let root = root.canonicalize().ok()?;
    // `spelled` is the path as asked for, made absolute but not resolved — what the
    // credential guards below judge alongside `full`.
    let (full, spelled) = if Path::new(requested).is_absolute() {
        (
            Path::new(requested).canonicalize().ok()?,
            files::lexical(Path::new(requested)),
        )
    } else {
        let rel = super::files::normalize_relative(requested)?;
        let spelled = root.join(rel);
        (spelled.canonicalize().ok()?, spelled)
    };
    if !full.is_file() {
        return None;
    }
    if let Ok(rel) = full.strip_prefix(&root) {
        let rel = rel.to_str()?;
        if files::is_sensitive(rel) {
            return None;
        }
        return Some(Readable {
            kind: files::readable_text_kind(rel)?,
            display: rel.to_owned(),
            full,
            inside: true,
        });
    }
    let abs = full.to_str()?.to_owned();
    // Every segment of the absolute path, so a granted or listed file under
    // `~/.ssh` or a `.git` directory is refused like one inside the worktree.
    if files::is_sensitive(&abs) || spelled.to_str().is_none_or(files::is_sensitive) {
        return None;
    }
    if credential_by_either_spelling(&full, &spelled, &CredentialHomes::current()) {
        return None;
    }
    let kind = files::readable_text_kind(&abs)?;
    if !(extra.iter().any(|folder| full.starts_with(folder)) || granted(&full)) {
        return None;
    }
    Some(Readable {
        full,
        display: abs,
        inside: false,
        kind,
    })
}

/// The folders [`files::is_home_credential`] and [`files::is_config_credential`]
/// are judged against, each in every spelling a path might use.
struct CredentialHomes {
    /// The home folder as `$HOME` spells it, and canonical.
    homes: Vec<PathBuf>,
    /// `$XDG_CONFIG_HOME`, when it is set to an absolute path, both ways too.
    configs: Vec<PathBuf>,
}

impl CredentialHomes {
    fn current() -> Self {
        fn both(p: Option<PathBuf>) -> Vec<PathBuf> {
            let canonical = p.as_ref().and_then(|p| p.canonicalize().ok());
            p.into_iter().chain(canonical).collect()
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        Self {
            homes: both(dirs::home_dir()),
            configs: both(config),
        }
    }
}

/// Whether a read outside the worktree is a CLI's login by **either** spelling:
/// `full`, the canonical file, or `spelled`, the path as it was asked for.
///
/// Both, because a dotfile manager makes them disagree. `~/.docker/config.json`
/// symlinked to `~/dotfiles/docker/config.json` canonicalises to a path no home
/// rule names, and the canonical one alone let it through; the spelling alone would
/// miss the reverse, a harmless-looking link to `~/.aws/config`. Each is matched
/// against the home folder as written and canonical — the canonical file against a
/// home reached through a symlink (`/home` → `/var/home`), the spelling against the
/// `$HOME` it was typed under.
fn credential_by_either_spelling(full: &Path, spelled: &Path, at: &CredentialHomes) -> bool {
    [full, spelled].into_iter().any(|path| {
        files::is_home_credential(path, None)
            || at
                .homes
                .iter()
                .any(|home| files::is_home_credential(path, Some(home)))
            || at
                .configs
                .iter()
                .any(|config| files::is_config_credential(path, config))
    })
}

/// `files.extraFolders`, canonicalised. A folder that does not exist (yet) is
/// dropped for this request rather than refused — it holds nothing to read.
///
/// Judged once more after canonicalising: `~/notes` that is a symlink to `~` names
/// the whole home folder, which no spelling of the entry may.
fn extra_folders(db: &Db) -> Vec<PathBuf> {
    let home = dirs::home_dir().and_then(|h| h.canonicalize().ok());
    db.files_extra_folders()
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| !files::folder_too_broad(p, home.as_deref()))
        .collect()
}

/// [`check_host`], answered the way this module's routes answer. Shared with
/// `changes`, whose routes serve file contents too.
pub(super) fn require_local_host(headers: &HeaderMap) -> Result<(), ApiError> {
    check_host(headers).map_err(|_| {
        err(
            StatusCode::FORBIDDEN,
            "this request's Host is not a name Veld answers to",
        )
    })
}

/// The worktree, or the 404 every route here answers for one that is not there.
fn worktree(db: &Db, id: i64) -> Result<veld_core::db::WorktreeRecord, ApiError> {
    db.get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such worktree"))
}

/// Resolve a read for worktree `id`: database and filesystem, on the blocking pool.
async fn lookup(id: i64, path: String) -> Result<Readable, ApiError> {
    crate::offload::blocking(move || {
        let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
        let wt = worktree(&db, id)?;
        let extra = extra_folders(&db);
        resolve_readable(Path::new(&wt.path), &path, &extra, |full| {
            full.to_str()
                .is_some_and(|f| db.file_read_granted(&wt.path, f).unwrap_or(false))
        })
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such file"))
    })
    .await
}

/// A version stamp for a file: modification time and size.
///
/// Nanoseconds rather than the `mtimeMs` beside it, so two writes inside one
/// millisecond — an agent writing a file in two steps — are two stamps.
fn etag_of(meta: &std::fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{mtime:x}-{:x}", meta.len())
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// The metadata half of a file-text answer, shared with the grant route.
fn describe(r: &Readable, meta: &std::fs::Metadata) -> serde_json::Map<String, serde_json::Value> {
    let serde_json::Value::Object(map) = serde_json::json!({
        "path": r.display,
        "absPath": r.full.to_string_lossy(),
        "kind": r.kind,
        "etag": etag_of(meta),
        "mtimeMs": mtime_ms(meta),
        "size": meta.len(),
        "insideWorktree": r.inside,
    }) else {
        unreachable!("a json! object literal is an object")
    };
    map
}

#[derive(Debug, Deserialize)]
struct TextQuery {
    path: String,
    /// The stamp the pane already has. A match answers `unchanged` without reading.
    etag: Option<String>,
}

/// `GET /api/worktrees/{id}/file-text?path=…[&etag=…]` — the text a file pane shows.
///
/// Polled every second or two by each visible pane, so the common answer is the
/// cheap one: a `stat` that matches the pane's `etag` and reads nothing. The stat
/// comes **before** the read, so a write landing between the two leaves the pane
/// holding new text under an old stamp — and the next poll's stat differs and
/// fetches again. The other order would pin the pane on stale text.
async fn file_text(
    headers: HeaderMap,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<TextQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    require_local_host(&headers)?;
    let readable = lookup(id, q.path).await?;
    let meta = tokio::fs::metadata(&readable.full)
        .await
        .map_err(|_| err(StatusCode::NOT_FOUND, "no such file"))?;
    let etag = etag_of(&meta);
    if q.etag.as_deref() == Some(etag.as_str()) {
        return Ok(Json(serde_json::json!({ "unchanged": true, "etag": etag })));
    }
    if meta.len() > MAX_TEXT_BYTES {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, too_large(meta.len())));
    }
    // The same semaphore as the file origin's reads: the bound is on the daemon's
    // memory, and two pools would each be allowed the whole of it.
    let _slot = super::files::READ_SLOTS
        .acquire()
        .await
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "shutting down"))?;
    let bytes = tokio::fs::read(&readable.full)
        .await
        .map_err(|_| err(StatusCode::NOT_FOUND, "no such file"))?;
    if files::looks_binary(&bytes) {
        return Err(not_text(&bytes));
    }
    // Lossy rather than refused: a Latin-1 log with one `é` is still worth reading,
    // and the NUL test above has already turned away the files this would mangle.
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    let mut body = describe(&readable, &meta);
    body.insert("text".to_owned(), serde_json::Value::String(text));
    Ok(Json(serde_json::Value::Object(body)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantRequest {
    /// Absolute, or relative the way a terminal prints it — see [`locate`].
    path: String,
}

/// `POST /api/worktrees/{id}/file-grants` — resolve a path the user clicked in W's
/// terminal, and let W's file panes read it if it is outside W.
///
/// What the bundle calls on every click of a path, relative or absolute: the click
/// is the user naming that file from W. The path is found the way the
/// `accepts:"file"` actions find theirs ([`locate`]), so `tabKeys.ts` printed by
/// an `ls` in a subdirectory opens the one file of that name. A file inside W, or
/// under an extra folder, needs no grant and gets none — the answer is the same
/// metadata either way (`path` worktree-relative inside, absolute outside), so the
/// client does not have to know which rule let it in.
///
/// CSRF-gated: without it any page could add entries to the set.
async fn grant_file(
    headers: HeaderMap,
    UrlPath(id): UrlPath<i64>,
    Json(body): Json<GrantRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    require_local_host(&headers)?;
    if body.path.len() > MAX_PATH_LEN {
        return Err(err(StatusCode::NOT_FOUND, "no such file"));
    }
    let root = crate::offload::blocking(move || {
        let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
        Ok::<_, ApiError>(worktree(&db, id)?.path)
    })
    .await?;
    let path = locate(&root, &body.path)
        .await
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such file"))?;
    let readable = crate::offload::blocking(move || {
        let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
        grant_blocking(&db, &root, &path).ok_or_else(|| err(StatusCode::NOT_FOUND, "no such file"))
    })
    .await?;
    let meta = tokio::fs::metadata(&readable.full)
        .await
        .map_err(|_| err(StatusCode::NOT_FOUND, "no such file"))?;
    Ok(Json(serde_json::Value::Object(describe(&readable, &meta))))
}

/// Which file a clicked `path` names in the worktree at `root`, before any rule
/// judges it.
///
/// Absolute, or relative and present at the root: itself. Relative and **not**
/// at the root: the one file in the checkout whose path ends with it
/// (`extensions::resolve_by_suffix`, whose docs say why a terminal path is so
/// often relative to something else). `None` when nothing — or more than one
/// file — matches, which the route answers with its one 404.
async fn locate(root: &str, path: &str) -> Option<String> {
    if Path::new(path).is_absolute() {
        return Some(path.to_owned());
    }
    let literal = super::files::normalize_relative(path)
        .is_some_and(|rel| Path::new(root).join(rel).is_file());
    if literal {
        return Some(path.to_owned());
    }
    super::extensions::resolve_by_suffix(root, path).await.ok()
}

/// Resolve `path` as if it were granted, and say whether a grant is what lets it
/// in: not inside the worktree and not under an extra folder. Every guard but the
/// set itself — sensitivity, kind, a regular file — has passed by then.
fn resolve_for_grant(extra: &[PathBuf], root: &str, path: &str) -> Option<(Readable, bool)> {
    let readable = resolve_readable(Path::new(root), path, extra, |_| true)?;
    let needs_grant =
        !readable.inside && !extra.iter().any(|folder| readable.full.starts_with(folder));
    Some((readable, needs_grant))
}

/// [`resolve_for_grant`], and record the grant if one is needed.
fn grant_blocking(db: &Db, root: &str, path: &str) -> Option<Readable> {
    let (readable, needs_grant) = resolve_for_grant(&extra_folders(db), root, path)?;
    if needs_grant {
        db.grant_file_read(root, &readable.display)
            .map_err(|e| tracing::warn!("could not record a file grant: {e}"))
            .ok()?;
    }
    Some(readable)
}

// ---------------------------------------------------------------------------
// `veld ide open`
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenRequest {
    worktree_id: Option<i64>,
    session_id: Option<String>,
    /// The CLI's working directory, absolute — how a shell that is not a Veld
    /// terminal still names a worktree.
    cwd: Option<String>,
    /// An absolute path or an http(s) URL.
    target: String,
    line: Option<u32>,
    #[serde(default)]
    notify: bool,
}

/// An error with a machine-readable `code` beside the sentence, so the CLI can
/// pick its exit status without parsing English.
pub(super) fn coded(status: StatusCode, code: &str, msg: impl Into<String>) -> ApiError {
    (
        status,
        Json(serde_json::json!({ "error": msg.into(), "code": code })),
    )
}

/// What the request resolved to, before anything is pushed.
#[derive(Debug, PartialEq, Eq)]
enum Resolved {
    /// `grant`: the file is outside the worktree and no other rule lets it in, so
    /// the pane will need it in the granted set.
    File {
        display: String,
        grant: bool,
    },
    Url {
        url: String,
    },
}

/// `POST /api/ide/open` — show a file or a page beside a terminal, or in whatever
/// window is showing a worktree. What `veld ide open` calls.
///
/// The worktree is named exactly one way: a terminal session (the pane opens beside
/// its dock), the caller's working directory, or an id. Text files go to a file
/// pane; pages, PDFs and images inside the worktree, and web URLs, go to a browser
/// pane. **There is no system-opener fallback**, unlike the `open` shim: a caller
/// that asked for this by name wants to know it did not happen, which is what the
/// 4xx answers are — each a sentence, plus a `code` the CLI maps to an exit status.
///
/// A text file outside the worktree is added to its granted set here — an agent in
/// W naming a file is the same act as the user clicking it in W's terminal — but
/// only for an open that happens: see the grant's comment below.
///
/// CSRF-gated: without it any page could push a pane, and a grant, into a window.
async fn ide_open(
    headers: HeaderMap,
    Json(body): Json<OpenRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    require_local_host(&headers)?;
    let named = [
        body.worktree_id.is_some(),
        body.session_id.is_some(),
        body.cwd.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if named != 1 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "name the worktree exactly one way: worktreeId, sessionId or cwd",
        ));
    }
    if body.target.len() > MAX_URL_LEN {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "target is too long"));
    }

    let worktree_id = match (&body.session_id, body.worktree_id) {
        (Some(session), _) => super::pty::session_worktree(session).await.ok_or_else(|| {
            coded(
                StatusCode::NOT_FOUND,
                "no_session",
                "that terminal session is not one Veld has",
            )
        })?,
        (None, Some(id)) => id,
        (None, None) => {
            let cwd = body.cwd.clone().unwrap_or_default();
            crate::offload::blocking(move || {
                let db = open_db()
                    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
                worktree_containing(&db, Path::new(&cwd)).ok_or_else(|| {
                    coded(
                        StatusCode::NOT_FOUND,
                        "not_in_worktree",
                        format!("not inside a worktree Veld knows (cwd: {cwd})"),
                    )
                })
            })
            .await?
        }
    };

    let (target, notify) = (body.target.clone(), body.notify);
    let (wt, resolved, fresh_grant) = crate::offload::blocking(move || {
        let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
        let wt = db
            .get_worktree(worktree_id)
            .map_err(db_err)?
            .ok_or_else(|| coded(StatusCode::NOT_FOUND, "no_worktree", "no such worktree"))?;
        let resolved = resolve_open_target(&db, &wt.path, &target)?;
        // **Granted before the push, taken back if the push fails.** Written after
        // a successful push instead, the grant would race the pane it is for: the
        // window gets the frame and asks for the file's text, and that read can
        // land before this write. `fresh` is whether this call added the entry, so
        // a refused open never takes back a grant somebody made earlier.
        let fresh = match &resolved {
            Resolved::File {
                display,
                grant: true,
            } => {
                let fresh = !db.file_read_granted(&wt.path, display).map_err(db_err)?;
                db.grant_file_read(&wt.path, display).map_err(db_err)?;
                fresh.then(|| display.clone())
            }
            _ => None,
        };
        Ok::<_, ApiError>((wt, resolved, fresh))
    })
    .await?;

    let name = if wt.display_name.is_empty() {
        wt.alias.clone()
    } else {
        wt.display_name.clone()
    };
    let (open, mut answer) = match resolved {
        Resolved::File { display, .. } => (
            super::ide::OpenTarget::File {
                path: display.clone(),
                line: body.line,
            },
            serde_json::json!({ "opened": "file", "path": display }),
        ),
        Resolved::Url { url } => (
            super::ide::OpenTarget::Url { url: url.clone() },
            serde_json::json!({ "opened": "browser", "url": url }),
        ),
    };
    if !super::ide::push_open(worktree_id, body.session_id.clone(), open, notify).await {
        if let Some(display) = fresh_grant {
            let root = wt.path.clone();
            // Best effort: the caller is told the open failed either way, and a
            // stray grant is one file this worktree's panes could read, not a leak.
            let _ = crate::offload::blocking(move || {
                let db =
                    open_db().map_err(|e| tracing::warn!("could not revoke a grant: {e:?}"))?;
                db.revoke_file_read(&root, &display)
                    .map_err(|e| tracing::warn!("could not revoke a grant: {e}"))
            })
            .await;
        }
        return Err(coded(
            StatusCode::CONFLICT,
            "no_window",
            format!("no Veld window is showing {name} right now"),
        ));
    }
    answer["worktreeId"] = worktree_id.into();
    answer["worktree"] = name.into();
    Ok(Json(answer))
}

/// Decide where `target` opens for the worktree at `root`, and whether the file
/// pane will need a grant to read it. Writes nothing.
///
/// A text file is checked the way the pane's first read will check it — size,
/// then a NUL sniff — so `veld ide open` on a 40 MB log or a `.json` that is
/// really a binary fails here with a sentence, rather than "opening" a pane that
/// can only say no.
fn resolve_open_target(db: &Db, root: &str, target: &str) -> Result<Resolved, ApiError> {
    let lower = target.trim_start().to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let web = veld_core::ide::parse_web_url(target)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "not an http(s) URL with a host"))?;
        return Ok(Resolved::Url {
            url: web.canonical.as_str().to_owned(),
        });
    }
    if target.len() > MAX_PATH_LEN || !Path::new(target).is_absolute() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "the target must be an absolute path or an http(s) URL",
        ));
    }
    let full = Path::new(target)
        .canonicalize()
        .map_err(|_| err(StatusCode::NOT_FOUND, format!("no such file: {target}")))?;
    if !full.is_file() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("not a file: {target}"),
        ));
    }
    let full_str = full.to_string_lossy();

    // Routing is decided on the resolved name. The read is resolved from `target`
    // as the caller spelled it (the CLI sends it unresolved), so the guards see
    // both names — see `credential_by_either_spelling`.
    if files::text_kind(&full_str).is_some() {
        let (readable, grant) =
            resolve_for_grant(&extra_folders(db), root, target).ok_or_else(|| {
                err(
                    StatusCode::FORBIDDEN,
                    "Veld does not show this file — it looks like a secret",
                )
            })?;
        check_showable_text(&readable.full)?;
        return Ok(Resolved::File {
            display: readable.display,
            grant,
        });
    }
    if files::servable_type(&full_str).is_some() {
        let inside = Path::new(root)
            .canonicalize()
            .ok()
            .and_then(|r| full.strip_prefix(&r).ok().map(Path::to_path_buf));
        let Some(rel) = inside.as_deref().and_then(Path::to_str) else {
            return Err(err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "only files inside the worktree can open in a browser pane",
            ));
        };
        if files::is_sensitive(rel) {
            return Err(err(
                StatusCode::FORBIDDEN,
                "Veld does not show this file — it looks like a secret",
            ));
        }
        return super::files::url_for(db, root, rel)
            .map(|url| Resolved::Url { url })
            .ok_or_else(|| {
                err(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "veld cannot serve local files right now (the files.* route is not \
                     registered — is the helper running?)",
                )
            });
    }
    Err(err(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "Veld cannot show this kind of file",
    ))
}

/// The size and binary checks `file_text` makes, for a file not yet shown: the
/// same 413 and 415, so the answer to "can a pane show this" does not depend on
/// which route asked first. Reads at most the first 8 KB. The `open` shim asks too
/// (`pty::for_file_pane`), to keep such a file on its browser-pane route.
pub(super) fn check_showable_text(full: &Path) -> Result<(), ApiError> {
    use std::io::Read;
    let missing = || err(StatusCode::NOT_FOUND, "no such file");
    let meta = std::fs::metadata(full).map_err(|_| missing())?;
    if meta.len() > MAX_TEXT_BYTES {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, too_large(meta.len())));
    }
    let mut head = Vec::with_capacity(8 * 1024);
    std::fs::File::open(full)
        .and_then(|f| f.take(8 * 1024).read_to_end(&mut head))
        .map_err(|_| missing())?;
    if files::looks_binary(&head) {
        return Err(not_text(&head));
    }
    Ok(())
}

/// The 415 for bytes [`files::looks_binary`] refused. UTF-16 text is refused too —
/// every other byte of ASCII in it is a NUL — so a file that starts with a UTF-16
/// byte-order mark is told that, rather than that it is binary.
fn not_text(bytes: &[u8]) -> ApiError {
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    err(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        if utf16 {
            "UTF-16 text isn't supported yet — Veld shows UTF-8 text"
        } else {
            "this looks like a binary file, which a file pane does not show"
        },
    )
}

/// The live worktree whose checkout contains `cwd`, deepest first.
///
/// Both spellings of the directory are tried — as given and canonical — because a
/// worktree is stored as it was registered, and on macOS `/tmp` and
/// `/private/tmp` are one directory with two names.
pub(super) fn worktree_containing(db: &Db, cwd: &Path) -> Option<i64> {
    if !cwd.is_absolute() {
        return None;
    }
    let canonical = cwd.canonicalize().ok();
    let spellings = std::iter::once(cwd).chain(canonical.as_deref());
    for spelling in spellings {
        for dir in spelling.ancestors() {
            let Some(dir) = dir.to_str() else { continue };
            if let Ok(Some(wt)) = db.get_worktree_by_path(dir)
                && wt.trashed_at.is_empty()
            {
                return Some(wt.id);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("wt");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/plan.md"), "# plan\n").unwrap();
        std::fs::write(root.join(".env"), "SECRET=1").unwrap();
        std::fs::write(root.join("logo.png"), "png").unwrap();
        (dir, root)
    }

    #[test]
    fn a_file_inside_the_worktree_reads_by_relative_or_absolute_path() {
        let (_dir, root) = tree();
        let rel = resolve_readable(&root, "docs/plan.md", &[], |_| false).unwrap();
        assert_eq!(rel.display, "docs/plan.md");
        assert!(rel.inside);
        assert_eq!(rel.kind, TextKind::Markdown);
        let abs = root.join("docs/plan.md");
        let by_abs = resolve_readable(&root, abs.to_str().unwrap(), &[], |_| false).unwrap();
        assert_eq!(by_abs, rel, "the same file is the same answer either way");
    }

    #[test]
    fn secrets_unknown_kinds_and_escapes_are_refused() {
        let (_dir, root) = tree();
        for refused in [".env", "logo.png", "../wt/.env", "docs", "nope.md"] {
            assert!(
                resolve_readable(&root, refused, &[], |_| true).is_none(),
                "{refused}"
            );
        }
    }

    /// Outside the worktree only the two lists let a file in, and the deny list
    /// still applies to the whole absolute path.
    #[test]
    fn outside_the_worktree_needs_a_folder_or_a_grant() {
        let (dir, root) = tree();
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(notes.join(".ssh")).unwrap();
        std::fs::write(notes.join("idea.md"), "x").unwrap();
        std::fs::write(notes.join(".ssh/config.md"), "x").unwrap();
        let idea = notes.join("idea.md").canonicalize().unwrap();
        let idea_str = idea.to_str().unwrap();

        assert!(resolve_readable(&root, idea_str, &[], |_| false).is_none());
        let granted = resolve_readable(&root, idea_str, &[], |p| p == idea).unwrap();
        assert!(!granted.inside);
        assert_eq!(granted.display, idea_str);

        let folders = [notes.canonicalize().unwrap()];
        assert!(resolve_readable(&root, idea_str, &folders, |_| false).is_some());
        let secret = notes.join(".ssh/config.md");
        assert!(
            resolve_readable(&root, secret.to_str().unwrap(), &folders, |_| true).is_none(),
            "a listed folder does not reach a sensitive segment"
        );
    }

    /// A symlink is judged by what it points at, inside or out.
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_judged_by_its_target() {
        let (dir, root) = tree();
        std::os::unix::fs::symlink(".env", root.join("notes.md")).unwrap();
        assert!(resolve_readable(&root, "notes.md", &[], |_| true).is_none());

        let outside = dir.path().join("elsewhere.md");
        std::fs::write(&outside, "x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link.md")).unwrap();
        assert!(
            resolve_readable(&root, "link.md", &[], |_| false).is_none(),
            "a link out of the worktree needs the same grant its target would"
        );
        assert!(resolve_readable(&root, "link.md", &[], |_| true).is_some());
    }

    /// A login a CLI keeps in a well-known file is refused outside the worktree
    /// even from a folder the user listed — its extension alone would let it in.
    #[test]
    fn a_credential_file_outside_is_refused_whatever_lets_it_in() {
        let (dir, root) = tree();
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        for name in [
            "auth.json",
            "hosts.yml",
            "tokens.json",
            "gcp-credentials.json",
        ] {
            std::fs::write(notes.join(name), "{}").unwrap();
        }
        std::fs::write(notes.join("idea.md"), "x").unwrap();
        let folders = [notes.canonicalize().unwrap()];
        let at = |name: &str| notes.join(name).to_string_lossy().into_owned();
        for name in [
            "auth.json",
            "hosts.yml",
            "tokens.json",
            "gcp-credentials.json",
        ] {
            assert!(
                resolve_readable(&root, &at(name), &folders, |_| true).is_none(),
                "{name}"
            );
        }
        assert!(resolve_readable(&root, &at("idea.md"), &folders, |_| false).is_some());
    }

    #[test]
    fn a_utf16_file_is_told_it_is_utf16_not_binary() {
        let message = |bytes: &[u8]| {
            let (status, Json(body)) = not_text(bytes);
            assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
            body["error"].as_str().unwrap().to_owned()
        };
        assert!(message(&[0xFF, 0xFE, b'h', 0]).contains("UTF-16"));
        assert!(message(&[0xFE, 0xFF, 0, b'h']).contains("UTF-16"));
        assert!(message(&[0x7F, b'E', b'L', b'F', 0]).contains("binary"));
    }

    /// A dotfile manager's link: the name asked for is a login, the file it lands
    /// on is not. The spelling is judged too, so it is refused.
    #[cfg(unix)]
    #[test]
    fn a_credential_reached_through_a_dotfile_link_is_refused() {
        let (dir, root) = tree();
        let dotfiles = dir.path().join("dotfiles");
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(dotfiles.join("gh-hosts.yml"), "token: x").unwrap();
        std::os::unix::fs::symlink(dotfiles.join("gh-hosts.yml"), notes.join("hosts.yml")).unwrap();
        let folders = [
            notes.canonicalize().unwrap(),
            dotfiles.canonicalize().unwrap(),
        ];
        let link = notes.join("hosts.yml");
        assert!(resolve_readable(&root, link.to_str().unwrap(), &folders, |_| true).is_none());
        // The file by its own name is not a login, and still reads.
        let target = dotfiles.join("gh-hosts.yml");
        assert!(resolve_readable(&root, target.to_str().unwrap(), &folders, |_| true).is_some());
    }

    #[test]
    fn a_home_credential_is_judged_by_its_spelling_and_xdg_config() {
        let home = PathBuf::from("/home/me");
        let at = CredentialHomes {
            homes: vec![home.clone()],
            configs: vec![PathBuf::from("/data/cfg")],
        };
        let dotfiles = Path::new("/home/me/dotfiles/docker/config.json");
        // `~/.docker/config.json` → `~/dotfiles/…`: the spelling is the login.
        assert!(credential_by_either_spelling(
            dotfiles,
            &home.join(".docker/config.json"),
            &at
        ));
        // A harmless-looking link to `~/.aws/config`: the canonical file is.
        assert!(credential_by_either_spelling(
            &home.join(".aws/config"),
            Path::new("/home/me/notes/aws.toml"),
            &at
        ));
        // `gh` under `$XDG_CONFIG_HOME`, wherever that is.
        assert!(credential_by_either_spelling(
            Path::new("/data/cfg/gh/config.yml"),
            Path::new("/data/cfg/gh/config.yml"),
            &at
        ));
        assert!(!credential_by_either_spelling(
            dotfiles,
            Path::new("/home/me/notes/docker.json"),
            &at
        ));
    }

    /// A database with no worktrees in it, for the rules that read settings or
    /// write grants.
    fn test_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
        (dir, db)
    }

    fn grants(db: &Db, root: &Path) -> Option<String> {
        db.kv_get(&format!("files.reads:{}", root.to_str().unwrap()))
            .unwrap()
    }

    /// A git checkout, so `git ls-files` has something to say: one `tabKeys.ts`
    /// deep in it, and two files called `mod.rs`. Canonical, so the answers
    /// compare equal to what `canonicalize` gives back on macOS.
    async fn git_tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("ui/src/panes")).unwrap();
        std::fs::write(root.join("ui/src/panes/tabKeys.ts"), "export {};").unwrap();
        std::fs::write(root.join("README.md"), "# hi").unwrap();
        for sub in ["a", "b"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
            std::fs::write(root.join(sub).join("mod.rs"), "").unwrap();
        }
        let out = tokio::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .await
            .unwrap();
        assert!(out.status.success());
        (dir, root)
    }

    /// A clicked relative path is tried at the root first, then by suffix; one
    /// match or nothing.
    #[tokio::test]
    async fn a_clicked_path_is_located_like_a_file_action_locates_it() {
        let (_dir, root) = git_tree().await;
        let r = root.to_str().unwrap();
        assert_eq!(locate(r, "README.md").await.as_deref(), Some("README.md"));
        let found = locate(r, "tabKeys.ts")
            .await
            .expect("one file has that name");
        assert_eq!(
            found,
            root.join("ui/src/panes/tabKeys.ts").to_string_lossy(),
            "found by suffix, absolute"
        );
        assert!(locate(r, "panes/tabKeys.ts").await.is_some());
        assert_eq!(locate(r, "mod.rs").await, None, "two files: ambiguous");
        assert_eq!(locate(r, "nowhere.ts").await, None);
        assert_eq!(
            locate(r, "/abs/as/is.md").await.as_deref(),
            Some("/abs/as/is.md")
        );
    }

    /// The grant route's second half: a file inside the worktree, however it was
    /// named, answers with its relative path and writes no grant; one outside with
    /// no other rule is granted by its absolute path.
    #[tokio::test]
    async fn only_a_file_no_other_rule_lets_in_is_granted() {
        let (_dir, root) = git_tree().await;
        let (_db_dir, db) = test_db();
        let r = root.to_str().unwrap();

        let by_suffix = locate(r, "tabKeys.ts").await.unwrap();
        let inside = grant_blocking(&db, r, &by_suffix).unwrap();
        assert_eq!(inside.display, "ui/src/panes/tabKeys.ts");
        assert!(inside.inside);
        let relative = grant_blocking(&db, r, "README.md").unwrap();
        assert_eq!(relative.display, "README.md");
        assert_eq!(grants(&db, &root), None, "inside needs no grant");

        let elsewhere = tempfile::TempDir::new().unwrap();
        let plan = elsewhere.path().canonicalize().unwrap().join("plan.md");
        std::fs::write(&plan, "# plan").unwrap();
        let plan_str = plan.to_str().unwrap();
        let outside = grant_blocking(&db, r, plan_str).unwrap();
        assert_eq!(outside.display, plan_str, "absolute outside");
        assert!(db.file_read_granted(r, plan_str).unwrap());

        // Under an extra folder the read is already allowed, so nothing is added.
        let other = elsewhere.path().canonicalize().unwrap().join("other.md");
        std::fs::write(&other, "x").unwrap();
        db.patch_settings(
            &[(
                "files.extraFolders".to_owned(),
                serde_json::json!([elsewhere.path().canonicalize().unwrap()]),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
        grant_blocking(&db, r, other.to_str().unwrap()).unwrap();
        assert!(!db.file_read_granted(r, other.to_str().unwrap()).unwrap());
    }

    fn status_of(res: Result<Resolved, ApiError>) -> StatusCode {
        match res {
            Ok(r) => panic!("expected a refusal, got {r:?}"),
            Err((status, _)) => status,
        }
    }

    /// Where `veld ide open` sends each kind of target — and that deciding writes
    /// no grant: that happens only around the push.
    #[test]
    fn an_open_target_routes_by_kind_and_place() {
        let (dir, root) = tree();
        let (_db_dir, db) = test_db();
        let r = root.to_str().unwrap();
        let at = |p: &Path| p.canonicalize().unwrap().to_string_lossy().into_owned();

        assert_eq!(
            resolve_open_target(&db, r, "https://example.com/a?b=1").unwrap(),
            Resolved::Url {
                url: "https://example.com/a?b=1".to_owned()
            }
        );
        assert_eq!(
            resolve_open_target(&db, r, &at(&root.join("docs/plan.md"))).unwrap(),
            Resolved::File {
                display: "docs/plan.md".to_owned(),
                grant: false
            }
        );

        let outside = dir.path().join("idea.md");
        std::fs::write(&outside, "x").unwrap();
        assert_eq!(
            resolve_open_target(&db, r, &at(&outside)).unwrap(),
            Resolved::File {
                display: at(&outside),
                grant: true
            }
        );
        assert!(
            !db.file_read_granted(r, &at(&outside)).unwrap(),
            "resolving writes nothing"
        );

        // A page inside the worktree goes to a browser pane (503 here only because
        // no file origin is registered in a test); outside, it cannot.
        std::fs::write(root.join("deck.html"), "<p>").unwrap();
        match resolve_open_target(&db, r, &at(&root.join("deck.html"))) {
            Ok(Resolved::Url { .. }) => {}
            Err((StatusCode::SERVICE_UNAVAILABLE, _)) => {}
            other => panic!("an inside page is for a browser pane: {other:?}"),
        }
        let page = dir.path().join("page.html");
        std::fs::write(&page, "<p>").unwrap();
        assert_eq!(
            status_of(resolve_open_target(&db, r, &at(&page))),
            StatusCode::UNPROCESSABLE_ENTITY
        );

        std::fs::write(root.join("secrets.md"), "x").unwrap();
        assert_eq!(
            status_of(resolve_open_target(&db, r, &at(&root.join("secrets.md")))),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_of(resolve_open_target(&db, r, "docs/plan.md")),
            StatusCode::BAD_REQUEST,
            "relative"
        );
    }

    /// Too big or binary fails at `veld ide open` time, with the codes `file-text`
    /// would have answered later.
    #[test]
    fn an_open_text_target_is_checked_for_size_and_binary_first() {
        let (_dir, root) = tree();
        let (_db_dir, db) = test_db();
        let r = root.to_str().unwrap();
        let big = root.join("big.log");
        std::fs::write(&big, vec![b'a'; MAX_TEXT_BYTES as usize + 1]).unwrap();
        let bin = root.join("data.json");
        std::fs::write(&bin, b"{\0\0}").unwrap();
        assert_eq!(
            status_of(resolve_open_target(&db, r, big.to_str().unwrap())),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            status_of(resolve_open_target(&db, r, bin.to_str().unwrap())),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }

    fn discovered(path: &Path, is_main: bool) -> veld_core::db::DiscoveredWorktree {
        veld_core::db::DiscoveredWorktree {
            path: path.to_string_lossy().into_owned(),
            branch: if is_main { "main" } else { "feat" }.to_owned(),
            is_main,
        }
    }

    /// The deepest worktree wins, and a cwd spelled through a symlink (macOS's
    /// `/var` → `/private/var`) still finds one registered canonically.
    #[test]
    fn the_worktree_containing_a_cwd_is_the_deepest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_db_dir, db) = test_db();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        let nested = repo.join(".worktrees/feat");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(nested.join("src")).unwrap();
        db.upsert_repo(&repo, "repo").unwrap();
        db.sync_worktrees(
            &repo,
            &[discovered(&repo, true), discovered(&nested, false)],
        )
        .unwrap();
        let id = |p: &Path| {
            db.get_worktree_by_path(p.to_str().unwrap())
                .unwrap()
                .unwrap()
                .id
        };

        assert_eq!(worktree_containing(&db, &repo.join("src")), Some(id(&repo)));
        assert_eq!(
            worktree_containing(&db, &nested.join("src")),
            Some(id(&nested))
        );
        assert_eq!(worktree_containing(&db, tmp.path()), None);
        assert_eq!(worktree_containing(&db, Path::new("src")), None, "relative");

        // The same directory through a symlink: the canonical spelling matches.
        let alias = tmp.path().join("alias");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&nested, &alias).unwrap();
            assert_eq!(
                worktree_containing(&db, &alias.join("src")),
                Some(id(&nested))
            );
        }
    }

    /// A request each of [`route_table`]'s routes accepts the shape of: method,
    /// URI, JSON body. A route with no sample here fails the two tests below.
    fn sample(route: &str) -> (&'static str, &'static str, &'static str) {
        match route {
            "/api/worktrees/{id}/file-text" => ("GET", "/api/worktrees/1/file-text?path=a.md", ""),
            "/api/worktrees/{id}/file-grants" => (
                "POST",
                "/api/worktrees/1/file-grants",
                r#"{"path":"/a.md"}"#,
            ),
            "/api/ide/open" => (
                "POST",
                "/api/ide/open",
                r#"{"worktreeId":1,"target":"/a.md"}"#,
            ),
            other => panic!("new route {other}: give it a sample request here"),
        }
    }

    #[tokio::test]
    async fn every_route_here_refuses_a_host_that_is_not_local() {
        use tower::ServiceExt;
        for (route, _) in route_table() {
            let (method, uri, body) = sample(route);
            let res = routes()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .header("X-Veld-Request", "1")
                        .header("Host", "evil.example:19898")
                        .body(axum::body::Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn every_route_here_requires_the_csrf_header() {
        use tower::ServiceExt;
        for (route, _) in route_table() {
            let (method, uri, body) = sample(route);
            let res = routes()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn ide_open_wants_exactly_one_way_to_name_the_worktree() {
        use tower::ServiceExt;
        for body in [
            r#"{"target":"/a.md"}"#,
            r#"{"worktreeId":1,"cwd":"/x","target":"/a.md"}"#,
        ] {
            let res = routes()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/api/ide/open")
                        .header("content-type", "application/json")
                        .header("X-Veld-Request", "1")
                        .body(axum::body::Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{body}");
        }
    }
}
