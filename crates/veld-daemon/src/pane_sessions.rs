//! Listing the sessions a config-declared pane could adopt.
//!
//! One endpoint, one command per pane, one shape on the wire. A pane that
//! declares `ide.panes[].sessions` names a command that prints the sessions its
//! tool has already got lying around for this worktree; the user picks one; the
//! pane launches its own declared `resume` against it. This module is only the
//! *listing* half — the adopting half is `pty::resolve_pane`'s `PaneMode::Adopt`,
//! and the line between them is the one this codebase keeps everywhere:
//!
//! > **The client sends a value. The daemon owns the command.**
//!
//! Nothing a script prints can change what runs. It chooses which session the
//! pane's already-declared `resume` runs against, and `veld_core::ide::is_session_value`
//! is what makes that value inert in an `argv` *and* in a `shell` string.
//!
//! ## Why this is allowed to run a command at all
//!
//! `schema/v3/veld.schema.json` says of `requires_bin` that veld "does not
//! execute a config command to decide whether to draw a menu item", and that is
//! still true of `requires_bin`. This is a different question and the honest
//! description of it is: **veld runs a repo-declared command without the user
//! clicking the thing it is about.** That is exactly what a `status` extension
//! already does, so this reuses that machinery wholesale rather than inventing a
//! second posture: [`super::extensions::spawn_command`]'s bounds (stdin closed,
//! no tty, process-group kill from a drop guard, capped output, `NO_COLOR`), the
//! machine-wide off switch `extensions.autoRefresh`, the single-flight memory
//! below, and — the one that matters most — the **same answer to "declared
//! where?"**.
//!
//! ### Declared where: `extensions.source`, not the worktree
//!
//! Declarations are read from `resolve_declare_root`, which is `main` by
//! default, exactly as a badge's are. The commands still *run* in the worktree
//! being viewed, with its own branch.
//!
//! **An earlier cut of this module read the worktree's own `veld.json`**, and
//! argued it was safe because `pty::resolve_pane` does the same. That argument
//! is wrong in one word: `resolve_pane` runs on a **click**. This does not. A
//! review round found the consequence — check out somebody's pull-request
//! branch, select that worktree in the IDE, and if it has no open tabs the pane
//! chooser mounts by itself and asks for the listers, so the branch's own
//! `sessions: {shell: …}` ran with no gesture at all. That is precisely the hole
//! the `extensions.source = main` default was introduced to close
//! (`docs/extensions-vision.md`, 2026-08-13), and this surface has to be behind
//! it or the default stops meaning what it says.
//!
//! The cost is the one that decision already accepted and documented: a picker
//! added on a branch does not appear until it merges, and
//! `extensions.source = worktree` is the escape hatch for testing one. The
//! residual is also the same — a main-declared lister whose `argv[0]` is a
//! repo-relative script is resolved against `declare_root` by
//! [`super::extensions::spawn_command`], so it runs main's copy, not the
//! branch's.
//!
//! Two things still narrow this further than a badge: it is **not on a timer**
//! (it runs when the pane chooser asks, and never otherwise), and it is capped
//! at [`veld_core::ide::MAX_PANE_SESSION_LISTERS`] panes per project.
//!
//! ## The stdout contract
//!
//! Line-oriented, tab-separated, and deliberately *not* the badge's
//! JSON-or-first-line sniffing. A badge's simple case is one value, so sniffing
//! buys it a free adapter; here the simple case is already a list, and the free
//! adapter is a shell pipeline that ends in `basename`:
//!
//! ```text
//! 1f2e3d4c-8a91-4c02-9f13-77bbd2e5a410
//! 9a8b7c6d-2231-4ff8-b0aa-1e3c9d5f2a77
//! ```
//!
//! A script with something to say splits the line on tabs — `value`, then the
//! text shown for it, then a second line of detail:
//!
//! ```text
//! 1f2e3d4c-…\t2h ago · fixing the pane resume bug\t41 messages
//! ```
//!
//! The tolerances mirror the badge contract exactly, because an author who has
//! written one already knows this one:
//!
//! - **exit 0, no output → no sessions**, and the pane simply does not offer the
//!   picker. This is how a script says "not applicable here", and it is the case
//!   a fresh clone hits.
//! - **a non-zero exit → the picker is offered and says it failed**, with the
//!   stderr tail. A broken script is visible, never silent.
//! - **a line whose value is not [`veld_core::ide::is_session_value`] is
//!   dropped**, and the picker says how many. One bad line never costs the other
//!   nineteen.
//!
//! ## Variables
//!
//! [`veld_core::ide::PANE_SESSIONS_BUILTINS`] is what `veld lint` accepts and
//! [`list`] is what resolves them, and they are a hand-maintained pair like the
//! two scopes before them — `pty::tests::sessions_commands_resolve_exactly_the_names_lint_accepts`
//! is the test that keeps them equal. `pane.id` and `pane.label` are added per
//! pane on top of `worktree_builtins`, which is what lets one script serve
//! several panes (`--agent ${veld.pane.id}`). `pane.token` is deliberately
//! absent from both: this command runs to decide *which* token there will be.

