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
/// local viewer: no `Range` state machine, at the cost of memory proportional to one
/// file *per in-flight request* — there is no concurrency limit here, so k parallel
/// reads cost k times this. The cap exists so a stray multi-gigabyte artefact is a
/// refusal rather than a daemon the OS kills.
///
/// **What that costs, stated rather than disclaimed:** `servable_type` does list
/// `mp4`/`webm`/`ogg`/`mp3`/`wav`, and no `Range` support means such a file plays from
/// the start and **cannot be seeked** — a short clip embedded in a deck works, a long
/// one is frustrating. Ranges are a named follow-up. The rows stay because playing
/// without seeking beats a 404.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// How many whole-file reads may be in flight at once.
///
/// [`MAX_FILE_BYTES`] bounds *one* read; this bounds the product, which is the number
/// that actually decides whether the daemon survives. Without it, a page served from
/// this origin — agent-authored, i.e. the prompt-injectable thing the module docs are
/// about — can same-origin `fetch` a large sibling on ~100 concurrent HTTP/2 streams,
/// and the buffered reads add up until the OS kills a process that owns every
/// terminal holder on the machine.
///
/// Four, not forty: the requests this serves are a person opening a document, and a
/// deck's subresources are small and sequential. Excess requests queue rather than
/// failing, so nothing breaks — it just waits.
///
/// The honest fix is to stream the body instead of buffering it, which would also
/// remove the no-`Range` caveat above. That needs `tokio-util` as a direct dependency
/// (it is only transitive today), and a lockfile change drags
/// `THIRD-PARTY-LICENSES.md` with it — so it is a named follow-up rather than a
/// smuggled-in dependency bump.
const MAX_CONCURRENT_READS: usize = 4;

/// See [`MAX_CONCURRENT_READS`].
static READ_SLOTS: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_READS));

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

