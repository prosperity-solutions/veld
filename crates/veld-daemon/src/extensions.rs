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

/// Bytes of stdout/stderr read per run, **enforced while reading**.
///
/// This has to be a read-time bound, not a truncation afterwards. `wait_with_output`
/// is `read_to_end` into a fresh `Vec` (tokio 1.50, `process/mod.rs`), so capping the
/// result would mean the whole stream reached memory first: a badge that `cat`s a
/// large file — re-run on a timer, unattended — could take the daemon's memory with
/// it, and with the daemon go every terminal and run on the machine. So both pipes
/// are read through a limited reader and the child's group is killed once either
/// side is full.
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

/// Live entries in [`RESULTS`] before idle ones are evicted.
///
/// Bounded because the key space is (worktree × extension) and a long-lived daemon
/// sees every worktree the user ever opens. **Only unlocked cells are evicted**, and
/// that restriction is the whole point: a locked cell is one a run is in flight on,
/// and dropping it would let the next caller mint a fresh cell and start a second
/// child for the same extension — losing exactly the single-flight property this
/// module exists to hold, and putting `invalidate`'s snapshot out of reach of the
/// run it needs to invalidate. Evicting an idle cell costs one re-run and nothing
/// else. Reachable without malice: 24 badges across 11 worktrees.
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

/// Both maps are keyed on the worktree's **path**, never on its database id.
///
/// `worktrees.id` is an `INTEGER PRIMARY KEY` with no `AUTOINCREMENT` and rows are
/// hard-deleted, so **SQLite reuses the id**. These maps live as long as the daemon
/// does, so a reused id means a new checkout's first poll can be answered from a
/// deleted one's value — and because every project copying the documented example
/// names its badge `pr`, that puts one repo's pull request number, link and offered
/// actions in another repo's top bar.
static RESULTS: LazyLock<Mutex<HashMap<(String, String), Cell>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static INVALIDATED: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether a value whose run **started** at `at` is still trustworthy.
///
/// `at` is the run's start, not its finish — see [`evaluate`]. A run already in
/// flight when the action landed started *before* the stamp and is therefore
/// rejected, which is the whole point; stamping completion instead let exactly that
/// run's pre-action value satisfy this gate.
fn invalidated_since(worktree: &str, at: Instant) -> bool {
    INVALIDATED
        .lock()
        .expect("extension invalidations poisoned")
        .get(worktree)
        .is_some_and(|stamp| *stamp >= at)
}

/// When a worktree's remembered values last stopped being trustworthy.
///
/// **An action is a state change, not a repeated question.** Running one usually
/// changes what a badge would say — creating the pull request a badge just
/// reported missing is the flagship case — so the memory of the previous run has to
/// be discarded rather than rate-limited against. Without this, an action followed
/// immediately by a refresh is answered from the run made *before* the action, and
/// the badge shows a spinner and then the old value: it looks like the refresh did
/// nothing, intermittently, depending on whether [`FORCED_REFRESH_FLOOR`] had
/// elapsed.
///
/// **A timestamp, not a walk over the cells.** Clearing the cells would mean taking
/// each of their locks, and [`evaluate`] holds one across a child run bounded by
/// [`STATUS_TIMEOUT`] — so a click on an unrelated button waited out a slow badge
/// (up to ~23s with the grace window), and every control in the project read as
/// broken because one badge was network-bound. Comparing a stored value's `Instant`
/// against this stamp is the same ordering with no waiting: a value produced before
/// the action is stale by definition, including one produced by a run that was
/// already in flight when the action landed.
fn invalidate(worktree: &str) {
    let mut stamps = INVALIDATED
        .lock()
        .expect("extension invalidations poisoned");
    if stamps.len() > MAX_TRACKED {
        // The **oldest** stamp, not the whole map. A stamp is the only thing that
        // makes a post-action refresh really re-run, so clearing them all meant a
        // click landing just as the cap was hit could be answered from before
        // itself — for every worktree at once.
        if let Some(oldest) = stamps
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(path, _)| path.clone())
        {
            stamps.remove(&oldest);
        }
    }
    stamps.insert(worktree.to_owned(), Instant::now());
}