use std::collections::{HashMap, HashSet};
use std::path::Path as FsPath;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::Path;
use serde::Serialize;
use veld_core::ide::{
    MAX_PANE_SESSION_LISTERS, MAX_PANE_SESSIONS, MAX_SESSION_LABEL_CHARS, PaneBody, PaneDef,
    SessionsPicker,
};

use super::desktop::ApiError;
use super::extensions::{clip, load_section, spawn_command, tail_suffix, worktree_target};
use super::pty::{missing_pane_binaries, worktree_builtins};

/// How long one pane's lister gets.
///
/// Shorter than a badge's 20s on purpose: a badge runs behind the user's back
/// and can afford to be slow, while this one is between the user and a pane they
/// are trying to open. A `ls | head` over a session directory is milliseconds; a
/// script that needs ten seconds to list what is on disk is a script whose author
/// wants to know.
const SESSIONS_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one pane's answer is reused before the lister runs again.
///
/// The badge runner's `FORCED_REFRESH_FLOOR` by another name, and for the same
/// reason its comment gives: without it, holding a control down — or a script
/// posting in a loop — spends a child process per event. Deliberately seconds
/// and not `refresh_seconds`-scale: a session that ended thirty seconds ago is
/// exactly the one somebody is looking for, so this may only be long enough to
/// absorb a burst, never long enough to hide a session.
const SESSIONS_FLOOR: Duration = Duration::from_secs(3);

/// One pane's last answer in one worktree, with when its run *started*.
///
/// **A rate-limit memory, not a cache**, copied from `extensions::Cell` and for
/// its reason: the mutex is held across the child run, so a second request
/// arriving mid-run waits and then shares the first one's answer instead of
/// starting a parallel `python3`. A TTL cache would instead return stale and let
/// both callers launch.
type Cell = Arc<tokio::sync::Mutex<Option<(Instant, PaneSessionsView)>>>;

/// `(worktree path, declare_root, the interpolated command)`.
///
/// Keyed on the worktree's **path, never its database id** — `worktrees.id` is
/// an `INTEGER PRIMARY KEY` with no `AUTOINCREMENT` and rows are hard-deleted, so
/// SQLite reuses ids, and this map lives as long as the daemon. A reused id would
/// serve a deleted checkout's session list to a new one. `declare_root` is in the
/// key for the same reason it is in the badge runner's: flipping
/// `extensions.source` changes which config produced the answer.
///
/// **The third element is the command, not the pane id**, and that is the one
/// place this diverges from `extensions::RESULTS`. A badge's command is its own;
/// a lister's is routinely *shared* — the real shape of this feature is several
/// panes of one tool, and this repo's own config is two Claude panes running one
/// `claude-sessions.sh`. Keyed on the pane, that is two identical forks of a
/// script that itself forks a `python3` per row. Keyed on the command they
/// collapse to one, and a script that *does* differ per pane (because it reads
/// `${veld.pane.id}`) interpolates to a different string and correctly does not.
type Key = (String, String, String);

