//! Local files, served to a browser pane from an origin the management API is not on.
//!
//! # Why this is a second listener
//!
//! The content is HTML an agent just wrote — that is the feature, not an edge case.
//! Served from the daemon's own origin, its scripts would be same-origin with
//! `/api/worktrees/{id}/delete`, `/api/settings` and everything else: the CSRF check
//! guarding those is header *presence* only (`X-Veld-Request`), which same-origin
//! script sets trivially, and there is no CORS layer to lean on because responses
//! carry no `Access-Control-Allow-Origin` at all. A prompt-injected or simply buggy
//! deck would have the run of the daemon.
//!
//! So the bytes go out on their own `TcpListener`, whose router has *only* the read
//! route on it, reached through Caddy at [`veld_core::instance::files_host`]. A
//! **path** under the dashboard would not have worked: an origin is scheme, host and
//! port, and a path is none of those.
//!
//! Two consequences worth naming, because they look like oversights:
//!
//! - The listener takes an **ephemeral** port and Caddy's route holds the upstream,
//!   re-registered on every daemon start under a stable `route_id`. That is how run
//!   URLs already work, and it is what keeps the *public* URL stable — a pane URL
//!   persisted in `veld.panes.v1` still resolves after a restart that moved the port.
//! - This origin is deliberately **absent** from `pty::allowed_origins`, so a page
//!   served here cannot open a terminal or IDE WebSocket. Pinned by
//!   [`tests::the_file_origin_can_never_open_a_terminal_socket`], because the failure
//!   would be silent and the fix is one line someone could "helpfully" add.
//!
//! # What confines a read
//!
//! The first path segment is a **grant** — an unguessable id the daemon resolves
//! back to a worktree root ([`veld_core::db::Db::file_grant_for_root`]), the same
//! shape as a PTY ticket carrying its worktree dir. The client never names a root.
//! Every read is then canonicalised and prefix-checked against it, so a symlink
//! pointing out of the worktree is refused even though `..` never appeared.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use axum::{
    Router,
    extract::Path as UrlPath,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tracing::{info, warn};
use veld_core::db::Db;
use veld_core::files;

/// Biggest file this will read into memory and hand back.
///
/// It reads whole rather than streaming, which is the honest simplification for a
/// local viewer: the cost is memory proportional to one file, and the benefit is no
/// `Range` state machine. The cap exists so a stray multi-gigabyte artefact is a
/// refusal rather than a daemon the OS kills. Large media that needs seeking is a
/// named follow-up, not a thing this quietly half-does.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Whether the Caddy route was registered, so file URLs actually resolve.
///
/// Read by the callers that can still do something useful when it is false: the
/// `open` shim falls through to the real opener (the pre-Veld behaviour, which is
/// the right answer), and the recently-edited list stays empty rather than offering
/// rows that would load nothing.
static READY: AtomicBool = AtomicBool::new(false);

/// Whether file serving is usable on this instance.
pub fn is_ready() -> bool {
    READY.load(Ordering::Relaxed)
}

/// The origin a pane loads file bytes from — `https://files.veld.localhost[:port]`.
///
/// The port is omitted when Caddy is on 443 and included otherwise, which is how a
/// browser serialises an origin and therefore how it has to be written here.
pub fn origin() -> String {
    let host = veld_core::instance::files_host();
    let (https, _) = super::pty::dashboard_ports();
    match https {
        443 => format!("https://{host}"),
        port => format!("https://{host}:{port}"),
    }
}

/// The URL that shows `rel_path` inside `root`, or `None` if it cannot be shown.
///
/// `None` covers both "file serving is not up" and "that path is not one this
/// worktree can serve", so a caller gets one answer to act on rather than a URL
/// that will 404 later.
pub fn url_for(db: &Db, root: &str, rel_path: &str) -> Option<String> {
    if !is_ready() {
        return None;
    }
    let grant = db.file_grant_for_root(root).ok()?;
    url_in(&origin(), &grant, rel_path)
}

