//! Running the commands `ide.extensions` declares.
//!
//! Two request paths, and the difference between them is the whole security story
//! of this module:
//!
//! - **`status`** evaluates a worktree's badge commands. It is the first thing veld
//!   runs from a repo's config with *no* user action — everything else needs
//!   `veld start` or a click — so execution is bounded and observable rather than
//!   gated by a consent prompt. See [`docs/extensions-vision.md`] for why a prompt
//!   was rejected (it must re-prompt on every `git pull` that touches `veld.json`,
//!   which manufactures the click-through reflex that makes prompts worthless).
//! - **`activate`** runs an `action` on a click, which is the same trust as a pane.
//!
//! Three rules hold across both:
//!
//! - **The client sends a name, never a command.** The id is resolved against the
//!   project's on-disk config here, exactly as `run_action` and `resolve_pane` do.
//!   That extends one step further for badges: a status command's *output* may name
//!   an action to offer, and it too is resolved against the config, so a runtime
//!   value can only ever choose among declared commands.
//! - **Nothing waits forever.** `stdin` is closed and no tty is attached, so a CLI
//!   that would prompt for credentials fails instead of hanging; a deadline backs
//!   that up, and it kills the **process group** so a shell's children go with it.
//! - **The cost bound belongs to veld.** A minimum interval, a count cap
//!   (`veld_core::ide`), an output byte cap, and single-flight per extension so
//!   three IDE windows asking at once spend one child process, not three.

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use veld_core::ide::{Extension, ExtensionBody, IdeSection, OpenIn, PaneIcon};

use crate::feedback_server::desktop::{ApiError, db_err, err, open_desktop_db};
use crate::feedback_server::pty::{missing_pane_binaries, worktree_builtins};

/// How long a status command may run before its process group is killed.
///
/// Generous because the flagship case is a network call to a code host on a
/// developer's laptop wifi, and a badge that times out at 2s on a slow connection
/// reads as broken. Short enough that a wedged CLI does not hold a request open
/// long enough for the UI to look hung.
const STATUS_TIMEOUT: Duration = Duration::from_secs(20);

/// How long an action is watched for an immediate failure before the request
/// returns.
///
/// An action normally launches something that keeps running (an editor), so
/// "finished successfully" is not the success signal — *still running* is. But a
/// missing tool or a bad argument fails in milliseconds, and reporting that is
/// worth a short wait: without it every mistake in an `argv` is a button that
/// silently does nothing.
const ACTIVATE_GRACE: Duration = Duration::from_secs(3);

/// Bytes of stdout/stderr kept per run. Beyond this the child is still killed at
/// the deadline, but nothing unbounded is buffered — a badge that accidentally
/// `cat`s a log file must not grow the daemon's memory.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Characters of `text` a badge may render, and of a tooltip.
///
/// A slot in a 42px bar, not a log viewer. Truncated rather than refused: the
/// author's intent is legible and a dropped badge teaches less than a clipped one.
const MAX_TEXT_CHARS: usize = 60;
const MAX_TOOLTIP_CHARS: usize = 400;

/// The shortest gap between two runs of one extension when the **user** asked.
///
/// A click is the user saying "now", so the declared `refresh_seconds` is not the
/// bound that matters — but something has to be, or holding the mouse down on
/// Refresh spawns a child per event. Short enough to feel immediate, long enough
/// that click-spam cannot fork a process per click.
const FORCED_REFRESH_FLOOR: Duration = Duration::from_secs(3);

/// Live entries in [`RESULTS`] before the oldest are dropped.
///
/// Bounded because the key space is (worktree × extension) and a long-lived daemon
/// sees every worktree the user ever opens. Eviction only costs a re-run.
const MAX_TRACKED: usize = 256;