static RESULTS: LazyLock<Mutex<HashMap<Key, Cell>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn cell(root: &str, declare_root: &str, command: &str) -> Cell {
    let mut map = RESULTS.lock().expect("pane sessions results poisoned");
    // Bounded the way the badge runner bounds its own map: this grows with
    // (worktrees × commands) and nothing removes a worktree's entries when it is
    // deleted, so a long-lived daemon would otherwise accumulate them forever.
    //
    // **Cells with a run in flight are spared**, which the badge runner also
    // does and for a reason worth restating: a plain `clear()` drops a cell a
    // task is holding the lock on, the next request builds a fresh one, and the
    // single-flight guarantee lapses at exactly the moment the map is busiest.
    // `strong_count == 1` means this map is the only owner, so nobody is inside.
    if map.len() > MAX_TRACKED {
        map.retain(|_, c| Arc::strong_count(c) > 1);
    }
    Arc::clone(
        map.entry((root.to_owned(), declare_root.to_owned(), command.to_owned()))
            .or_default(),
    )
}

/// How many `(worktree, declare_root, pane)` answers to remember before dropping
/// the lot. Generous: 18 worktrees × 8 listers is 144.
const MAX_TRACKED: usize = 512;

/// One session a pane could adopt.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct SessionRow {
    /// What goes into `${veld.pane.token}`. Always passes
    /// [`veld_core::ide::is_session_value`] by the time it is on the wire.
    value: String,
    /// What the picker shows. The script's second field, or the value itself.
    label: String,
    /// A second, quieter line. The script's third field, when it wrote one.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// Whether a pane in this worktree currently has this session open.
    ///
    /// Computed here rather than left to the client, because the client is
    /// never told a pane's token (`Db::resumable_panes` withholds it), so it
    /// structurally cannot work this out. `resolve_pane` refuses the adopt
    /// regardless — this field is only so the picker can say so *before* the
    /// click instead of after it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    in_use: bool,
}

/// What one pane's lister produced.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct PaneSessionsView {
    /// The `ide.panes[].id` this belongs to.
    id: String,
    /// The picker's entry-point text, from `sessions.label`.
    label: String,
    /// Whether clicking the pane should open the picker instead of starting
    /// fresh, from `sessions.ask_first`.
    ///
    /// It rides on the answer rather than on `PaneView` because it is only ever
    /// consulted once the rows are in hand: a pane with no rows opens fresh on
    /// click whatever this says, so putting it on the worktree listing would be
    /// a field the renderer had to ignore most of the time.
    ask_first: bool,
    /// `ok` — rows to show. `empty` — ran fine, found nothing, offer no picker.
    /// `failed` / `timeout` — offer the picker and say so. `off` — the machine
    /// has `extensions.autoRefresh` disabled.
    state: &'static str,
    sessions: Vec<SessionRow>,
    /// Why, for every state but `ok` and `empty`; and for `ok`, whatever had to
    /// be dropped to get here.
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PaneSessionsResponse {
    panes: Vec<PaneSessionsView>,
}

