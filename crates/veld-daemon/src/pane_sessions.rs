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
//! second posture — [`super::extensions::spawn_command`]'s bounds (stdin closed,
//! no tty, process-group kill from a drop guard, capped output, `NO_COLOR`), and
//! the same machine-wide off switch, `extensions.autoRefresh`.
//!
//! Two things narrow it further than a badge:
//!
//! - It is not on a timer. It runs when the pane chooser is on screen and asks,
//!   and never otherwise.
//! - Declarations come from the **worktree's own** `veld.json`, with no
//!   `extensions.source` equivalent — because the command a pane runs already
//!   comes from there (`pty::resolve_pane` reads `root_config_in`), and a picker
//!   sourced from somewhere other than the pane it feeds would be two answers to
//!   one question.
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

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::time::Duration;

use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use serde::Serialize;
use veld_core::ide::{
    MAX_PANE_SESSIONS, MAX_SESSION_LABEL_CHARS, PaneBody, PaneDef, SessionsPicker,
};

use super::desktop::{ApiError, db_err, err, open_desktop_db};
use super::extensions::{clip, load_section, spawn_command, tail_suffix};
use super::pty::{missing_pane_binaries, worktree_builtins};

/// How long one pane's lister gets.
///
/// Shorter than a badge's 20s on purpose: a badge runs behind the user's back
/// and can afford to be slow, while this one is between the user and a pane they
/// are trying to open. A `ls | head` over a session directory is milliseconds; a
/// script that needs ten seconds to list what is on disk is a script whose author
/// wants to know.
const SESSIONS_TIMEOUT: Duration = Duration::from_secs(10);

/// One session a pane could adopt.
#[derive(Debug, Serialize, PartialEq)]
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
#[derive(Debug, Serialize, PartialEq)]
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
    let (root, branch, auto_refresh) = {
        let db = open_desktop_db()?;
        let wt = db
            .get_worktree(id)
            .map_err(db_err)?
            .ok_or_else(|| err(StatusCode::NOT_FOUND, "worktree not found"))?;
        (wt.path, wt.branch, db.extensions_auto_refresh())
    };

    let Some((config, section)) = load_section(&root) else {
        return Ok(Json(PaneSessionsResponse { panes: Vec::new() }));
    };

    let declared: Vec<(&PaneDef, &SessionsPicker)> = section
        .panes
        .iter()
        .filter_map(|pane| {
            let PaneBody::Terminal(terminal) = &pane.body;
            terminal.sessions.as_ref().map(|s| (pane, s))
        })
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
                    // Names the toggle as the settings screen labels it. A
                    // message pointing at a control the reader cannot find is
                    // worse than no message, and this one is the whole reason a
                    // picker they configured is not appearing.
                    message: Some(
                        "veld is not running project commands on this machine (Settings → \
                         General → Let projects run their own status commands)"
                            .to_owned(),
                    ),
                })
                .collect(),
        }));
    }

    let builtins = worktree_builtins(FsPath::new(&root), &branch, &config);

    // Concurrent, like the badge fan-out: these are independent child processes
    // and the screen waits for the slowest one either way. The count is already
    // bounded by how many panes a project declares.
    let runs = declared.into_iter().map(|(pane, picker)| {
        let builtins = builtins.clone();
        let root = root.clone();
        async move { run_one(pane, picker, &root, &builtins).await }
    });

    Ok(Json(PaneSessionsResponse {
        panes: futures_util::future::join_all(runs).await,
    }))
}

async fn run_one(
    pane: &PaneDef,
    picker: &SessionsPicker,
    root: &str,
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
        return base(
            "empty",
            Some(format!("{missing} is not installed on this machine")),
        );
    }

    // `root` twice: a pane's declarations always come from the worktree it runs
    // in, so the "declared here, ran there" split a badge can have does not exist
    // for this one. See the module docs.
    let out = match spawn_command(&picker.command, root, root, builtins, SESSIONS_TIMEOUT).await {
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
            "{skipped} line(s) were skipped because their first field is not a usable session id"
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