/// The last result of one extension in one worktree, with when it was produced.
///
/// **This is a rate-limit memory, not a cache.** It answers one question — "did we
/// just run this?" — so that three IDE windows polling the same worktree spend one
/// child process between them instead of three. It is deliberately not a TTL cache
/// serving stale values on a miss: the mutex is what makes the second caller *wait
/// for and share* the first caller's run rather than starting a parallel one, which
/// is the stampede a TTL cache produces at the moment it expires.
type Cell = Arc<tokio::sync::Mutex<Option<(Instant, StatusView)>>>;

static RESULTS: LazyLock<Mutex<HashMap<(i64, String), Cell>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Forget every remembered value for a worktree, so the next request re-runs.
///
/// **An action is a state change, not a repeated question.** Running one usually
/// changes what a badge would say — creating the pull request a badge just
/// reported missing is the flagship case — so the memory of the previous run has
/// to be dropped rather than rate-limited against. Without this, an action
/// followed immediately by a refresh is answered from the run made *before* the
/// action, and the badge shows a spinner and then the old value: it looks like
/// the refresh did nothing, intermittently, depending on whether
/// [`FORCED_REFRESH_FLOOR`] had elapsed.
///
/// The cells are **cleared in place rather than removed** from the map, so the
/// `Arc` every concurrent caller is already waiting on stays the one they get.
/// Dropping the entry instead would let the next request mint a fresh cell and
/// start a second child while the first was still running, which is the
/// single-flight property this module exists to hold. Awaiting each lock is what
/// makes it ordered: an in-flight run finishes, its value is discarded, and the
/// next caller re-runs.
async fn invalidate(worktree: i64) {
    let cells: Vec<Cell> = {
        let map = RESULTS.lock().expect("extension results poisoned");
        map.iter()
            .filter(|((wt, _), _)| *wt == worktree)
            .map(|(_, cell)| Arc::clone(cell))
            .collect()
    };
    for cell in cells {
        *cell.lock().await = None;
    }
}

fn cell(worktree: i64, id: &str) -> Cell {
    let mut map = RESULTS.lock().expect("extension results poisoned");
    if map.len() > MAX_TRACKED {
        // Cheap and correct: a dropped entry costs one extra run. Anything
        // smarter (LRU bookkeeping) buys nothing at this size.
        map.clear();
    }
    Arc::clone(map.entry((worktree, id.to_owned())).or_default())
}

/// What a status extension produced, as the UI consumes it.
#[derive(Clone, Serialize)]
pub(crate) struct StatusView {
    id: String,
    /// `ok`, `empty`, `failed`, `timeout`, or `unavailable`.
    ///
    /// `empty` is a first-class answer, not a failure: a command that exits 0 with
    /// no output is saying "nothing to show here", which is how one config serves
    /// worktrees where the badge does not apply.
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// A glyph the *output* asked for, from the same allowlist `veld.json` uses.
    /// The declaration's own `icon` is on the worktree listing; this is how a
    /// badge changes its glyph with its state (a merge mark once merged).
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<PaneIcon>,
    tone: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tooltip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    href: Option<String>,
    /// Where [`Self::href`] should open. Decided here from the declaration and the
    /// output, so the client renders a link target rather than a policy.
    open_in: &'static str,
    /// Ids of declared `action` extensions this value offers, already resolved
    /// against the config — the client may only ever activate one of these.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actions: Vec<StatusActionView>,
    /// Seconds until the client should ask again.
    refresh_seconds: u64,
    /// How old this value is. Non-zero when the request was answered from a run
    /// another window had just made, which the UI shows rather than hiding.
    age_seconds: u64,
}

#[derive(Clone, Debug, Serialize)]
struct StatusActionView {
    id: String,
    label: String,
}

#[derive(Serialize)]
pub(crate) struct StatusResponse {
    items: Vec<StatusView>,
}

/// Query for the status endpoint.
#[derive(Deserialize, Default)]
pub(crate) struct StatusQuery {
    /// The user asked, so ignore the declared interval and re-run — bounded by
    /// [`FORCED_REFRESH_FLOOR`] rather than unbounded. Absent means the ordinary
    /// poll, which honours `refresh_seconds`.
    #[serde(default)]
    force: bool,
    /// Force just this one extension. Absent with `force` set means all of them.
    id: Option<String>,
}