/// `POST /api/worktrees/{id}/panes/sessions`
///
/// Batched over every pane in the worktree that declares one, the way
/// [`super::extensions::status`] is batched over a slot: the caller is a screen
/// opening, not a control being clicked, so one round trip is the right shape and
/// it keeps the fan-out bounded by config rather than by however many requests a
/// client feels like making.
pub(crate) async fn list(Path(id): Path<i64>) -> Result<Json<PaneSessionsResponse>, ApiError> {
    // The *same* resolver the badge endpoints use, deliberately shared rather
    // than reimplemented — see this module's docs on "declared where?".
    let (root, branch, auto_refresh, declare_root) = worktree_target(id)?;
    // `extensions.source = main` with no resolvable main checkout: fail closed,
    // exactly as `extensions::status` and `desktop::extensions_view_for` do, so
    // no surface disagrees about what is declared.
    let Some(declare_root) = declare_root else {
        return Ok(Json(PaneSessionsResponse { panes: Vec::new() }));
    };

    // Declarations from `declare_root`; the branch and the working directory
    // below stay this worktree's own.
    let Some((config, section)) = load_section(&declare_root) else {
        return Ok(Json(PaneSessionsResponse { panes: Vec::new() }));
    };

    let declared: Vec<(&PaneDef, &SessionsPicker)> = section
        .panes
        .iter()
        .filter_map(|pane| {
            let PaneBody::Terminal(terminal) = &pane.body;
            terminal.sessions.as_ref().map(|s| (pane, s))
        })
        // `parse_panes` already drops pickers past the cap with a lint problem,
        // so this is the defensive half of the same bound: the config is re-read
        // from disk on every request and this is the process count it decides.
        .take(MAX_PANE_SESSION_LISTERS)
        .collect();
    if declared.is_empty() {
        return Ok(Json(PaneSessionsResponse { panes: Vec::new() }));
    }

    if !auto_refresh {
        return Ok(Json(PaneSessionsResponse {
            panes: declared
                .into_iter()
                .map(|(pane, picker)| PaneSessionsView {
                    id: pane.id.clone(),
                    label: picker.label.clone(),
                    ask_first: picker.ask_first,
                    state: "off",
                    sessions: Vec::new(),
                    // Names the toggle as the settings screen labels it. **The
                    // UI deliberately renders nothing at all for this state** —
                    // the switch is the user's own choice and Settings already
                    // explains it, so a pane must look exactly as it did before
                    // this feature existed. The message is here for whoever is
                    // reading the endpoint (a `curl`, the daemon log, a future
                    // client), not for a card.
                    message: Some(
                        "veld is not running project commands on this machine (Settings → \
                         General → Let projects run their own status commands)"
                            .to_owned(),
                    ),
                })
                .collect(),
        }));
    }

    // Which of this worktree's recorded tokens belong to a session that is
    // *currently open* — so a row for a conversation another pane is running can
    // say so before it is clicked. `resolve_pane` refuses it either way.
    let taken = super::pty::live_pane_tokens(id).await;

    // From `config`, which is `declare_root`'s — but `root` and `branch` are the
    // viewed worktree's, so `${veld.branch}` names the checkout being looked at
    // rather than main's. Same split as a badge's.
    let worktree = worktree_builtins(FsPath::new(&root), &branch, &config);

    // Concurrent, like the badge fan-out: these are independent child processes
    // and the screen waits for the slowest one either way. The count is already
    // bounded by how many panes a project declares.
    let runs = declared.into_iter().map(|(pane, picker)| {
        // Per pane, because two of the names in `PANE_SESSIONS_BUILTINS` are the
        // pane's own. Built here rather than in `run_one` so the worktree half —
        // which involves a `slugify` and a config read — is computed once.
        let mut builtins = worktree.clone();
        builtins.insert("pane.id".to_owned(), pane.id.clone());
        builtins.insert("pane.label".to_owned(), pane.label.clone());
        let root = root.clone();
        let declare_root = declare_root.clone();
        let taken = taken.clone();
        async move { evaluate(pane, picker, &root, &declare_root, &builtins, &taken).await }
    });

    Ok(Json(PaneSessionsResponse {
        panes: futures_util::future::join_all(runs).await,
    }))
}

/// The command as it will actually be spawned, for use as a cache key.
///
/// `None` when it does not interpolate — the same failure `spawn_command` is
/// about to return, so the caller does not need to distinguish it here.
fn interpolated(
    spec: &veld_core::config::CommandSpec,
    builtins: &HashMap<String, String>,
) -> Option<String> {
    let ctx = veld_core::variables::VariableContext {
        builtins: builtins.clone(),
        ..Default::default()
    };
    spec.interpolate(&ctx).ok().map(|s| s.display())
}