/// The prefix every URL under one grant shares — `<origin>/<grant>/`.
///
/// One function rather than two `format!`s for the reason
/// [`veld_core::percent::encode_component`] is one encoder: [`list_viewable`] reports
/// this to the client as `root`, the client strips it back off to recover the path
/// (`filePathIn` in `panes/model.ts`), and [`url_in`] builds the URLs it has to be a
/// prefix of. Two spellings that agree today are two spellings that disagree after
/// somebody edits one, and the failure is panes that silently stop watching.
fn root_url(origin: &str, grant: &str) -> String {
    format!("{origin}/{grant}/")
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
    Some(format!("{}{encoded}", root_url(origin, grant)))
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
///
/// It also answers `root`: the `<origin>/<grant>/` prefix every URL in the list is
/// under. That is what lets a pane recognise a file URL it was *not* handed a path
/// for — a relative link followed inside a deck, or a file opened by the `open` shim
/// — and watch it. The client cannot derive it: the grant is opaque and the origin is
/// not the one the bundle is served from.
///
/// **This does hand the grant to a caller a row would not have.** A body with an
/// empty `files` carried no grant before — an ordinary answer for a worktree with
/// nothing viewable, or with `viewImages`/`viewPlainText` off — and the grant reaches
/// more than the list does: rows are filtered by [`files::is_viewable`], while a read
/// is only checked against `servable_type` and [`files::is_sensitive`], so it also
/// fetches the `*.css`/`*.png`/`*.md` no row offers. It is disclosed anyway because
/// reaching this route already means being a same-origin caller with the CSRF header,
/// i.e. holding the whole management API — including `open-file`, which pushes a file
/// of the caller's choosing into a pane. There is no `Access-Control-Allow-Origin`
/// anywhere on this listener, so a page from another origin cannot read the body.
async fn list_viewable(
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<axum::Json<serde_json::Value>, super::desktop::ApiError> {
    // Gated despite being a GET, for two reasons that both bite. It *writes*: a grant
    // is minted and persisted on first use for this root. And it costs a blocking
    // thread for up to `SCAN_BUDGET` of disk walking, on a worktree id that is a small
    // enumerable integer — so ungated, any page in any browser could fire these in
    // bulk at `veld.localhost` and saturate the pool the rest of the daemon shares. It
    // could never *read* the answer (no `Access-Control-Allow-Origin` anywhere), which
    // is why this was medium rather than critical, but a side-effecting GET with a
    // 1.5-second cost has no business being reachable from a drive-by page.
    super::management::check_csrf(&headers)
        .map_err(|_| super::desktop::err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    let db = super::management::open_db().map_err(|_| {
        super::desktop::err(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable")
    })?;
    let worktree = db
        .get_worktree(id)
        .map_err(super::desktop::db_err)?
        .ok_or_else(|| super::desktop::err(StatusCode::NOT_FOUND, "no such worktree"))?;
    if !is_ready() {
        return Ok(axum::Json(
            serde_json::json!({ "ready": false, "files": [], "root": null }),
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
        serde_json::json!({ "ready": true, "files": files, "root": root_url(&origin, &grant) }),
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
        // **`break` for the budgets, `continue` for the depth.** These are not the same
        // kind of bound and the first version treated them as one: a single directory
        // popped at depth 13 abandoned every entry still on the stack, silently
        // truncating the list for the whole worktree. Running out of time or entries is
        // a reason to stop walking; one branch being too deep is a reason to skip that
        // branch.
        if seen >= MAX_SCAN_ENTRIES || started.elapsed() > SCAN_BUDGET {
            break;
        }
        if depth > MAX_SCAN_DEPTH {
            continue;
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
            // the same rule — `resolve_servable` judges the link's *target* at serve
            // time, so listing one would offer a row whose identity is not what the
            // row says.
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
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::extract::Query(q): axum::extract::Query<StatQuery>,
) -> Result<axum::Json<serde_json::Value>, super::desktop::ApiError> {
    // Gated for the same reason as its neighbour, plus one of its own: ungated it is a
    // mtime-and-size probe for any path a caller cares to name, one request at a time.
    super::management::check_csrf(&headers)
        .map_err(|_| super::desktop::err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    let db = super::management::open_db().map_err(|_| {
        super::desktop::err(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable")
    })?;
    let worktree = db
        .get_worktree(id)
        .map_err(super::desktop::db_err)?
        .ok_or_else(|| super::desktop::err(StatusCode::NOT_FOUND, "no such worktree"))?;
    // The same resolution the read path uses, so a symlink cannot turn this into an
    // mtime-and-size oracle for a file the read path would refuse.
    let (full, _) = resolve_servable(Path::new(&worktree.path), &q.path)
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
    let root = root_for_grant(grant).ok_or(StatusCode::NOT_FOUND)?;
    let (full, content_type) = resolve_servable(&root, path).ok_or(StatusCode::NOT_FOUND)?;

    let meta = tokio::fs::metadata(&full)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if !meta.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    // Held across the read only — the `stat` above is cheap and the permit is about
    // memory, not about disk. `acquire` fails only once the semaphore is closed, which
    // nothing here does.
    let _slot = READ_SLOTS
        .acquire()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
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
/// A simple-form UUID's shape: 32 hex characters. Deliberately checked with
/// `is_ascii_hexdigit`, which also accepts uppercase — a mixed-case spelling is not a
/// grant this daemon ever minted, so it falls through to the lookup and 404s there
/// rather than being rejected twice. Cheap, and it keeps a path segment full of
/// anything else from becoming a database query.
fn is_grant_shaped(grant: &str) -> bool {
    grant.len() == 32 && grant.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A requested path resolved to a real file inside `root`, with the type to serve it
/// as — or `None` for every refusal.
///
/// **The guards run on the resolved path, not the requested one, and that ordering is
/// the whole point of this function.** The first version checked `servable_type` and
/// `is_sensitive` against the string the client sent and only then resolved it, which
/// a single symlink defeated: `deck2.html -> .env`, *inside* the worktree, passed the
/// extension table (`.html`), passed the deny list (the segment is `deck2.html`), and
/// passed confinement (the target really is under the root) — so `.env` was served, as
/// `text/html`, to whatever asked. Reproduced before this was written.
///
/// That attacker needs no code execution: git carries symlinks, so checking out a
/// repository, a tarball or a dependency tree is enough to plant one — which is
/// exactly the case `veld_core::files::is_sensitive` documents itself as existing for.
///
/// So: normalise, resolve, prove it is inside the root, then re-derive the relative
/// path *from the resolved one* and judge that. A symlink to a sibling `.html` still
/// works, because after resolution it is an `.html` inside the worktree — which is the
/// honest answer.
fn resolve_servable(root: &Path, path: &str) -> Option<(PathBuf, &'static str)> {
    let rel = normalize_relative(path)?;
    // Cheap pre-check on the requested path. Not the guard — the one below is — but it
    // refuses the overwhelmingly common junk without touching the filesystem.
    files::servable_type(&rel)?;
    let root = root.canonicalize().ok()?;
    let full = root.join(&rel).canonicalize().ok()?;
    let resolved = full.strip_prefix(&root).ok()?.to_str()?;
    let content_type = files::servable_type(resolved)?;
    if files::is_sensitive(resolved) {
        return None;
    }
    Some((full, content_type))
}

/// Whether this daemon is an instance a person can actually reach by hostname.
///
/// **This exists because `cargo test` was editing the developer's Caddy config.**
/// `veld-daemon/tests/pty_recovery.rs` starts real daemons on random high ports, and
/// `start` below asked the *developer's* helper to register a route for each one. Every
/// test run left another `files-<ephemeral>.veld.localhost` behind, and the helper
/// persists routes across reboots — seven of them had accumulated before this was
/// noticed. That is the same class as the shim directory escaping into `~/.veld`, which
/// this repo has a confinement test for.
///
/// Guarded by construction rather than by an opt-out variable a future test would
/// forget to set: all three signals below are things a *real* instance has and a test
/// daemon does not.
///
/// - the installed daemon runs on [`DEFAULT_DAEMON_PORT`](veld_core::instance::DEFAULT_DAEMON_PORT);
/// - the bootstrap dev tier sets `VELD_MANAGEMENT_HOST`;
/// - a dev instance started as a veld run gets `VELD_URL`/`VELD_PROXY_ORIGINS`, which
///   is what `dev_trusted_origins` reads.
///
/// A test daemon sets `VELD_DB_PATH`, `VELD_PTY_DIR` and a random port, and none of
/// these. `VELD_PTY_DIR` is deliberately *not* the signal: `veld doctor` tells real
/// users to set it when their socket path is too long, and gating on it would switch
/// the feature off for them silently.
fn instance_is_addressable() -> bool {
    veld_core::instance::daemon_port() == veld_core::instance::DEFAULT_DAEMON_PORT
        || veld_core::instance::management_host().is_some()
        || !veld_core::instance::dev_trusted_origins().is_empty()
}

/// Start the file origin: bind a loopback listener, serve it, register its Caddy
/// route.
///
/// Best-effort with backoff, like the management route beside it in `main.rs` — the
/// helper may still be booting. A failure here is not fatal and not silent: nothing
/// sets [`READY`], so every caller degrades to the behaviour it had before this
/// feature existed.
pub async fn start() {
    if !instance_is_addressable() {
        // A daemon nobody can reach by hostname has no business owning one. See the
        // function's own docs — this is the guard that stops `cargo test` editing the
        // developer's Caddy configuration.
        tracing::debug!("file serving off: this daemon is not an addressable instance");
        return;
    }
    let port = veld_core::instance::files_port();
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            // Loud, and not fatal. A fixed port can be squatted; every caller then
            // degrades to the behaviour it had before this feature existed, which is
            // strictly better than the ephemeral-port alternative this replaced (see
            // `instance::files_port` — a persisted route pointing at a recycled port).
            warn!(
                "local files will not open in a pane: port {port} is in use ({e}). \
                 `open <file>` falls through to your system opener."
            );
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
         (helper unreachable). `open <file>` falls through to your system opener."
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
    fn every_url_starts_with_the_root_the_client_is_told() {
        // `list_viewable` answers `root`, and the pane decides "is this a local file,
        // and which one" by stripping exactly that prefix (`filePathIn` in
        // panes/model.ts). So the prefix has to be one URLs in the same body actually
        // start with, and what is left has to be the path.
        //
        // `root_url` is the only spelling of the prefix, so the handler and `url_in`
        // cannot drift apart; what this pins is the other half — that a URL really is
        // that prefix plus the path, and not the prefix plus something the client
        // would have to know more to undo.
        //
        // Written out longhand rather than as `strip_prefix(&root_url(..))`: the two
        // sides sharing one function is what makes *that* comparison hold for any
        // `root_url` at all, including one missing its trailing `/` — which the router
        // `/{grant}/{*path}` never matches, so every file URL would 404 with this test
        // still green.
        let origin = "https://files.veld.localhost:18443";
        let grant = "a".repeat(32);
        let root = format!("{origin}/{grant}/");
        assert_eq!(root_url(origin, &grant), root);
        for rel in ["deck.html", "notes/deck.html", "a/b/c/slides.pdf"] {
            let url = url_in(origin, &grant, rel).expect("servable");
            let rest = url
                .strip_prefix(&root)
                .unwrap_or_else(|| panic!("{url} is not under {root}"));
            assert_eq!(rest, rel, "what is left of {url} has to be the path");
        }
        // A percent-encoded segment survives the round trip the client makes with
        // `decodeURIComponent`, which is the other half of the same contract.
        let url = url_in(origin, &grant, "my notes/café deck.html").expect("servable");
        assert_eq!(
            url.strip_prefix(&root),
            Some("my%20notes/caf%C3%A9%20deck.html")
        );
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
            resolve_servable(root.path(), "ok.html").is_some(),
            "a file in the worktree resolves"
        );
        #[cfg(unix)]
        assert!(
            resolve_servable(root.path(), "escape.html").is_none(),
            "a symlink pointing out of the worktree is refused"
        );
    }

    /// A symlink *inside* the worktree cannot launder a refused file into a served one.
    ///
    /// The bug this pins was live and reproduced: the extension table and the deny list
    /// were evaluated on the path the client asked for, so `deck.html -> .env` satisfied
    /// both (`.html`, and a segment that is not `.env`) and confinement too — the target
    /// genuinely is inside the root. `.env` was then served as `text/html`.
    ///
    /// Planting one needs no code execution: git carries symlinks, so a checked-out
    /// repository or an unpacked dependency is enough. Every case below is a *different*
    /// guard being laundered, which is why they are not one assertion.
    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_worktree_cannot_launder_a_refused_file() {
        let root = tempfile::TempDir::new().unwrap();
        let at = |p: &str| root.path().join(p);
        std::fs::write(at(".env"), "SECRET=hunter2").unwrap();
        std::fs::create_dir_all(at(".git")).unwrap();
        std::fs::write(at(".git/config"), "[remote]").unwrap();
        std::fs::write(at("deploy.pem"), "-----BEGIN").unwrap();
        std::fs::write(at("db.sqlite"), "SQLite format 3").unwrap();
        std::fs::write(at("real.html"), "<h1>fine</h1>").unwrap();

        // Each of these is a `.html` request — so the extension table always says yes —
        // pointing at something a different guard is supposed to refuse.
        for (link, target, why) in [
            ("a.html", ".env", "the deny list"),
            (
                "b.html",
                ".git/config",
                "the deny list, via a directory segment",
            ),
            ("c.html", "deploy.pem", "the deny list, by suffix"),
            ("d.html", "db.sqlite", "the closed extension table"),
        ] {
            std::os::unix::fs::symlink(target, at(link)).unwrap();
            assert!(
                resolve_servable(root.path(), link).is_none(),
                "{link} -> {target} laundered past {why}"
            );
        }

        // …and the legitimate case still works: a link to a servable sibling resolves to
        // a servable file inside the worktree, which is the honest answer.
        std::os::unix::fs::symlink("real.html", at("alias.html")).unwrap();
        assert!(
            resolve_servable(root.path(), "alias.html").is_some(),
            "a symlink to a servable sibling is still served"
        );
    }

    /// A branch too deep to walk must not cost the rest of the worktree.
    ///
    /// The bug this pins shipped in the first version: the depth bound shared a `break`
    /// with the two budget bounds, so popping one over-deep directory abandoned every
    /// entry still on the stack — every sibling directory not yet visited.
    ///
    /// **Why the many siblings.** Only unexplored *stack* entries are lost, and whether
    /// the deep branch is popped before them depends on `read_dir` order, which no test
    /// can pin. With thirty siblings the buggy walk drops some of them unless the deep
    /// branch happens to be popped last — so this is deterministic-green on correct code
    /// (the property holds for every order) and catches the regression with probability
    /// ~30/31. Verified by re-introducing the `break` and watching it fail.
    #[test]
    fn one_branch_being_too_deep_does_not_truncate_the_walk() {
        let root = tempfile::TempDir::new().unwrap();
        for i in 0..30 {
            let dir = root.path().join(format!("s{i:02}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("page.html"), "<h1>hi</h1>").unwrap();
        }
        // One chain far past the bound, whose pop must skip that branch and nothing else.
        let mut deep = root.path().join("deep");
        for i in 0..(MAX_SCAN_DEPTH + 4) {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("buried.html"), "<h1>deep</h1>").unwrap();

        let policy = veld_core::files::ViewPolicy {
            web_pages: true,
            ..Default::default()
        };
        let found = scan(root.path(), &policy);
        let names: Vec<&str> = found.iter().map(|(rel, _)| rel.as_str()).collect();
        assert_eq!(
            names.iter().filter(|n| n.ends_with("page.html")).count(),
            30,
            "the deep branch swallowed siblings: {names:?}"
        );
        // The buried one is past the bound, so it is legitimately absent — the branch is
        // skipped, not walked.
        assert!(
            !names.iter().any(|n| n.ends_with("buried.html")),
            "a file past the depth bound must not be returned: {names:?}"
        );
    }

    /// The file origin must never grow a CORS header.
    ///
    /// The whole reason agent-authored HTML is tolerable here is that a page served on
    /// this origin cannot *read* anything from the management API: there is no CORS
    /// layer anywhere in the daemon, so a cross-origin `fetch` gets an opaque response
    /// and the header-presence CSRF check holds. That is an argument about an absence,
    /// and an absence is what nobody notices removing.
    ///
    /// The natural wrong fix — a contributor sees a cross-origin console error and adds
    /// `CorsLayer::permissive()` — would pass clippy, fmt and every other test in this
    /// crate. This is the one that fails.
    #[tokio::test]
    async fn the_file_origin_serves_no_cors_header() {
        use tower::ServiceExt;
        // A 404 is fine: the header would be added by a layer, so it is present or
        // absent on every response regardless of the route's own answer. Using a
        // request that needs no database keeps this a test about the layer stack.
        let res = origin_routes()
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/{}/deck.html", "a".repeat(32)))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        for header in [
            "access-control-allow-origin",
            "access-control-allow-credentials",
            "access-control-allow-headers",
        ] {
            assert!(
                res.headers().get(header).is_none(),
                "{header} would let a served page read the management API"
            );
        }
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