/// Evaluate every `status` extension a worktree declares.
///
/// One request for the whole worktree rather than one per badge: the client asks
/// about the worktree it is showing, and batching means a switch costs one round
/// trip. The commands run concurrently.
pub(crate) async fn status(
    Path(id): Path<i64>,
    axum::extract::Query(query): axum::extract::Query<StatusQuery>,
) -> Result<Json<StatusResponse>, ApiError> {
    let (root, branch, auto_refresh) = worktree_target(id)?;
    // The machine-global off switch. Answered with an empty list rather than an
    // error: the declarations still travel on the worktree listing, so the UI can
    // say the badges are switched off instead of showing a broken one.
    if !auto_refresh {
        return Ok(Json(StatusResponse { items: Vec::new() }));
    }
    let Some((config, section)) = load_section(&root) else {
        return Ok(Json(StatusResponse { items: Vec::new() }));
    };

    let ctx = worktree_builtins(FsPath::new(&root), &branch, &config);
    let section = Arc::new(section);
    let mut runs = Vec::new();
    for ext in section.extensions.iter().filter(|e| e.slot.is_some()) {
        let ExtensionBody::Status(_) = &ext.body else {
            continue;
        };
        // `force` with no id means every badge; with one, only that badge is
        // re-run and the rest answer from what they already had.
        let forced = query.force && query.id.as_ref().is_none_or(|want| *want == ext.id);
        runs.push(evaluate(
            id,
            ext.clone(),
            Arc::clone(&section),
            root.clone(),
            ctx.clone(),
            forced,
        ));
    }
    let items = futures_util::future::join_all(runs).await;
    Ok(Json(StatusResponse { items }))
}

/// One badge, single-flighted and rate-limited by its own `refresh_seconds`.
async fn evaluate(
    worktree: i64,
    ext: Extension,
    section: Arc<IdeSection>,
    root: String,
    builtins: HashMap<String, String>,
    forced: bool,
) -> StatusView {
    let ExtensionBody::Status(status) = &ext.body else {
        unreachable!("callers filter to status extensions");
    };
    let cell = cell(worktree, &ext.id);
    // Held across the run on purpose: a second window arriving mid-run waits here
    // and then finds the fresh value below, instead of starting a parallel `gh`.
    let mut guard = cell.lock().await;
    if let Some((at, view)) = guard.as_ref() {
        let age = at.elapsed();
        let floor = if forced {
            FORCED_REFRESH_FLOOR
        } else {
            Duration::from_secs(status.refresh_seconds)
        };
        if age < floor {
            let mut view = view.clone();
            view.age_seconds = age.as_secs();
            return view;
        }
    }

    let missing = missing_pane_binaries(&ext.requires_bin);
    let view = if missing.is_empty() {
        run_status(&ext, status, &section, &root, &builtins).await
    } else {
        // Not an error state: the UI already knows the extension is unavailable
        // from the worktree listing and renders the hint. Saying so here keeps the
        // two surfaces from disagreeing while a tool is being installed.
        StatusView {
            id: ext.id.clone(),
            state: "unavailable",
            text: None,
            icon: None,
            tone: "neutral",
            tooltip: Some(format!("needs {}", missing.join(", "))),
            href: None,
            open_in: open_in_str(status.open_in),
            actions: Vec::new(),
            refresh_seconds: status.refresh_seconds,
            age_seconds: 0,
        }
    };
    *guard = Some((Instant::now(), view.clone()));
    view
}