/// One pane's answer, behind the single-flight memory.
async fn evaluate(
    pane: &PaneDef,
    picker: &SessionsPicker,
    root: &str,
    declare_root: &str,
    builtins: &HashMap<String, String>,
    taken: &HashSet<String>,
) -> PaneSessionsView {
    // The *interpolated* command, so two panes running one script share a run and
    // two panes running the same script with different `${veld.pane.id}` do not.
    // Falling back to the pane id keeps a spec that cannot interpolate — which
    // `run_one` is about to report as failed anyway — from sharing a cell with an
    // unrelated one.
    // **Before the cell, because it is the pane's answer and the cell is shared.**
    // Two panes may run one lister and declare different `requires_bin`; keyed on
    // the command, whichever missed the cache first would otherwise decide "the
    // binary is missing" for both, and `mine` cannot correct it because `state`
    // and `message` are the half that legitimately *is* shared.
    if let Some(missing) = missing_pane_binaries(&pane.requires_bin).first() {
        // `empty`, so the card is left exactly as the worktree listing drew it —
        // which already names the missing binary on that card's own line
        // (`PaneView::missing`). The message here is for whoever is reading the
        // endpoint, not for the UI, which renders nothing for this state; saying
        // it twice on one card is how two surfaces start disagreeing.
        return PaneSessionsView {
            id: pane.id.clone(),
            label: picker.label.clone(),
            ask_first: picker.ask_first,
            state: "empty",
            sessions: Vec::new(),
            message: Some(format!("{missing} is not installed on this machine")),
        };
    }
    let key = interpolated(&picker.command, builtins).unwrap_or_else(|| pane.id.clone());
    let cell = cell(root, declare_root, &key);
    // Held across the run on purpose: a second chooser opening mid-run waits
    // here and is then answered from the run the first one made, instead of
    // forking a second copy of the project's script.
    let mut guard = cell.lock().await;
    if let Some((at, view)) = guard.as_ref() {
        if at.elapsed() < SESSIONS_FLOOR {
            return mine(view.clone(), pane, picker);
        }
    }
    // Stamped before the run, not after, so the floor measures from when the
    // work started — the reasoning `extensions::evaluate` spells out.
    let started = Instant::now();
    let view = run_one(pane, picker, root, declare_root, builtins, taken).await;
    *guard = Some((started, view.clone()));
    mine(view, pane, picker)
}

/// Re-stamp a shared answer with *this* pane's identity.
///
/// The cell is keyed on the command, so a cached view may have been produced for
/// a different pane running the same script — and three of its fields are the
/// pane's, not the command's. Without this, two Claude panes sharing one lister
/// would have the second one answered under the first one's `id`, and the UI
/// keys its map by that id: the second pane would show no picker while the first
/// showed two.
fn mine(view: PaneSessionsView, pane: &PaneDef, picker: &SessionsPicker) -> PaneSessionsView {
    PaneSessionsView {
        id: pane.id.clone(),
        label: picker.label.clone(),
        ask_first: picker.ask_first,
        ..view
    }
}

async fn run_one(
    pane: &PaneDef,
    picker: &SessionsPicker,
    root: &str,
    declare_root: &str,
    builtins: &HashMap<String, String>,
    taken: &HashSet<String>,
) -> PaneSessionsView {
    let base = |state: &'static str, message: Option<String>| PaneSessionsView {
        id: pane.id.clone(),
        label: picker.label.clone(),
        ask_first: picker.ask_first,
        state,
        sessions: Vec::new(),
        message,
    };

    // `root` is the cwd, `declare_root` is what a relative `argv[0]` resolves
    // against — so a main-declared `scripts/veld/…` lister runs *main's* copy of
    // the script even while its cwd is the branch being viewed. See the module
    // docs on "declared where?".
    let out = match spawn_command(
        &picker.command,
        root,
        declare_root,
        builtins,
        SESSIONS_TIMEOUT,
        // No stdin: a session lister is asked what exists, it is not handed
        // anything. Only `ide.worktreeName` writes to a project command's
        // stdin — see `spawn_command`.
        None,
    )
    .await
    {
        Err(message) => return base("failed", Some(message)),
        Ok(out) => out,
    };
    if out.timed_out {
        return base(
            "timeout",
            Some(format!(
                "listing sessions took longer than {}s{}",
                SESSIONS_TIMEOUT.as_secs(),
                tail_suffix(&out.stderr)
            )),
        );
    }
    if !out.success {
        return base(
            "failed",
            Some(format!(
                "exited with status {}{}",
                out.code
                    .map_or_else(|| "unknown".to_owned(), |c| c.to_string()),
                tail_suffix(&out.stderr)
            )),
        );
    }

    let parsed = parse_sessions(&out.stdout, out.truncated, taken);
    if parsed.rows.is_empty() {
        // **Two different answers that both produce no rows.**
        //
        // Nothing dropped is the ordinary "nothing to resume here": the script
        // printed nothing, the pane opens as it always did, no dialog.
        //
        // Rows *dropped* is a script that is not producing what it thinks it is
        // — a badge-shaped adapter printing one line of JSON is the case that
        // will actually happen — and `empty` renders nothing, so the note would
        // be computed and then thrown away, leaving the author debugging a
        // picker that never appears with no message anywhere. That is a broken
        // script, and this module's contract is that a broken script is visible.
        return match parsed.note {
            Some(note) => base("failed", Some(note)),
            None => base("empty", None),
        };
    }
    PaneSessionsView {
        sessions: parsed.rows,
        message: parsed.note,
        ..base("ok", None)
    }
}

