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

use std::collections::HashMap;
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

/// `(worktree path, declare_root, pane id)`.
///
/// Keyed on the worktree's **path, never its database id** — `worktrees.id` is
/// an `INTEGER PRIMARY KEY` with no `AUTOINCREMENT` and rows are hard-deleted, so
/// SQLite reuses ids, and this map lives as long as the daemon. A reused id would
/// serve a deleted checkout's session list to a new one. `declare_root` is in the
/// key for the same reason it is in the badge runner's: flipping
/// `extensions.source` changes which config produced the answer.
type Key = (String, String, String);

static RESULTS: LazyLock<Mutex<HashMap<Key, Cell>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn cell(root: &str, declare_root: &str, id: &str) -> Cell {
    let mut map = RESULTS.lock().expect("pane sessions results poisoned");
    // Bounded the way the badge runner bounds its own map: this grows with
    // (worktrees × panes) and nothing removes a worktree's entries when it is
    // deleted, so a long-lived daemon would otherwise accumulate them forever.
    if map.len() > MAX_TRACKED {
        map.clear();
    }
    Arc::clone(
        map.entry((root.to_owned(), declare_root.to_owned(), id.to_owned()))
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
        async move { evaluate(pane, picker, &root, &declare_root, &builtins).await }
    });

    Ok(Json(PaneSessionsResponse {
        panes: futures_util::future::join_all(runs).await,
    }))
}

/// One pane's answer, behind the single-flight memory.
async fn evaluate(
    pane: &PaneDef,
    picker: &SessionsPicker,
    root: &str,
    declare_root: &str,
    builtins: &HashMap<String, String>,
) -> PaneSessionsView {
    let cell = cell(root, declare_root, &pane.id);
    // Held across the run on purpose: a second chooser opening mid-run waits
    // here and is then answered from the run the first one made, instead of
    // forking a second copy of the project's script.
    let mut guard = cell.lock().await;
    if let Some((at, view)) = guard.as_ref() {
        if at.elapsed() < SESSIONS_FLOOR {
            return view.clone();
        }
    }
    // Stamped before the run, not after, so the floor measures from when the
    // work started — the reasoning `extensions::evaluate` spells out.
    let started = Instant::now();
    let view = run_one(pane, picker, root, declare_root, builtins).await;
    *guard = Some((started, view.clone()));
    view
}

async fn run_one(
    pane: &PaneDef,
    picker: &SessionsPicker,
    root: &str,
    declare_root: &str,
    builtins: &HashMap<String, String>,
) -> PaneSessionsView {
    let base = |state: &'static str, message: Option<String>| PaneSessionsView {
        id: pane.id.clone(),
        label: picker.label.clone(),
        ask_first: picker.ask_first,
        state,
        sessions: Vec::new(),
        message,
    };

    // The same gate the pane itself is behind. Running a lister for a pane the
    // user cannot start would spend a subprocess to populate a picker attached
    // to a disabled card.
    if let Some(missing) = missing_pane_binaries(&pane.requires_bin).first() {
        // `empty`, so the card is left exactly as the worktree listing drew it —
        // which already names the missing binary on that card's own line
        // (`PaneView::missing`). The message here is for whoever is reading the
        // endpoint, not for the UI, which renders nothing for this state; saying
        // it twice on one card is how two surfaces start disagreeing.
        return base(
            "empty",
            Some(format!("{missing} is not installed on this machine")),
        );
    }

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

    let parsed = parse_sessions(&out.stdout, out.truncated);
    if parsed.rows.is_empty() {
        // No rows and nothing dropped is the ordinary "nothing to resume here"
        // answer. No rows but lines dropped is a script that is not producing what
        // it thinks it is, and saying nothing would leave the author debugging a
        // picker that never appears.
        return base("empty", parsed.note);
    }
    PaneSessionsView {
        sessions: parsed.rows,
        message: parsed.note,
        ..base("ok", None)
    }
}

struct Parsed {
    rows: Vec<SessionRow>,
    note: Option<String>,
}