async fn run_status(
    ext: &Extension,
    status: &veld_core::ide::StatusExtension,
    section: &IdeSection,
    root: &str,
    builtins: &HashMap<String, String>,
) -> StatusView {
    let base = |state: &'static str, tone: &'static str| StatusView {
        id: ext.id.clone(),
        state,
        text: None,
        icon: None,
        tone,
        tooltip: None,
        href: None,
        open_in: open_in_str(status.open_in),
        actions: Vec::new(),
        refresh_seconds: status.refresh_seconds,
        age_seconds: 0,
    };

    let outcome = spawn_command(&status.command, root, builtins, STATUS_TIMEOUT).await;
    let out = match outcome {
        Err(message) => {
            return StatusView {
                tooltip: Some(message),
                ..base("failed", "danger")
            };
        }
        Ok(out) => out,
    };
    if out.timed_out {
        return StatusView {
            text: Some(ext.label.clone()),
            tooltip: Some(format!(
                "timed out after {}s{}",
                STATUS_TIMEOUT.as_secs(),
                tail_suffix(&out.stderr)
            )),
            ..base("timeout", "warning")
        };
    }
    if !out.success {
        return StatusView {
            text: Some(ext.label.clone()),
            tooltip: Some(format!(
                "exited with status {}{}",
                out.code
                    .map_or_else(|| "unknown".to_owned(), |c| c.to_string()),
                tail_suffix(&out.stderr)
            )),
            ..base("failed", "danger")
        };
    }

    let stdout = out.stdout.trim();
    if stdout.is_empty() {
        // "Nothing to show" — the badge is simply absent. See `StatusView::state`.
        return base("empty", "neutral");
    }
    parse_badge(stdout, ext, section, base("ok", "neutral"))
}

/// Read a badge out of a status command's stdout.
///
/// Tolerant by design, because the tolerance is most of the ergonomics: a command
/// that knows nothing about veld — `git rev-parse --short HEAD` — is a working
/// badge, and only a command that wants tone, a link or an action has to emit the
/// object form.
fn parse_badge(
    stdout: &str,
    ext: &Extension,
    section: &IdeSection,
    base: StatusView,
) -> StatusView {
    let Some(obj) = serde_json::from_str::<Value>(stdout)
        .ok()
        .and_then(|v| v.as_object().cloned())
    else {
        return StatusView {
            text: Some(clip(first_line(stdout), MAX_TEXT_CHARS)),
            ..base
        };
    };

    let text = obj
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| clip(t, MAX_TEXT_CHARS));
    // An object with no `text` says nothing renderable. Falling back to the
    // declared label keeps the badge visible and clickable, which is what an
    // author emitting only `{ "href": … }` clearly meant.
    let text = text.or_else(|| Some(clip(&ext.label, MAX_TEXT_CHARS)));

    let tone = match obj.get("tone").and_then(Value::as_str).map(str::trim) {
        Some("info") => "info",
        Some("success") => "success",
        Some("warning") => "warning",
        Some("danger") => "danger",
        // An unknown tone is neutral rather than an error: the badge's job is to
        // render, and a typo in a colour must not take the information with it.
        _ => "neutral",
    };

    let tooltip = obj
        .get("tooltip")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| clip(t, MAX_TOOLTIP_CHARS));

    // Repo-controlled and handed to the OS on a click, so the same `http(s)`-only
    // rule every URL in `ide` carries. A rejected scheme drops the link and keeps
    // the badge — the alternative is a badge that vanishes because of its href.
    let href = obj
        .get("href")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| veld_core::ide::is_web_url(u))
        .map(str::to_owned);

    let open_in = match obj.get("open_in").and_then(Value::as_str).map(str::trim) {
        Some("system") => "system",
        Some("pane") => "pane",
        _ => base.open_in,
    };

    let icon = obj
        .get("icon")
        .and_then(Value::as_str)
        .and_then(veld_core::ide::parse_icon_name);

    let actions = obj
        .get("actions")
        .and_then(Value::as_array)
        .map(|items| resolve_actions(items, section))
        .unwrap_or_default();

    StatusView {
        text,
        icon,
        tone,
        tooltip,
        href,
        open_in,
        actions,
        ..base
    }
}