/// [`url_for`] with the origin and grant already resolved.
///
/// Split out because a list of a hundred files needs both exactly once, and the
/// combined version would otherwise re-read `~/.veld/setup.json` (via `origin`)
/// and re-query the grant per row.
fn url_in(origin: &str, grant: &str, rel_path: &str) -> Option<String> {
    let rel = normalize_relative(rel_path)?;
    if files::servable_type(&rel).is_none() || files::is_sensitive(&rel) {
        return None;
    }
    // Per segment, so the separators survive and everything else — spaces, `#`,
    // `%`, non-ASCII — does not turn one path into a different URL.
    let encoded = rel
        .split('/')
        .map(veld_core::percent::encode_component)
        .collect::<Vec<_>>()
        .join("/");
    Some(format!("{origin}/{grant}/{encoded}"))
}

/// A client-supplied relative path, reduced to a form safe to join.
///
/// Rejects — rather than sanitises — anything that is not already a plain relative
/// path: absolute paths, any `..`, any root or prefix component, and the empty
/// string. Sanitising would mean deciding what somebody meant by `../../etc`, and
/// there is no good answer to that question.
///
/// `.` components are dropped, because `./deck.html` is what a person types.
fn normalize_relative(rel: &str) -> Option<String> {
    if rel.is_empty() {
        return None;
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return None;
    }
    let mut out = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(seg) => out.push(seg.to_str()?.to_owned()),
            Component::CurDir => {}
            // `..`, `/` and a Windows prefix all mean this is not a path a grant
            // may be joined with.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out.join("/"))
}

/// The router for the **file origin**. Nothing else is ever mounted here.
pub fn origin_routes() -> Router {
    Router::new().route("/{grant}/{*path}", get(serve))
}

/// Routes on the **management** origin, for the `/ide` bundle.
///
/// These are here rather than on the file origin for one reason: the renderer has to
/// *read* their bodies. A `fetch` from `/ide` to the file origin is cross-origin and
/// carries no `Access-Control-Allow-Origin`, so the response would be opaque — which
/// is the isolation working, not a bug to punch a hole in. So the questions the UI
/// asks (what is worth opening, has it changed) are answered same-origin, and only
/// the bytes come from the other host.
pub fn api_routes() -> Router {
    Router::new()
        .route("/api/worktrees/{id}/viewable-files", get(list_viewable))
        .route("/api/worktrees/{id}/file-stat", get(file_stat))
}

/// How deep a recency scan descends.
///
/// Generated artefacts sit near the top of a worktree; twelve levels is past
/// anything a person navigates to and short of a pathological tree.
const MAX_SCAN_DEPTH: usize = 12;
/// How many directory entries one scan will look at, whatever it has found.
const MAX_SCAN_ENTRIES: usize = 50_000;
/// How many files the list returns. It is a "what did I just make" list, not a file
/// manager; past a screenful, recency has stopped being the useful ordering.
const MAX_SCAN_RESULTS: usize = 100;
/// How long a scan may take before it returns what it has.
///
/// A partial list beats a spinner: the file somebody wants is almost always among
/// the newest, and those are found in the first few milliseconds.
const SCAN_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);

/// One row of the recently-edited list.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewableFile {
    /// Worktree-relative path — what the row shows, and what it is matched against.
    name: String,
    /// The absolute file-origin URL a pane loads.
    url: String,
    /// Modification time in milliseconds, so the client can render "2 minutes ago"
    /// without re-deriving the ordering.
    mtime_ms: i64,
}