/// Read a lister's stdout into rows. See the module docs for the contract.
fn parse_sessions(stdout: &str, truncated: bool) -> Parsed {
    // A cut payload keeps its whole lines and loses the partial tail one. That is
    // the opposite of what a badge does with a cut payload, and it is right for
    // the same reason: a badge's payload is one indivisible object, a list's is
    // not, so refusing the lot would throw away rows that parsed perfectly.
    let mut lines: Vec<&str> = stdout.lines().collect();
    if truncated {
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
        let label = fields.next().map(str::trim).filter(|s| !s.is_empty());
        let detail = fields.next().map(str::trim).filter(|s| !s.is_empty());
        rows.push(SessionRow {
            value: value.to_owned(),
            label: clip(label.unwrap_or(value), MAX_SESSION_LABEL_CHARS),
            detail: detail.map(|d| clip(d, MAX_SESSION_LABEL_CHARS)),
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
            "{skipped} line(s) were skipped — their first field is not a usable session id, or              repeats one already listed"
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

    fn values(stdout: &str) -> Vec<String> {
        parse_sessions(stdout, false)
            .rows
            .into_iter()
            .map(|r| r.value)
            .collect()
    }

    #[test]
    fn bare_ids_need_no_adapter() {
        let out = parse_sessions("abc123\ndef456\n", false);
        assert_eq!(values("abc123\ndef456\n"), ["abc123", "def456"]);
        // With no label field, the value is its own label — a picker showing
        // blank rows would be worse than one showing ids.
        assert_eq!(out.rows[0].label, "abc123");
        assert_eq!(out.rows[0].detail, None);
        assert_eq!(out.note, None);
    }

    #[test]
    fn tabs_carry_the_label_and_the_detail() {
        let out = parse_sessions("abc123\t2h ago · the pane bug\t41 messages\n", false);
        assert_eq!(out.rows[0].label, "2h ago · the pane bug");
        assert_eq!(out.rows[0].detail.as_deref(), Some("41 messages"));
    }

    #[test]
    fn a_fourth_field_stays_in_the_detail() {
        // `splitn(3)` on purpose: a label written by a script that happens to
        // contain a tab must not silently become a phantom column.
        let out = parse_sessions("abc\tlabel\tone\ttwo\n", false);
        assert_eq!(out.rows[0].detail.as_deref(), Some("one\ttwo"));
    }

    #[test]
    fn one_bad_line_never_costs_the_others() {
        let out = parse_sessions("abc\n; rm -rf /\ndef\n", false);
        assert_eq!(
            out.rows.iter().map(|r| &r.value).collect::<Vec<_>>(),
            ["abc", "def"]
        );
        assert!(out.note.as_deref().unwrap().contains("1 line(s)"));
    }

    #[test]
    fn a_repeated_value_is_listed_once() {
        // The value is the row's identity all the way into React's key, so two
        // rows with one value is a duplicate-key warning and two rows nobody can
        // tell apart.
        let out = parse_sessions("abc\tfirst\ndef\nabc\tsecond\n", false);
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
        assert_eq!(parse_sessions("", false).rows, Vec::new());
        assert_eq!(parse_sessions("   \n\n", false).note, None);
    }

    #[test]
    fn a_truncated_payload_drops_only_its_partial_tail() {
        let out = parse_sessions("abc\ndef\nghi-half", true);
        assert_eq!(
            out.rows.iter().map(|r| &r.value).collect::<Vec<_>>(),
            ["abc", "def"]
        );
        assert!(out.note.as_deref().unwrap().contains("cut short"));
    }

    #[test]
    fn the_cap_is_velds_and_it_says_when_it_bit() {
        let stdout = (0..MAX_PANE_SESSIONS + 5)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = parse_sessions(&stdout, false);
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
    fn a_long_label_is_clipped_not_dropped() {
        let long = "x".repeat(MAX_SESSION_LABEL_CHARS + 40);
        let out = parse_sessions(&format!("abc\t{long}"), false);
        assert_eq!(out.rows[0].label.chars().count(), MAX_SESSION_LABEL_CHARS);
    }
}