/// Turn the ids a badge offered into actions the client may activate.
///
/// **The resolution is the security boundary.** An entry names a declared `action`
/// extension or it is dropped; nothing here can introduce a command, so a status
/// command's output can only ever choose among what the config already declares.
fn resolve_actions(items: &[Value], section: &IdeSection) -> Vec<StatusActionView> {
    let mut out = Vec::new();
    for item in items.iter().take(8) {
        // Accept a bare string as well as `{ id, label }`: the short form is what
        // a one-liner adapter writes, and rejecting it would push every author
        // through the object form for no gain.
        let (id, label) = match item {
            Value::String(id) => (id.trim(), None),
            Value::Object(map) => (
                map.get("id").and_then(Value::as_str).unwrap_or("").trim(),
                map.get("label")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|l| !l.is_empty()),
            ),
            _ => continue,
        };
        let Some(ext) = section.extension(id) else {
            continue;
        };
        if !matches!(ext.body, ExtensionBody::Action(_)) {
            continue;
        }
        if out.iter().any(|a: &StatusActionView| a.id == id) {
            continue;
        }
        out.push(StatusActionView {
            id: id.to_owned(),
            label: clip(label.unwrap_or(&ext.label), MAX_TEXT_CHARS),
        });
    }
    out
}

#[derive(Deserialize)]
pub(crate) struct ActivateBody {
    /// The extension to run. A name — see the module docs.
    id: String,
}

#[derive(Serialize)]
pub(crate) struct ActivateResponse {
    /// `started` when the command is still running after the grace window, which is
    /// success for something that launches an app; `finished` when it exited 0.
    state: &'static str,
}

/// Run an `action` extension the user clicked.
pub(crate) async fn activate(
    Path(id): Path<i64>,
    Json(body): Json<ActivateBody>,
) -> Result<Json<ActivateResponse>, ApiError> {
    let (root, branch, _) = worktree_target(id)?;
    let (config, section) = load_section(&root).ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            "this worktree has no veld config to declare extensions",
        )
    })?;

    let ext = section.extension(&body.id).ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            format!("this project declares no extension called {:?}", body.id),
        )
    })?;
    // A badge is not clickable-as-a-command and a menu runs nothing, so naming
    // either here is a client bug rather than something to guess at.
    let ExtensionBody::Action(action) = &ext.body else {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "extension {:?} is a {} extension, which has nothing to run",
                body.id,
                ext.kind()
            ),
        ));
    };
    let missing = missing_pane_binaries(&ext.requires_bin);
    if !missing.is_empty() {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("not installed: {}", missing.join(", ")),
        ));
    }

    let builtins = worktree_builtins(FsPath::new(&root), &branch, &config);
    let out = spawn_command(&action.command, &root, &builtins, ACTIVATE_GRACE)
        .await
        .map_err(|message| err(StatusCode::UNPROCESSABLE_ENTITY, message))?;
    if !out.success && !out.timed_out {
        // A failed action changed nothing worth re-reading, so the remembered
        // values stay — the badge keeps saying what it last truthfully said.
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "exited with status {}{}",
                out.code
                    .map_or_else(|| "unknown".to_owned(), |c| c.to_string()),
                tail_suffix(&out.stderr)
            ),
        ));
    }
    // Anything that ran — finished, or still running past the grace window —
    // may have changed what a badge reports.
    invalidate(id).await;
    // Still running at the deadline is the *expected* outcome for an editor
    // launcher, so the grace window's timeout is success here — unlike a status
    // run, where it means the badge never produced a value.
    Ok(Json(ActivateResponse {
        state: if out.timed_out { "started" } else { "finished" },
    }))
}

struct Output {
    stdout: String,
    stderr: String,
    success: bool,
    code: Option<i32>,
    timed_out: bool,
}