/// Strip what a row's *display* text must never carry.
///
/// `clip` bounds length; this bounds content, and the two are different
/// problems. `NO_COLOR=1`/`TERM=dumb` are requests a command may ignore, and
/// React escapes markup but not text direction — so without this a label
/// containing U+202E (right-to-left override) **renders reversed**, and a row can
/// read as a different session from the one it adopts. That is a spoof on the
/// one control in this feature where reading the row is how you choose.
///
/// Control characters go for the ordinary reason (a stray `\r` or escape
/// sequence in a card), the bidi overrides for the spoof. Replaced with a space
/// rather than removed, so a label does not silently close up around what was
/// taken out.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

struct Parsed {
    rows: Vec<SessionRow>,
    note: Option<String>,
}

/// Read a lister's stdout into rows. See the module docs for the contract.
fn parse_sessions(stdout: &str, truncated: bool, taken: &HashSet<String>) -> Parsed {
    // A cut payload keeps its whole lines and loses the partial tail one. That is
    // the opposite of what a badge does with a cut payload, and it is right for
    // the same reason: a badge's payload is one indivisible object, a list's is
    // not, so refusing the lot would throw away rows that parsed perfectly.
    let mut lines: Vec<&str> = stdout.lines().collect();
    // Only when the cut actually landed mid-line. `read_capped` stops at a byte
    // count with no line awareness, so a cut that happened to fall on a newline
    // left every line complete — and dropping one there loses a perfectly good
    // row for nothing.
    if truncated && !stdout.ends_with('\n') {
        lines.pop();
    }

    let mut rows = Vec::new();
    let mut skipped = 0usize;
    let mut over_cap = 0usize;
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.splitn(3, '\t');
        // `unwrap_or_default` cannot fire — `splitn` always yields at least one —
        // but spelling it that way keeps the parse total.
        let value = fields.next().unwrap_or_default().trim();
        if !veld_core::ide::is_session_value(value) {
            skipped += 1;
            continue;
        }
        if rows.len() >= MAX_PANE_SESSIONS {
            over_cap += 1;
            continue;
        }
        // **First wins.** A script that walks more than one session directory can
        // print the same basename twice, and the value is the row's identity all
        // the way to the browser (React keys off it) — so a duplicate is a
        // duplicate-key warning and two rows that cannot be told apart. Counted
        // with the skipped rows rather than silently swallowed.
        if rows.iter().any(|r: &SessionRow| r.value == value) {
            skipped += 1;
            continue;
        }
        // **Sanitised before the emptiness test, not after.** A field of nothing
        // but bidi overrides is non-empty as written and empty once cleaned, so
        // deciding first gave the row a blank label where the value should have
        // stood in — the one outcome `bare_ids_need_no_adapter` exists to
        // prevent — and a blank second line under it.
        let clean = |f: Option<&str>| f.map(sanitize).filter(|s: &String| !s.is_empty());
        let mut fields = fields.map(str::trim);
        let label = clean(fields.next());
        let detail = clean(fields.next());
        rows.push(SessionRow {
            value: value.to_owned(),
            label: clip(label.as_deref().unwrap_or(value), MAX_SESSION_LABEL_CHARS),
            detail: detail.map(|d| clip(&d, MAX_SESSION_LABEL_CHARS)),
            in_use: taken.contains(value),
        });
    }

    let mut notes = Vec::new();
    if truncated {
        notes.push("the list was cut short because the command printed too much".to_owned());
    }
    if over_cap > 0 {
        notes.push(format!(
            "{over_cap} more were not shown (veld lists at most {MAX_PANE_SESSIONS})"
        ));
    }
    if skipped > 0 {
        notes.push(format!(
            "{skipped} line(s) were skipped — their first field is not a usable session id, \
             or repeats one already listed"
        ));
    }
    Parsed {
        rows,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No pane in this worktree currently holds a session — the ordinary case,
    /// and the one every parsing test wants.
    fn none_taken() -> HashSet<String> {
        HashSet::new()
    }

    fn parsed(stdout: &str, truncated: bool) -> Parsed {
        parse_sessions(stdout, truncated, &none_taken())
    }

    fn values(stdout: &str) -> Vec<String> {
        parsed(stdout, false)
            .rows
            .into_iter()
            .map(|r| r.value)
            .collect()
    }

    #[test]
    fn bare_ids_need_no_adapter() {
        let out = parsed("abc123\ndef456\n", false);
        assert_eq!(values("abc123\ndef456\n"), ["abc123", "def456"]);
        // With no label field, the value is its own label — a picker showing
        // blank rows would be worse than one showing ids.
        assert_eq!(out.rows[0].label, "abc123");
        assert_eq!(out.rows[0].detail, None);
        assert_eq!(out.note, None);
    }

    #[test]
    fn tabs_carry_the_label_and_the_detail() {
        let out = parsed("abc123\t2h ago · the pane bug\t41 messages\n", false);
        assert_eq!(out.rows[0].label, "2h ago · the pane bug");
        assert_eq!(out.rows[0].detail.as_deref(), Some("41 messages"));
    }

    #[test]
    fn a_fourth_field_stays_in_the_detail() {
        // `splitn(3)` on purpose: a label written by a script that happens to
        // contain a tab must not silently become a phantom column. The tab
        // itself arrives as a space — `sanitize` flattens control characters,
        // and the detail renders as one ellipsised line where a tab is noise —
        // but the *text* is all still there, which is the property that matters.
        let out = parsed("abc\tlabel\tone\ttwo\n", false);
        assert_eq!(out.rows[0].detail.as_deref(), Some("one two"));
    }

    #[test]
    fn one_bad_line_never_costs_the_others() {
        let out = parsed("abc\n; rm -rf /\ndef\n", false);
        assert_eq!(
            out.rows.iter().map(|r| &r.value).collect::<Vec<_>>(),
            ["abc", "def"]
        );
        assert!(out.note.as_deref().unwrap().contains("1 line(s)"));
    }

    #[test]
    fn a_shared_answer_is_restamped_with_the_pane_that_asked() {
        // Two panes running one script share a cell, so the cached view carries
        // whichever pane ran it first. The UI keys its map on `id`, so handing
        // the second pane the first one's identity loses it its picker.
        let other = PaneSessionsView {
            id: "claude".to_owned(),
            label: "First label".to_owned(),
            ask_first: true,
            state: "ok",
            sessions: vec![SessionRow {
                value: "abc".to_owned(),
                label: "abc".to_owned(),
                detail: None,
                in_use: false,
            }],
            message: None,
        };
        let pane = PaneDef {
            id: "claude-sonnet".to_owned(),
            label: "Claude Sonnet".to_owned(),
            description: None,
            icon: None,
            requires_bin: Vec::new(),
            body: PaneBody::Terminal(veld_core::ide::TerminalPane {
                launch: veld_core::config::CommandSpec::Argv(vec!["x".to_owned()]),
                resume: None,
                sessions: None,
                agent: None,
                auto_resume: false,
                close_on_exit: true,
                fixed_label: false,
            }),
        };
        let picker = SessionsPicker {
            label: "Second label".to_owned(),
            ask_first: false,
            command: veld_core::config::CommandSpec::Argv(vec!["list.sh".to_owned()]),
        };
        let out = mine(other, &pane, &picker);
        assert_eq!(out.id, "claude-sonnet");
        assert_eq!(out.label, "Second label");
        assert!(!out.ask_first);
        // The half that really is shared comes through untouched.
        assert_eq!(out.state, "ok");
        assert_eq!(out.sessions.len(), 1);
    }

    #[test]
    fn a_repeated_value_is_listed_once() {
        // The value is the row's identity all the way into React's key, so two
        // rows with one value is a duplicate-key warning and two rows nobody can
        // tell apart.
        let out = parsed("abc\tfirst\ndef\nabc\tsecond\n", false);
        assert_eq!(
            out.rows.iter().map(|r| &r.value).collect::<Vec<_>>(),
            ["abc", "def"]
        );
        assert_eq!(out.rows[0].label, "first", "the first occurrence wins");
        assert!(out.note.as_deref().unwrap().contains("1 line(s)"));
    }

    #[test]
    fn a_leading_dash_is_not_a_session_id() {
        // Argument injection: `claude --resume --dangerously-…` is a different
        // command from the one the config declared.
        assert!(values("-oops\n").is_empty());
    }

    #[test]
    fn empty_output_is_no_sessions_not_an_error() {
        assert_eq!(parsed("", false).rows, Vec::new());
        assert_eq!(parsed("   \n\n", false).note, None);
    }

    #[test]
    fn a_session_another_pane_is_running_is_marked() {
        let taken = HashSet::from(["busy".to_owned()]);
        let out = parse_sessions("busy\nfree\n", false, &taken);
        assert!(out.rows[0].in_use, "the one a live pane holds");
        assert!(!out.rows[1].in_use);
        // Still listed, not dropped: the row is the only place the user can be
        // told *why* they cannot have it.
        assert_eq!(out.rows.len(), 2);
    }

    #[test]
    fn a_truncated_payload_drops_only_its_partial_tail() {
        let out = parsed("abc\ndef\nghi-half", true);
        assert_eq!(
            out.rows.iter().map(|r| &r.value).collect::<Vec<_>>(),
            ["abc", "def"]
        );
        assert!(out.note.as_deref().unwrap().contains("cut short"));
    }

    #[test]
    fn a_cut_that_landed_on_a_newline_keeps_every_row() {
        // The cap is a byte count with no line awareness, so it sometimes falls
        // exactly on a boundary. There is no partial line to drop there, and
        // dropping one anyway costs a good row.
        let out = parsed("abc\ndef\n", true);
        assert_eq!(
            out.rows.iter().map(|r| &r.value).collect::<Vec<_>>(),
            ["abc", "def"]
        );
        // Still says it was cut: rows the user cannot see were still lost.
        assert!(out.note.as_deref().unwrap().contains("cut short"));
    }

    #[test]
    fn the_cap_is_velds_and_it_says_when_it_bit() {
        let stdout = (0..MAX_PANE_SESSIONS + 5)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = parsed(&stdout, false);
        assert_eq!(out.rows.len(), MAX_PANE_SESSIONS);
        assert!(out.note.as_deref().unwrap().contains("5 more"));
    }

    #[test]
    fn carriage_returns_do_not_become_part_of_a_value() {
        // A script written on Windows, or one piping through a tool that emits
        // CRLF. `\r` is not in the accepted set, so without the trim every row
        // would be silently skipped.
        assert_eq!(values("abc\r\ndef\r\n"), ["abc", "def"]);
    }

    #[test]
    fn a_label_cannot_reverse_itself_or_carry_control_bytes() {
        // U+202E makes the rest of a line render right-to-left, so a row could
        // read as one session and adopt another — the one spoof that matters on
        // a control where reading the row *is* the choice.
        let out = parsed("abc\tsafe\u{202e}gnp.exe\tone\u{0007}two", false);
        assert!(!out.rows[0].label.contains('\u{202e}'));
        assert_eq!(out.rows[0].label, "safe gnp.exe");
        assert_eq!(out.rows[0].detail.as_deref(), Some("one two"));
        // The value itself never needed this: `is_session_value` already refuses
        // every character involved.
        assert_eq!(out.rows[0].value, "abc");
    }

    #[test]
    fn a_label_that_sanitises_to_nothing_falls_back_to_the_value() {
        // A field of nothing but bidi overrides is non-empty as written and empty
        // once cleaned. Deciding "is there a label" before cleaning gave the row
        // a blank line where the id should have been.
        let out = parsed("abc\t\u{202e}\t\u{202d}\n", false);
        assert_eq!(out.rows[0].label, "abc");
        assert_eq!(out.rows[0].detail, None);
    }

    #[test]
    fn a_long_label_is_clipped_not_dropped() {
        let long = "x".repeat(MAX_SESSION_LABEL_CHARS + 40);
        let out = parsed(&format!("abc\t{long}"), false);
        assert_eq!(out.rows[0].label.chars().count(), MAX_SESSION_LABEL_CHARS);
    }
}