fn cell(worktree: &str, id: &str) -> Cell {
    let mut map = RESULTS.lock().expect("extension results poisoned");
    if map.len() > MAX_TRACKED {
        // `try_lock` is the "nobody is using this" test — see MAX_TRACKED. An
        // `Arc` held only by the map, whose mutex is free, is an entry no caller
        // can be waiting on. Anything smarter (true LRU) buys nothing at this size.
        map.retain(|_, cell| Arc::strong_count(cell) > 1 || cell.try_lock().is_err());
    }
    Arc::clone(map.entry((worktree.to_owned(), id.to_owned())).or_default())
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
    // The machine-global off switch. An empty list rather than an error, because
    // the user turned it off and a control reporting that back at them is noise —
    // the badges simply do not render. The declarations still travel on the
    // worktree listing, so `action` and `menu` entries keep working: a click is the
    // user asking, which is the half this switch does not gate.
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
    ext: Extension,
    section: Arc<IdeSection>,
    root: String,
    builtins: HashMap<String, String>,
    forced: bool,
) -> StatusView {
    let ExtensionBody::Status(status) = &ext.body else {
        unreachable!("callers filter to status extensions");
    };
    let cell = cell(&root, &ext.id);
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
        if age < floor && !invalidated_since(&root, *at) {
            let mut view = view.clone();
            view.age_seconds = age.as_secs();
            return view;
        }
    }

    // **Stamped before the run, not after, and both halves matter.**
    //
    // The freshness gate below measures `refresh_seconds` from this instant while
    // the client's timer measures it from when it *asked*, so stamping completion
    // instead made every declared interval effectively double: the next poll arrives
    // one run-duration early, is answered from memory, and only the one after it
    // re-runs.
    //
    // It is also what makes `invalidated_since` mean what it claims. A run already
    // in flight when an action lands finishes *after* the invalidation stamp, so a
    // completion timestamp would let that run's pre-action value pass the gate —
    // reintroducing the "clicking loads but nothing changes" bug for the specific
    // case where a poll overlaps the click, which is the *correlated* case, since
    // people click the action a badge just offered them.
    let started = Instant::now();
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
    *guard = Some((started, view.clone()));
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

    if out.truncated {
        // Deliberately *not* the tolerant path. A contract object cut mid-JSON
        // fails to parse, and treating that as "not the contract" would render 60
        // characters of raw JSON as the badge's text — which looks like the author's
        // mistake rather than a limit they hit.
        return StatusView {
            text: Some(ext.label.clone()),
            tooltip: Some(format!(
                "printed more than {} KiB, which is more than a badge can carry",
                MAX_OUTPUT_BYTES / 1024
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
    // Invalidate whatever the exit status. A non-zero exit does **not** mean
    // nothing happened: `gh pr create && notify-something` can create the pull
    // request and then fail, and a badge left on its pre-action value for a full
    // interval is the worse answer. The cost of being wrong this way is one extra
    // child run per failed click.
    invalidate(&root);
    if !out.success && !out.timed_out {
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
    // Still running at the deadline is the *expected* outcome for an editor
    // launcher, so the grace window's timeout is success here — unlike a status
    // run, where it means the badge never produced a value.
    Ok(Json(ActivateResponse {
        state: if out.timed_out { "started" } else { "finished" },
    }))
}

struct Output {
    stdout: String,
    /// True when the child wrote more than [`MAX_OUTPUT_BYTES`] and the rest was
    /// discarded. The caller must not parse a truncated payload: a contract object
    /// cut mid-JSON fails to deserialize and would fall through the *tolerant* path,
    /// putting 60 characters of raw JSON in the bar.
    truncated: bool,
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

    // `kill_on_drop` covers the direct child; the guard below covers its whole
    // group. Both are needed — see `GroupKill`.
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", spec.display()))?;
    // Armed immediately, so every exit from this function — the deadline, an I/O
    // error, or the request future being dropped under us — kills the group.
    let mut guard = GroupKill(child.id().map(|p| p as i32));

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let collect = async {
        // Both pipes are drained concurrently with the wait. Draining matters as
        // much as the cap: a child that fills a pipe nobody reads blocks in
        // `write` and then dies at the deadline having done nothing, which reads
        // as "your command is slow" rather than "veld stopped listening".
        let (status, stdout, stderr) = tokio::try_join!(
            child.wait(),
            read_capped(&mut stdout_pipe),
            read_capped(&mut stderr_pipe),
        )?;
        Ok::<_, std::io::Error>((status, stdout, stderr))
    };

    match tokio::time::timeout(timeout, collect).await {
        Ok(result) => {
            let (status, (stdout, truncated), (stderr, _)) =
                result.map_err(|e| format!("could not read the command's output: {e}"))?;
            // It exited on its own, so there is no group left to signal.
            guard.disarm();
            Ok(Output {
                stdout,
                truncated,
                stderr,
                success: status.success(),
                code: status.code(),
                timed_out: false,
            })
        }
        // The guard kills the group on the way out of this scope.
        Err(_) => Ok(Output {
            stdout: String::new(),
            truncated: false,
            stderr: String::new(),
            success: false,
            code: None,
            timed_out: true,
        }),
    }
}

/// Reads a pipe to EOF while **keeping** at most [`MAX_OUTPUT_BYTES`].
///
/// Two requirements pull in opposite directions and both matter.
///
/// The cap has to be applied *while reading*: `wait_with_output` is `read_to_end`
/// into a fresh `Vec`, so truncating its result means the whole stream reached
/// memory first, and a badge that `cat`s a large file — unattended, on a timer —
/// could take the daemon down with it.
///
/// But the read must not simply **stop** at the cap either. A pipe nobody drains
/// fills, the child blocks in `write`, and it is then killed at the deadline: a
/// merely chatty command that would have exited fine turns into a 20-second
/// timeout with no output at all. That is a worse answer for the careless case than
/// truncation, and the careless case is the common one.
///
/// So: drain to EOF, discarding past the cap. Memory is bounded, a well-behaved
/// command still succeeds and gets a truncated badge, and an *endless* writer still
/// hits the deadline — which is the only case where killing it is right.
async fn read_capped<R>(pipe: &mut Option<R>) -> std::io::Result<(String, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let Some(pipe) = pipe.as_mut() else {
        return Ok((String::new(), false));
    };
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let n = pipe.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if kept.len() < MAX_OUTPUT_BYTES {
            let room = MAX_OUTPUT_BYTES - kept.len();
            kept.extend_from_slice(&chunk[..n.min(room)]);
            truncated |= n > room;
        } else {
            truncated = true;
        }
        // Past the cap the bytes are read and dropped: the child keeps making
        // progress, and nothing accumulates.
    }
    Ok((String::from_utf8_lossy(&kept).into_owned(), truncated))
}

/// Kills a child's **process group** unless disarmed.
///
/// A guard rather than a line in the timeout branch, because the timeout branch is
/// not the only way out. Axum drops a handler's future when the client disconnects
/// — a page reload, a closed window, a quit app — and a bare
/// `timeout(.., child.wait())` dropped that way leaves the repo's command running
/// with no deadline and nothing left to signal it. `kill_on_drop` alone is not
/// enough either: it reaps the direct child, so a `shell` command's `sh` dies while
/// whatever it forked keeps the pipe and the CPU.
///
/// It also removes a subtler dependency: the previous version's `killpg` was sound
/// only because the `Child` happened to still be alive as the match scrutinee, so an
/// ordinary refactor could have made it signal a recycled pgid.
struct GroupKill(Option<i32>);

impl GroupKill {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        let Some(pid) = self.0 else { return };
        // SAFETY: the pid led its own group (`process_group(0)` at spawn), so this
        // either signals that group or fails with ESRCH because it has already
        // exited. It cannot reach the daemon's own group. `kill_on_drop` reaps the
        // child itself, so this is not racing a `wait` we still owe.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
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

    /// The child-process hygiene, against real processes.
    ///
    /// **These bounds are what veld shipped instead of a consent prompt**
    /// (`docs/extensions-vision.md`), and every one of them could be deleted
    /// without breaking a compile, a lint or another test. The symptom of losing
    /// one is a leaked process tree, a hung request, or the daemon's memory — none
    /// of which anybody reproduces from a diff.
    mod hygiene {
        use super::*;

        fn shell(script: &str) -> veld_core::config::CommandSpec {
            veld_core::config::CommandSpec::Shell(script.to_owned())
        }

        async fn run(script: &str, timeout: Duration) -> Output {
            spawn_command(&shell(script), "/tmp", &HashMap::new(), timeout)
                .await
                .expect("spawned")
        }

        #[tokio::test]
        async fn stdin_is_closed_so_a_prompt_cannot_hang_the_run() {
            // A CLI that reads stdin must reach EOF at once rather than waiting on a
            // pipe nobody will ever write to. This is what makes an unauthenticated
            // `gh` fail with its login hint instead of wedging until the deadline.
            let out = run("cat; echo done", Duration::from_secs(5)).await;
            assert!(!out.timed_out, "a command reading stdin must not block");
            assert!(out.success);
            assert_eq!(out.stdout.trim(), "done");
        }

        #[tokio::test]
        async fn output_is_capped_while_reading_not_afterwards() {
            // Writes well past the cap and then **exits**, so this runs on the
            // success path. That matters: the timeout path returns an empty
            // `stdout` by contract, so a command that had to be killed would
            // satisfy a length assertion without the cap existing at all — the
            // test would pass for the wrong reason.
            let over = MAX_OUTPUT_BYTES * 3;
            let out = run(
                &format!("yes veld | head -c {over}"),
                Duration::from_secs(10),
            )
            .await;
            assert!(!out.timed_out, "must exit on its own, not be killed");
            // Stated against what was *written*, not only against the constant: an
            // assertion of the form `len() <= MAX_OUTPUT_BYTES` alone is satisfied
            // by a cap of any size, including one large enough to be no cap at all,
            // because the input is sized from the same constant.
            assert!(
                out.stdout.len() < over,
                "nothing was dropped — kept all {over} bytes, so the cap is not applied"
            );
            assert!(
                out.truncated,
                "a run that lost output must say so, or a payload cut mid-JSON is \
                 rendered as if it were the author's text"
            );
            assert!(
                !out.stdout.is_empty() && out.stdout.len() <= MAX_OUTPUT_BYTES,
                "read {} bytes, cap is {MAX_OUTPUT_BYTES}",
                out.stdout.len()
            );
        }

        #[tokio::test]
        async fn the_deadline_kills_the_whole_group_not_just_the_shell() {
            // The reason the kill is `killpg` and not `kill`: a `shell` command's
            // `sh` forks the thing that is actually stuck, so killing the shell
            // alone leaves a process holding the pipe and the CPU forever.
            //
            // The grandchild reports its own pid to a file rather than to stdout,
            // because stdout is empty on the timeout path by contract — and the pid
            // is then checked directly. A `pgrep -f "sleep 30"` would match the
            // `sh -c 'sleep 30 …'` wrapper's own command line too and pass or fail
            // for the wrong reason.
            let dir = tempfile::tempdir().expect("tempdir");
            let pidfile = dir.path().join("grandchild.pid");
            let out = run(
                &format!("sleep 30 & echo $! > {}; wait", pidfile.display()),
                Duration::from_millis(300),
            )
            .await;
            assert!(out.timed_out, "the deadline must fire");

            let pid: i32 = std::fs::read_to_string(&pidfile)
                .expect("the shell wrote its grandchild's pid")
                .trim()
                .parse()
                .expect("a pid");
            // SIGKILL is asynchronous; give the kernel a moment to reap.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                !veld_core::process::is_alive(pid as u32),
                "pid {pid} (`sleep 30`) survived the deadline — killing the direct \
                 child is not enough, the group has to go"
            );
        }

        #[tokio::test]
        async fn the_child_environment_cannot_colour_the_contract() {
            // A tool that thinks it is on a terminal emits escape sequences, and
            // those would land inside the badge's text.
            let out = run("echo \"$NO_COLOR|$TERM\"", Duration::from_secs(5)).await;
            assert_eq!(out.stdout.trim(), "1|dumb");
        }

        #[tokio::test]
        async fn the_command_runs_in_the_worktree() {
            let dir = tempfile::tempdir().expect("tempdir");
            let out = spawn_command(
                &shell("pwd -P"),
                &dir.path().to_string_lossy(),
                &HashMap::new(),
                Duration::from_secs(5),
            )
            .await
            .expect("spawned");
            let expected = std::fs::canonicalize(dir.path()).expect("canonical");
            assert_eq!(out.stdout.trim(), expected.to_string_lossy());
        }
    }

    /// Running an action makes the worktree's remembered values untrustworthy.
    ///
    /// The regression this pins was reported as "clicking sometimes loads but the
    /// colour does not change": an action changed state, the client forced a
    /// refresh, and `FORCED_REFRESH_FLOOR` answered it from the run made *before*
    /// the action — so it depended on how fast you clicked. Asserted against
    /// `invalidated_since`, which is what `evaluate`'s freshness gate consults,
    /// because the handler needs a database and a real child process and the defect
    /// was never in either.
    #[tokio::test]
    async fn an_action_makes_earlier_values_untrustworthy() {
        let mine = "/tmp/wt-mine";
        let other = "/tmp/wt-other";
        let before = Instant::now();
        // A stamp has to be strictly later than the value it invalidates, and
        // `Instant` resolution is fine but not zero — sleep past it rather than
        // relying on two adjacent calls differing.
        tokio::time::sleep(Duration::from_millis(2)).await;

        invalidate(mine);

        assert!(
            invalidated_since(mine, before),
            "a value produced before the action must be re-run, not served"
        );
        // Including one produced by a run that was already in flight when the
        // action landed — that is the case a walk over the cells could not cover
        // without waiting for the run to finish.
        assert!(
            !invalidated_since(mine, Instant::now()),
            "a value produced after the action is trustworthy"
        );
        // Scoped to the worktree: an action here says nothing about another
        // checkout's badges.
        assert!(
            !invalidated_since(other, before),
            "another worktree's values must survive"
        );
        // Leave the shared statics as they were found — these tests share a process.
        INVALIDATED
            .lock()
            .expect("extension invalidations poisoned")
            .retain(|wt, _| wt != mine && wt != other);
    }

    /// A run already in flight when an action lands does not satisfy the gate.
    ///
    /// The case `an_action_makes_earlier_values_untrustworthy` cannot see, because it
    /// never calls [`evaluate`] — and the *correlated* case in practice, since people
    /// click the action a badge just offered them, so a poll is often mid-flight.
    /// With the value stamped at completion this passed the freshness gate (finish >
    /// stamp) and the post-action refresh was answered from the pre-action value:
    /// exactly the "clicking loads but nothing changes" bug, in a narrower window.
    #[tokio::test]
    async fn a_run_in_flight_when_an_action_lands_is_not_reused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runs = dir.path().join("runs");
        let root = dir.path().to_string_lossy().into_owned();
        // Appends a line per run, so "did it re-run" is countable rather than
        // inferred from the rendered value.
        let section = Arc::new(veld_core::ide::parse(Some(&serde_json::json!({
            "extensions": [{
                "id": "slow", "slot": "topBar", "type": "status", "label": "slow",
                "shell": format!("echo run >> {}; sleep 0.4; echo value", runs.display()),
                "refresh_seconds": 3600,
            }]
        }))));
        let ext = section.extension("slow").expect("declared").clone();

        let count = || {
            std::fs::read_to_string(&runs)
                .map(|t| t.lines().count())
                .unwrap_or(0)
        };

        let first = tokio::spawn(evaluate(
            ext.clone(),
            Arc::clone(&section),
            root.clone(),
            HashMap::new(),
            false,
        ));
        // Let it get past the spawn and into the sleep, then act while it runs.
        tokio::time::sleep(Duration::from_millis(150)).await;
        invalidate(&root);
        first.await.expect("first run").text.expect("a value");
        assert_eq!(count(), 1, "the first evaluation ran once");

        // `refresh_seconds` is an hour and this is *not* forced, so the only thing
        // that can make this re-run is the invalidation.
        let after = evaluate(ext, section, root.clone(), HashMap::new(), false).await;
        assert_eq!(
            count(),
            2,
            "a value from a run that started before the action must not be reused"
        );
        assert_eq!(after.text.as_deref(), Some("value"));

        RESULTS
            .lock()
            .expect("extension results poisoned")
            .retain(|(wt, _), _| wt != &root);
        INVALIDATED
            .lock()
            .expect("extension invalidations poisoned")
            .retain(|wt, _| wt != &root);
    }

    /// Eviction never drops a cell a run is in flight on.
    ///
    /// The property `MAX_TRACKED` documents, and the one a `map.clear()` broke: a
    /// dropped locked cell lets the next caller mint a fresh one and start a second
    /// child for the same extension, which is the single-flight guarantee gone.
    #[tokio::test]
    async fn eviction_spares_cells_with_a_run_in_flight() {
        let busy = "/tmp/wt-busy";
        let held = cell(busy, "in-flight");
        let guard = held.lock().await;

        // Push the map over the threshold with idle cells.
        for i in 0..=MAX_TRACKED {
            let _ = cell(&format!("/tmp/wt-idle-{i}"), "idle");
        }

        {
            let map = RESULTS.lock().expect("extension results poisoned");
            assert!(
                map.contains_key(&(busy.to_owned(), "in-flight".to_owned())),
                "a locked cell must survive eviction"
            );
        }
        drop(guard);
        RESULTS
            .lock()
            .expect("extension results poisoned")
            .retain(|(wt, _), _| wt != busy && !wt.starts_with("/tmp/wt-idle-"));
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