/// Spawn one extension command in a worktree and collect a bounded amount of its
/// output.
///
/// Every hazard this module documents is handled here, in one place:
///
/// - the user's login-shell `PATH` is injected from the warm cache, never resolved
///   inline (AGENTS.md: resolution spawns a login shell and can take 10s);
/// - `stdin` is null, so a CLI that would prompt for credentials fails fast
///   instead of blocking on a pipe nobody writes to;
/// - `NO_COLOR`/`TERM` stop a tool colouring output it thinks is a terminal, which
///   would put escape sequences inside the badge contract;
/// - the child gets its **own process group**, so the deadline can kill a `shell`
///   command's whole tree rather than just the shell, and the kill cannot reach the
///   daemon's own group;
/// - output is capped.
async fn spawn_command(
    spec: &veld_core::config::CommandSpec,
    root: &str,
    builtins: &HashMap<String, String>,
    timeout: Duration,
) -> Result<Output, String> {
    let ctx = veld_core::variables::VariableContext {
        builtins: builtins.clone(),
        ..Default::default()
    };
    let spec = spec
        .interpolate(&ctx)
        .map_err(|e| format!("could not resolve the command: {e}"))?;

    let mut cmd = match &spec {
        veld_core::config::CommandSpec::Argv(argv) => {
            let (program, args) = argv
                .split_first()
                .ok_or_else(|| "the command runs nothing".to_owned())?;
            let mut c = tokio::process::Command::new(program);
            c.args(args);
            c
        }
        veld_core::config::CommandSpec::Shell(script) => {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(script);
            c
        }
    };

    // Logged before the spawn, with the full argv, because "what did veld run"
    // must be answerable for a command the user never clicked.
    tracing::info!(worktree = %root, command = %spec.display(), "running ide extension command");

    cmd.current_dir(root)
        .env("PATH", veld_core::user_path::cached_user_path().await)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);

    let child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", spec.display()))?;
    let pid = child.id().map(|p| p as i32);

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => {
            let out = result.map_err(|e| format!("could not read the command's output: {e}"))?;
            Ok(Output {
                stdout: cap(&out.stdout),
                stderr: cap(&out.stderr),
                success: out.status.success(),
                code: out.status.code(),
                timed_out: false,
            })
        }
        Err(_) => {
            // The group, not the pid: a `shell` command's `sh` may have forked the
            // thing that is actually stuck, and killing only the shell leaves it
            // holding the pipe.
            if let Some(pid) = pid {
                // SAFETY: `killpg` on a pid that led its own group (set by
                // `process_group(0)` above) either signals that group or fails
                // with ESRCH because it already exited. It cannot reach the
                // daemon's own group, which is what the spawn-time call bought.
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
            Ok(Output {
                stdout: String::new(),
                stderr: String::new(),
                success: false,
                code: None,
                timed_out: true,
            })
        }
    }
}

/// Resolve a worktree id to its path and branch, plus the machine's auto-refresh
/// switch.
///
/// One database open for all three: the switch is read from the handle that is
/// already being opened to resolve the worktree, because putting a second
/// `Db::open()` on a request path is a design decision and not a detail
/// (AGENTS.md).
fn worktree_target(id: i64) -> Result<(String, String, bool), ApiError> {
    let db = open_desktop_db()?;
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
    let auto_refresh = db.extensions_auto_refresh();
    Ok((wt.path, wt.branch, auto_refresh))
}

/// The project's config and its parsed `ide` section, or `None` when the worktree
/// has no config or it does not load.
///
/// A worktree without a loadable config simply declares no extensions — never an
/// error, for the reason `parse_config` carries no semantic checks: a config that
/// cannot load must not take unrelated surfaces down with it.
fn load_section(root: &str) -> Option<(veld_core::config::VeldConfig, IdeSection)> {
    let path = veld_core::config::root_config_in(FsPath::new(root))?;
    let config = veld_core::config::parse_config(&path).ok()?;
    let section = config.ide_section();
    Some((config, section))
}

fn open_in_str(open_in: OpenIn) -> &'static str {
    match open_in {
        OpenIn::System => "system",
        OpenIn::Pane => "pane",
    }
}