/// `GET /api/worktrees/{id}/viewable-files` — the worktree's viewable files, newest
/// first.
///
/// Reports `ready: false` rather than an error when file serving is not up, because
/// the honest UI for that is a note explaining why the list is empty, not a failed
/// request the user cannot act on.
async fn list_viewable(
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<axum::Json<serde_json::Value>, super::desktop::ApiError> {
    let db = super::management::open_db().map_err(|_| {
        super::desktop::err(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable")
    })?;
    let worktree = db
        .get_worktree(id)
        .map_err(super::desktop::db_err)?
        .ok_or_else(|| super::desktop::err(StatusCode::NOT_FOUND, "no such worktree"))?;
    if !is_ready() {
        return Ok(axum::Json(
            serde_json::json!({ "ready": false, "files": [] }),
        ));
    }
    let policy = db.view_policy();
    let root = worktree.path.clone();
    let scan_root = root.clone();
    // Blocking filesystem walk, off the runtime's worker threads.
    let found = tokio::task::spawn_blocking(move || scan(Path::new(&scan_root), &policy))
        .await
        .unwrap_or_default();

    // Origin and grant once for the whole list, not once per row.
    let origin = origin();
    let grant = db
        .file_grant_for_root(&root)
        .map_err(super::desktop::db_err)?;
    let files: Vec<ViewableFile> = found
        .into_iter()
        .filter_map(|(rel, mtime_ms)| {
            url_in(&origin, &grant, &rel).map(|url| ViewableFile {
                name: rel,
                url,
                mtime_ms,
            })
        })
        .collect();
    Ok(axum::Json(
        serde_json::json!({ "ready": true, "files": files }),
    ))
}

/// Walk `root` for viewable files, newest first.
///
/// Bounded four ways (depth, entries, results, wall clock) because this runs on a
/// directory nobody has promised anything about. Returns what it has when a bound is
/// hit rather than failing — a short list is useful and an error is not.
///
/// Ordering is by modification time descending, which is the whole reason the list
/// exists: the file somebody wants is the one an agent wrote a moment ago.
fn scan(root: &Path, policy: &files::ViewPolicy) -> Vec<(String, i64)> {
    let started = std::time::Instant::now();
    let mut seen = 0usize;
    let mut out: Vec<(String, i64)> = Vec::new();
    // Explicit stack rather than recursion: depth is bounded by a constant here,
    // but a symlink loop is not, and a walk that can be made to recurse is a walk
    // that can be made to overflow.
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_SCAN_DEPTH || seen >= MAX_SCAN_ENTRIES || started.elapsed() > SCAN_BUDGET {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen >= MAX_SCAN_ENTRIES || started.elapsed() > SCAN_BUDGET {
                break;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // `file_type` rather than `metadata`: it does not follow symlinks, so a
            // link pointing at its own ancestor is skipped instead of walked. A
            // symlinked *file* is skipped too, which is the conservative half of
            // the same rule — `confine` would refuse it at serve time anyway if it
            // pointed outside.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if files::SKIP_DIRS.contains(&name) {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root).map(Path::to_path_buf) else {
                continue;
            };
            let Some(rel) = rel.to_str() else { continue };
            if !files::is_viewable(rel, policy) {
                continue;
            }
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            out.push((rel.to_owned(), mtime));
        }
    }
    out.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));
    out.truncate(MAX_SCAN_RESULTS);
    out
}

/// What the client sends to ask whether a watched file has changed.
#[derive(Debug, serde::Deserialize)]
struct StatQuery {
    path: String,
}

