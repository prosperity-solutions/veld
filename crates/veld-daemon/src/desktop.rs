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
use tracing::warn;
use veld_core::db::{Db, DiscoveredWorktree, RepoRecord, WorktreeRecord, default_alias};
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
        .route("/api/repos/import", post(import_repo))
        .route("/api/worktrees", post(create_worktree))
        .route(
            "/api/worktrees/{id}",
            patch(patch_worktree).delete(delete_worktree),
        )
        .route("/api/worktrees/{id}/start", post(start_worktree_run))
        .route("/api/worktrees/{id}/restore", post(restore_worktree))
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
        .route("/api/worktree-emoji", get(worktree_emoji))
        .route("/api/lanes", get(list_lanes).post(create_lane))
        .route("/api/lane-order", post(reorder_lanes))
        .route("/api/lanes/{name}", patch(rename_lane).delete(delete_lane))
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

/// Open the OS folder picker and return the chosen absolute path. The daemon
/// runs in the user's GUI session (it already opens Terminal.app), so it can
/// host the dialog for the browser build too — the web platform itself never
/// exposes absolute paths. Responses: 200 `{path}`, 204 on cancel, 409 while
/// another pick is already open, 408 after the 10-minute timeout, 501 when no
/// picker backend exists, 500 when the backend fails (no GUI session, macOS
/// permission denial).
async fn pick_directory() -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;

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

fn open_desktop_db() -> Result<Db, ApiError> {
    open_db().map_err(|code| err(code, "failed to open the veld database"))
}

// ---------------------------------------------------------------------------
// Git plumbing
// ---------------------------------------------------------------------------

/// Run `git -C <dir> <args…>` with the user's login-shell PATH. Returns
/// trimmed stdout, or the trimmed stderr as the error message.
pub(super) async fn git(dir: &FsPath, args: &[&str]) -> Result<String, String> {
    let path_env = cached_user_path().await;
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
/// is unreachable, `git worktree list` fails, `sync_repo_worktrees` returns `Err`,
/// and `RepoView.available` goes false with every row left untouched.
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
/// - **U+2028/U+2029 are real line breaks**, which is precisely what the rule
///   below says it rejects. `white-space: nowrap` suppresses soft wraps, not
///   forced ones, so `"prod\u{2028}rm -rf"` renders in the rail as `prod`.
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
    available: bool,
    worktrees: Vec<WorktreeView>,
    /// The repo's rail lanes, in their own order.
    ///
    /// Travels with the repo rather than on its own poll because the rail cannot
    /// render a group header without both halves, and fetching them separately
    /// means a frame where a worktree's `lane` names a lane the client has not
    /// heard of yet.
    lanes: Vec<veld_core::db::LaneRecord>,
}

#[derive(Serialize)]
struct WorktreeView {
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
    let ide = cfg
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
                    }
                })
                .collect();
            IdeView {
                quicklinks: section.quicklinks,
                permissions: section.permissions,
                panes,
            }
        })
        .unwrap_or_default();
    // `wt` is moved into the view below, so the guard read happens here — before
    // the move — rather than in the literal, where `wt.id` would not resolve.
    let deleting = super::worktree_trash::now_deleting(wt.id);
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
        ide,
    }
}

async fn repo_view(db: &Db, repo: RepoRecord, available: bool) -> Result<RepoView, ApiError> {
    let worktrees = db
        .list_worktrees(FsPath::new(&repo.root))
        .map_err(db_err)?
        .into_iter()
        .map(worktree_view)
        .collect();
    let lanes = db.list_lanes(FsPath::new(&repo.root)).map_err(db_err)?;
    Ok(RepoView {
        repo,
        available,
        worktrees,
        lanes,
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
        LAST_SYNC.record(availability);
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
    /// Custom checkout path; defaults to `<repo parent>/_worktrees/<alias>`.
    #[serde(default)]
    path: Option<String>,
    /// Marker glyph chosen in the create dialog; the daemon assigns one when absent.
    #[serde(default)]
    emoji: Option<String>,
    /// Marker colour chosen in the create dialog; assigned when absent.
    #[serde(default)]
    marker_color: Option<String>,
}

async fn create_worktree(
    Json(body): Json<CreateWorktreeBody>,
) -> Result<Json<WorktreeView>, ApiError> {
    validate_branch(&body.branch)?;
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
    Ok(Json(worktree_view(wt)))
}

#[derive(Deserialize)]
struct DeleteQuery {
    /// Remove the checkout even with modified or untracked files
    /// (`git worktree remove --force`).
    ///
    /// Deliberately **not** persisted with the trash state. If the daemon dies
    /// mid-removal, recovery retries without it — a crash must not silently
    /// upgrade a removal to one that discards uncommitted work, and forcing is a
    /// decision worth re-taking rather than inheriting.
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
    // return a 500 having already set `trashed_at`, leaving the row in "Pending
    // removal" with nothing queued to act on it until the next daemon restart —
    // every other exit from this function releases the row.
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
    Ok(Json(worktree_view(wt)))
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
    if code == StatusCode::ACCEPTED {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(err(code, "failed to spawn veld start"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ("POST", "/api/worktrees/1/restore", ""),
                ("POST", "/api/worktrees/1/delete", ""),
                ("DELETE", "/api/trash?repo_root=/tmp", ""),
                ("DELETE", "/api/worktrees/1/trash-error", ""),
                (
                    "POST",
                    "/api/worktree-order",
                    r#"{"repo_root":"/tmp","order":[]}"#,
                ),
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