fn cap(bytes: &[u8]) -> String {
    let end = bytes.len().min(MAX_OUTPUT_BYTES);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// Clip to `max` **characters**, never bytes — a byte slice would panic on a
/// multi-byte boundary, and emoji in a badge are entirely expected.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// The last line of stderr, as a suffix for an error message, or nothing.
fn tail_suffix(stderr: &str) -> String {
    let tail = stderr
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if tail.is_empty() {
        String::new()
    } else {
        format!(": {}", clip(tail, MAX_TOOLTIP_CHARS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project declaring one badge and one action it can offer.
    fn section() -> IdeSection {
        veld_core::ide::parse(Some(&serde_json::json!({
            "extensions": [
                { "id": "pr", "slot": "topBar", "type": "status", "label": "PR",
                  "argv": ["true"] },
                { "id": "create-pr", "type": "action", "label": "Create a PR",
                  "argv": ["true"] },
                { "id": "menu", "slot": "topBar", "type": "menu",
                  "items": ["create-pr"] },
            ]
        })))
    }

    fn badge(stdout: &str) -> StatusView {
        let section = section();
        let ext = section.extension("pr").expect("declared").clone();
        let ExtensionBody::Status(status) = &ext.body else {
            panic!("pr is a status extension");
        };
        let base = StatusView {
            id: ext.id.clone(),
            state: "ok",
            text: None,
            icon: None,
            tone: "neutral",
            tooltip: None,
            href: None,
            open_in: open_in_str(status.open_in),
            actions: Vec::new(),
            refresh_seconds: status.refresh_seconds,
            age_seconds: 0,
        };
        parse_badge(stdout, &ext, &section, base)
    }

    /// Running an action forgets the worktree's remembered values.
    ///
    /// The regression this pins was reported as "clicking sometimes loads but the
    /// colour does not change": an action changed state, the client forced a
    /// refresh, and `FORCED_REFRESH_FLOOR` answered it from the run made *before*
    /// the action — so it depended on how fast you clicked. Asserted at the cache
    /// level because the handler needs a database and a real child process, and the
    /// defect was never in either.
    #[tokio::test]
    async fn an_action_forgets_what_the_badges_last_said() {
        let mine = 4242;
        let other = 4343;
        let view = |id: &str| StatusView {
            id: id.to_owned(),
            state: "ok",
            text: Some("before".to_owned()),
            icon: None,
            tone: "neutral",
            tooltip: None,
            href: None,
            open_in: "system",
            actions: Vec::new(),
            refresh_seconds: 60,
            age_seconds: 0,
        };
        for (wt, id) in [(mine, "pr"), (mine, "tone"), (other, "pr")] {
            let cell = cell(wt, id);
            *cell.lock().await = Some((Instant::now(), view(id)));
        }

        invalidate(mine).await;

        for id in ["pr", "tone"] {
            assert!(
                cell(mine, id).lock().await.is_none(),
                "{id} must be re-run after an action, not answered from before it"
            );
        }
        // Scoped to the worktree: another checkout's badges were not affected by
        // an action taken over here.
        assert!(
            cell(other, "pr").lock().await.is_some(),
            "another worktree's values must survive"
        );
        // Leave the shared static as it was found — these tests run in one process.
        RESULTS
            .lock()
            .expect("extension results poisoned")
            .retain(|(wt, _), _| *wt != mine && *wt != other);
    }

    #[test]
    fn output_that_is_not_the_contract_becomes_the_text() {
        // The tolerance that makes `git rev-parse --short HEAD` a working badge
        // with no adapter at all. First line only — a command that prints a page
        // must not paste it into a 42px bar.
        let view = badge("a1b2c3d\nand some more\n");
        assert_eq!(view.text.as_deref(), Some("a1b2c3d"));
        assert_eq!(view.tone, "neutral");
        assert!(view.href.is_none());
    }

    #[test]
    fn the_contract_is_read_when_it_is_there() {
        let view = badge(
            r#"{"text":"PR #7 · merged","tone":"success","tooltip":"t",
                "href":"https://example.com/7","open_in":"pane","icon":"git-merge"}"#,
        );
        assert_eq!(view.text.as_deref(), Some("PR #7 · merged"));
        assert_eq!(view.tone, "success");
        assert_eq!(view.tooltip.as_deref(), Some("t"));
        assert_eq!(view.href.as_deref(), Some("https://example.com/7"));
        assert_eq!(
            view.open_in, "pane",
            "the output may override the declaration"
        );
        assert_eq!(
            view.icon,
            Some(veld_core::ide::PaneIcon::Name("git-merge".to_owned()))
        );
    }

    #[test]
    fn an_unknown_tone_or_icon_is_dropped_rather_than_taking_the_badge_with_it() {
        let view = badge(r#"{"text":"x","tone":"chartreuse","icon":"nonesuch"}"#);
        assert_eq!(view.tone, "neutral", "a typo in a colour is not an error");
        assert!(view.icon.is_none());
        assert_eq!(view.text.as_deref(), Some("x"), "the information survives");
    }

    #[test]
    fn a_non_web_href_is_dropped_and_the_badge_stays() {
        // Same rule `ide.quicklinks` carries: a click hands this to the OS, so a
        // custom scheme would make a badge a launcher for whatever is registered.
        // The badge is kept, because vanishing over its own link is worse.
        for href in ["vscode://open", "file:///etc/passwd", "javascript:alert(1)"] {
            let view = badge(&format!(r#"{{"text":"x","href":"{href}"}}"#));
            assert!(view.href.is_none(), "{href} must not survive");
            assert_eq!(view.text.as_deref(), Some("x"));
        }
    }

    #[test]
    fn an_object_with_no_text_falls_back_to_the_declared_label() {
        // An author emitting only `{ "href": … }` clearly meant "keep my label,
        // make it clickable".
        let view = badge(r#"{"href":"https://example.com"}"#);
        assert_eq!(view.text.as_deref(), Some("PR"));
    }

    #[test]
    fn only_declared_actions_survive_and_only_actions() {
        // **The security boundary of the output contract.** A runtime value may
        // choose which declared action is offered; it may never introduce one, and
        // it may not reach a `menu` or a `status` either.
        let view = badge(
            r#"{"text":"x","actions":[
                 "create-pr",
                 {"id":"create-pr","label":"again"},
                 {"id":"ghost"},
                 {"id":"menu"},
                 {"id":"pr"},
                 {"argv":["rm","-rf","/"]}
               ]}"#,
        );
        assert_eq!(view.actions.len(), 1, "{:?}", view.actions);
        assert_eq!(view.actions[0].id, "create-pr");
        assert_eq!(
            view.actions[0].label, "Create a PR",
            "the bare-string form takes the declaration's label"
        );
    }

    #[test]
    fn an_action_label_from_the_output_overrides_presentation_only() {
        let view = badge(r#"{"text":"x","actions":[{"id":"create-pr","label":"Open one"}]}"#);
        assert_eq!(view.actions[0].id, "create-pr");
        assert_eq!(view.actions[0].label, "Open one");
    }

    #[test]
    fn a_badge_cannot_render_more_than_the_bar_can_hold() {
        let long = "x".repeat(500);
        let view = badge(&format!(r#"{{"text":"{long}"}}"#));
        let text = view.text.expect("text");
        assert_eq!(text.chars().count(), MAX_TEXT_CHARS);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn clipping_counts_characters_not_bytes() {
        // A byte slice would panic on a multi-byte boundary, and emoji in a badge
        // are entirely expected.
        let view = badge(&format!(r#"{{"text":"{}"}}"#, "🦊".repeat(200)));
        let text = view.text.expect("text");
        assert_eq!(text.chars().count(), MAX_TEXT_CHARS);
    }
}