/// `GET /api/worktrees/{id}/file-stat?path=<rel>` — the timestamp a file pane polls.
///
/// This is what makes reload-on-change possible without injecting anything into the
/// page: the renderer compares the number it last saw and reloads the view when it
/// moves. Same-origin, so the body is readable; confined by the same rules as a
/// read, so it is not a stat oracle for the rest of the disk.
async fn file_stat(
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::extract::Query(q): axum::extract::Query<StatQuery>,
) -> Result<axum::Json<serde_json::Value>, super::desktop::ApiError> {
    let db = super::management::open_db().map_err(|_| {
        super::desktop::err(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable")
    })?;
    let worktree = db
        .get_worktree(id)
        .map_err(super::desktop::db_err)?
        .ok_or_else(|| super::desktop::err(StatusCode::NOT_FOUND, "no such worktree"))?;
    let rel = normalize_relative(&q.path)
        .ok_or_else(|| super::desktop::err(StatusCode::BAD_REQUEST, "not a relative path"))?;
    if files::servable_type(&rel).is_none() || files::is_sensitive(&rel) {
        return Err(super::desktop::err(
            StatusCode::NOT_FOUND,
            "not a file veld serves",
        ));
    }
    let full = confine(Path::new(&worktree.path), &rel)
        .ok_or_else(|| super::desktop::err(StatusCode::NOT_FOUND, "no such file"))?;
    let meta = tokio::fs::metadata(&full)
        .await
        .map_err(|_| super::desktop::err(StatusCode::NOT_FOUND, "no such file"))?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok(axum::Json(serde_json::json!({
        "mtimeMs": mtime_ms,
        "size": meta.len(),
    })))
}

/// `GET /{grant}/{*path}` on the file origin.
async fn serve(UrlPath((grant, path)): UrlPath<(String, String)>) -> Response {
    match read_file(&grant, &path).await {
        Ok((body, content_type)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                // The whole point of a file pane is watching an agent rewrite the
                // file. A cached response is a pane that reloads and shows the old
                // deck, which reads as "reloading is broken".
                (header::CACHE_CONTROL, "no-store"),
                // The type table is closed and deliberately narrow; re-sniffing it
                // in the renderer would reintroduce every guess it excludes.
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            body,
        )
            .into_response(),
        // One status for every refusal. A grant that does not exist, a path outside
        // the worktree, a type not served and a file that is not there are all "no
        // such thing here" as far as a caller is concerned, and distinguishing them
        // would confirm which of a guessed grant or a guessed path was the wrong
        // half.
        Err(status) => status.into_response(),
    }
}

/// Resolve, confine, and read. Every failure is a status, never a message.
async fn read_file(grant: &str, path: &str) -> Result<(Vec<u8>, &'static str), StatusCode> {
    let rel = normalize_relative(path).ok_or(StatusCode::NOT_FOUND)?;
    let content_type = files::servable_type(&rel).ok_or(StatusCode::NOT_FOUND)?;
    if files::is_sensitive(&rel) {
        return Err(StatusCode::NOT_FOUND);
    }
    let root = root_for_grant(grant).ok_or(StatusCode::NOT_FOUND)?;
    let full = confine(&root, &rel).ok_or(StatusCode::NOT_FOUND)?;

    let meta = tokio::fs::metadata(&full)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if !meta.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let bytes = tokio::fs::read(&full)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((bytes, content_type))
}

/// The worktree root a grant resolves to, if it is still a live worktree.
///
/// The second half is not redundant. A grant is stored forever (it is keyed on a
/// path, so that a reused row id cannot inherit one), which means a worktree that
/// has since been deleted or trashed still has a grant sitting in `kv`. Checking
/// the registry here is what makes the grant stop working when the worktree does.
fn root_for_grant(grant: &str) -> Option<PathBuf> {
    if !is_grant_shaped(grant) {
        return None;
    }
    let db = super::management::open_db().ok()?;
    let root = db.file_grant_root(grant).ok().flatten()?;
    let worktree = db.get_worktree_by_path(&root).ok().flatten()?;
    // `""` is a live worktree; anything else is a removal the user has asked for.
    if !worktree.trashed_at.is_empty() {
        return None;
    }
    Some(PathBuf::from(root))
}

/// Whether a string is shaped like a grant, checked before it reaches the database.
///
/// A simple-form UUID: 32 lowercase hex characters. Cheap, and it keeps a path
/// segment full of anything else from becoming a query.
fn is_grant_shaped(grant: &str) -> bool {
    grant.len() == 32 && grant.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Join `rel` onto `root` and prove the result is still inside it.
///
/// [`normalize_relative`] has already refused `..`, so this exists for the case it
/// cannot see: a **symlink** inside the worktree pointing out of it. Both sides are
/// canonicalised and compared, which resolves every link on the way.
fn confine(root: &Path, rel: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let full = root.join(rel).canonicalize().ok()?;
    full.starts_with(&root).then_some(full)
}

/// Start the file origin: bind a loopback listener, serve it, register its Caddy
/// route.
///
/// Best-effort with backoff, like the management route beside it in `main.rs` — the
/// helper may still be booting. A failure here is not fatal and not silent: nothing
/// sets [`READY`], so every caller degrades to the behaviour it had before this
/// feature existed.
pub async fn start() {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(l) => l,
        Err(e) => {
            warn!("file serving disabled: could not bind a loopback port ({e})");
            return;
        }
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(e) => {
            warn!("file serving disabled: no local address ({e})");
            return;
        }
    };
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, origin_routes()).await {
            warn!("file origin stopped: {e}");
            READY.store(false, Ordering::Relaxed);
        }
    });

    let host = veld_core::instance::files_host();
    let route = serde_json::json!({
        // Stable *per instance*, so a restart replaces this daemon's route rather
        // than adding a second one pointing at a port nothing is listening on —
        // and never replaces another instance's. See `files_route_id`.
        "route_id": veld_core::instance::files_route_id(),
        "hostname": host,
        "upstream": format!("127.0.0.1:{port}"),
    });
    for attempt in 0..5u64 {
        if let Ok(helper) = veld_core::helper::HelperClient::connect().await
            && helper.add_route(route.clone()).await.is_ok()
        {
            READY.store(true, Ordering::Relaxed);
            info!("local files served at https://{host} (upstream 127.0.0.1:{port})");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2 * (attempt + 1))).await;
    }
    warn!(
        "local files will not open in a pane: could not register the {host} route \
         (helper unreachable). `open <file>` falls through to your system browser."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_is_refused_rather_than_sanitised() {
        assert_eq!(
            normalize_relative("deck.html").as_deref(),
            Some("deck.html")
        );
        assert_eq!(
            normalize_relative("./docs/deck.html").as_deref(),
            Some("docs/deck.html"),
            "a leading ./ is what a person types"
        );
        // Every shape that is not a plain relative path.
        for bad in [
            "",
            ".",
            "/etc/passwd",
            "../outside.html",
            "docs/../../outside.html",
            "..",
            "docs/..",
        ] {
            assert_eq!(normalize_relative(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_grant_is_checked_for_shape_before_it_is_looked_up() {
        assert!(is_grant_shaped(&"a".repeat(32)));
        assert!(is_grant_shaped(&uuid::Uuid::new_v4().simple().to_string()));
        for bad in [
            "",
            "short",
            &"g".repeat(32),
            &"a".repeat(31),
            &"a".repeat(33),
        ] {
            assert!(!is_grant_shaped(bad), "{bad:?}");
        }
    }

    /// A symlink out of the worktree is refused even with no `..` in the request.
    #[test]
    fn a_symlink_cannot_leave_the_worktree() {
        let outside = tempfile::TempDir::new().unwrap();
        let secret = outside.path().join("secret.html");
        std::fs::write(&secret, "<h1>not yours</h1>").unwrap();

        let root = tempfile::TempDir::new().unwrap();
        std::fs::write(root.path().join("ok.html"), "<h1>mine</h1>").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, root.path().join("escape.html")).unwrap();

        assert!(
            confine(root.path(), "ok.html").is_some(),
            "a file in the worktree resolves"
        );
        #[cfg(unix)]
        assert_eq!(
            confine(root.path(), "escape.html"),
            None,
            "a symlink pointing out of the worktree is refused"
        );
    }

    /// The file origin must never be allowed to open a terminal or IDE socket.
    ///
    /// Agent-authored HTML runs on that origin. `allowed_origins` is what decides
    /// who may upgrade `/api/pty/attach`, so a well-meaning addition of this host to
    /// that list would hand a deck a shell — with no other symptom.
    #[test]
    fn the_file_origin_can_never_open_a_terminal_socket() {
        let allowed = crate::feedback_server::pty::allowed_origins();
        let files = veld_core::instance::files_host();
        for origin in &allowed {
            assert!(
                !origin.contains(&files),
                "{origin} would let a served file open a terminal socket"
            );
        }
    }
}
