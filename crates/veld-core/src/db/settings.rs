//! User preferences: the home for every behaviour that should be settable
//! rather than decided for the user.
//!
//! # Why this is untyped at rest and typed at the edges
//!
//! The `settings` table stores `(scope, key, value)` with `value` a JSON scalar.
//! Nothing in the schema knows what keys exist. Typing lives here, in
//! [`SettingKey`] and [`DEFAULTS`], for two reasons:
//!
//! 1. **A downgrade must not delete preferences.** `DbError::NewerSchema` exists
//!    because users move between builds, so an older daemon will be handed keys it
//!    has never heard of. An unknown key is stored and echoed back verbatim rather
//!    than rejected — the newer client's setting survives the round trip. Rejection
//!    is reserved for a *known* key whose value fails that key's validator.
//! 2. **Defaults have exactly one home: [`DEFAULTS`], in Rust.** [`Db::settings`]
//!    returns *effective* values — the defaults merged with whatever is stored — so
//!    the UI never hardcodes a default and there is no Rust↔TypeScript pair to keep
//!    in sync. The one remaining copy is documentation, and
//!    `documented_detach_grace_matches_the_default` pins it.
//!
//! # Which settings the daemon itself reads
//!
//! Most of these are pure UI rendering (cursor blink, marker style, which
//! quick-switch buttons appear) and the daemon only stores bytes for the client.
//! [`SettingKey::TerminalDetachGrace`] is the exception — the daemon enforces that
//! timer itself — which is why it is validated and clamped here rather than trusted
//! from the wire. That distinction is worth keeping in mind when adding a key: a
//! value the daemon acts on needs a server-side validator, a value only the UI
//! reads does not.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use super::{Db, DbError, now_str};

/// The scope a setting is stored under. Only [`SCOPE_GLOBAL`] is written today;
/// the column exists so per-project overrides do not need a migration later.
pub const SCOPE_GLOBAL: &str = "global";

/// Terminal sessions are reaped this long after their last client detaches.
///
/// Quoted in `README.md` and `website/llms-full.txt`. Those copies are pinned by
/// `documented_detach_grace_matches_the_default` — if you change this, that test
/// tells you which files to update rather than letting the docs drift.
pub const DEFAULT_DETACH_GRACE_MINUTES: i64 = 30;

/// Lower bound on the detach grace. Below a minute, a page reload that takes
/// longer than the grace would reap the shell it is reconnecting to — which is the
/// exact loss the holder-process design exists to prevent.
pub const MIN_DETACH_GRACE_MINUTES: i64 = 1;
/// Upper bound. A week of holder processes for terminals nobody returned to is
/// already generous; unbounded means a leak with a preference in front of it.
pub const MAX_DETACH_GRACE_MINUTES: i64 = 10_080;

/// Lines of scrollback each terminal keeps.
///
/// xterm stores a cell as **3 × u32 = 12 bytes** (`new Uint32Array(3 * cols)` per
/// line), so a line costs `12 × cols` — about 1.4 KB at 120 columns. 10 000 lines is
/// therefore ~14 MB per terminal, and a handful of open terminals is tens of MB,
/// which is the right trade for a desktop app whose whole point is long-running
/// builds. For comparison: Alacritty defaults to 10 000, Windows Terminal to 9 001,
/// GNOME Terminal to 8 192, VS Code and iTerm2 to 1 000, tmux to 2 000.
///
/// Note the asymmetry this does **not** fix: the daemon's replay buffer is
/// `SCROLLBACK_BYTES` (256 KiB, `veld-daemon/src/pty.rs`), so a page reload restores
/// only the last few thousand lines of output regardless of this number. Scrollback
/// above that is history for the life of the page, not across a reload.
pub const DEFAULT_SCROLLBACK: i64 = 10_000;

/// Upper bound on scrollback.
///
/// 100 000 lines is ~144 MB per terminal at 120 columns — generous past any real
/// use and still short of wedging the renderer. The first version allowed 500 000
/// (~720 MB for one terminal), which is not a preference, it is a way to run out of
/// memory with a number in a box.
pub const MAX_SCROLLBACK: i64 = 100_000;

/// Days a worktree stays in the trash before it is deleted for good.
///
/// **Zero means "keep until I empty it", and that is the default.** The trash is a
/// holding area: moving a worktree there deletes nothing, and this is the only thing
/// that ever deletes one without the user asking again. A default of zero means the
/// only destructive act veld performs unprompted is one the user opted into by
/// setting a number.
pub const DEFAULT_TRASH_RETENTION_DAYS: i64 = 0;

/// Shortest retention that is still a grace period. Below a day, "trash" would not
/// give anyone time to notice they had binned the wrong checkout.
pub const MIN_TRASH_RETENTION_DAYS: i64 = 1;
/// Longest retention. Past a year the trash is not a holding area, it is a disk
/// full of checkouts nobody remembers binning.
pub const MAX_TRASH_RETENTION_DAYS: i64 = 365;

/// Days of ended runs the history views show, or `0` for "show all".
///
/// A **view** horizon, not a retention policy: nothing is deleted by it, and it is
/// deliberately not wired to the GC. Someone who starts forty runs a day wants a
/// shorter list, not a shorter history.
///
/// **Defaults to three days rather than to "show all"**, which is the one place this
/// key differs in spirit from [`DEFAULT_TRASH_RETENTION_DAYS`] above: hiding a run
/// costs a settings change to undo, while deleting a checkout costs the checkout, so
/// the cautious default is only mandatory for the destructive setting. Three days
/// covers "what did I run yesterday" and keeps the History tab short for someone who
/// starts dozens of runs a day, and the runs are still there — the horizon says what
/// is *shown*, and the view says how many it hid.
pub const DEFAULT_RUN_HISTORY_DAYS: i64 = 3;

/// Longest run-history horizon that can show anything, in days.
///
/// **Bounded by the GC, not by taste.** The housekeeping pass already deletes ended
/// runs older than `MAX_LOG_AGE_HOURS` (`veld-daemon/src/gc.rs`, 168h), so a horizon
/// past that filters against runs that no longer exist and would read as broken —
/// "show me 30 days" then showing 7. `run_history_horizon_matches_the_gc_window` in
/// that module fails if the two ever drift.
pub const MAX_RUN_HISTORY_DAYS: i64 = 7;

/// Longest accepted `terminal.fontFamily`. A CSS font-family list that needs more
/// than this is not a font list.
const MAX_FONT_FAMILY_LEN: usize = 200;

/// Bounds on a key this binary does not recognise.
///
/// Unknown keys are preserved rather than rejected (see the module docs), which
/// makes them the one unbounded thing a client can put in this table. Preserving a
/// newer build's preference does not require accepting an arbitrary blob: the whole
/// document is returned by `GET` to every client on every window focus and mirrored
/// into `localStorage`, so an unbounded write is a cost every future read pays.
const MAX_UNKNOWN_KEY_LEN: usize = 128;
const MAX_UNKNOWN_VALUE_LEN: usize = 4096;

/// Every setting this binary understands.
///
/// `Unknown` is not an error case — see the module docs. It is how a preference
/// written by a newer client survives being read and rewritten by an older one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingKey {
    TerminalFontSize,
    TerminalFontFamily,
    TerminalCursorStyle,
    TerminalCursorBlink,
    TerminalScrollback,
    TerminalShiftEnterNewline,
    TerminalDetachGrace,
    WorktreeMarkerStyle,
    WorktreeTrashRetention,
    RunsHistoryDays,
    BrowserQuickSwitchResponsive,
    BrowserQuickSwitchColorScheme,
    Unknown(String),
}

impl SettingKey {
    /// Every known key.
    ///
    /// Exists so tests can enumerate the variants, which is the only thing that
    /// catches the two silent misses when a key is added: [`Self::parse`] ends in an
    /// `other => Unknown` catch-all, so a forgotten arm makes the key *unvalidated*
    /// rather than a compile error — and a forgotten [`defaults`] entry makes the
    /// effective document incomplete, at which point TypeScript's `FALLBACK`
    /// silently becomes the real default, the exact Rust↔TS drift this module's docs
    /// claim is impossible. `as_str` and `validate` are exhaustive matches and need
    /// no help; these two do.
    pub const ALL: &'static [SettingKey] = &[
        Self::TerminalFontSize,
        Self::TerminalFontFamily,
        Self::TerminalCursorStyle,
        Self::TerminalCursorBlink,
        Self::TerminalScrollback,
        Self::TerminalShiftEnterNewline,
        Self::TerminalDetachGrace,
        Self::WorktreeMarkerStyle,
        Self::WorktreeTrashRetention,
        Self::RunsHistoryDays,
        Self::BrowserQuickSwitchResponsive,
        Self::BrowserQuickSwitchColorScheme,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            Self::TerminalFontSize => "terminal.fontSize",
            Self::TerminalFontFamily => "terminal.fontFamily",
            Self::TerminalCursorStyle => "terminal.cursorStyle",
            Self::TerminalCursorBlink => "terminal.cursorBlink",
            Self::TerminalScrollback => "terminal.scrollback",
            Self::TerminalShiftEnterNewline => "terminal.shiftEnterNewline",
            Self::TerminalDetachGrace => "terminal.detachGraceMinutes",
            Self::WorktreeMarkerStyle => "worktree.markerStyle",
            Self::WorktreeTrashRetention => "worktree.trashRetentionDays",
            Self::RunsHistoryDays => "runs.historyDays",
            Self::BrowserQuickSwitchResponsive => "browser.quickSwitch.responsive",
            Self::BrowserQuickSwitchColorScheme => "browser.quickSwitch.colorScheme",
            Self::Unknown(k) => k,
        }
    }

    pub fn parse(key: &str) -> Self {
        match key {
            "terminal.fontSize" => Self::TerminalFontSize,
            "terminal.fontFamily" => Self::TerminalFontFamily,
            "terminal.cursorStyle" => Self::TerminalCursorStyle,
            "terminal.cursorBlink" => Self::TerminalCursorBlink,
            "terminal.scrollback" => Self::TerminalScrollback,
            "terminal.shiftEnterNewline" => Self::TerminalShiftEnterNewline,
            "terminal.detachGraceMinutes" => Self::TerminalDetachGrace,
            "worktree.markerStyle" => Self::WorktreeMarkerStyle,
            "worktree.trashRetentionDays" => Self::WorktreeTrashRetention,
            "runs.historyDays" => Self::RunsHistoryDays,
            "browser.quickSwitch.responsive" => Self::BrowserQuickSwitchResponsive,
            "browser.quickSwitch.colorScheme" => Self::BrowserQuickSwitchColorScheme,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// Validate and normalise a value for this key.
    ///
    /// Numbers are **clamped, not rejected**, because the only producer is our own
    /// UI and a slider that reports "invalid" is worse than one that stops at its
    /// end. Enums *are* rejected: an unrecognised cursor style has no sensible
    /// nearest value, and silently substituting one would make the UI and the store
    /// disagree about what was saved.
    ///
    /// An [`Unknown`](Self::Unknown) key accepts anything — it is being preserved,
    /// not interpreted.
    fn validate(&self, value: &Value) -> Result<Value, DbError> {
        let bad = || DbError::InvalidSetting {
            key: self.as_str().to_string(),
            value: value.to_string(),
        };
        Ok(match self {
            Self::TerminalFontSize => Value::from(clamp_i64(value, 6, 72).ok_or_else(bad)?),
            Self::TerminalScrollback => {
                Value::from(clamp_i64(value, 0, MAX_SCROLLBACK).ok_or_else(bad)?)
            }
            Self::TerminalDetachGrace => Value::from(
                clamp_i64(value, MIN_DETACH_GRACE_MINUTES, MAX_DETACH_GRACE_MINUTES)
                    .ok_or_else(bad)?,
            ),
            // Zero is "keep until emptied" and is deliberately outside the clamped
            // range, because this is the one numeric setting whose off state is not a
            // small value — clamping it up to `MIN` would arm a timer that deletes
            // checkouts for a user who was trying to turn it off.
            Self::WorktreeTrashRetention => {
                let n = clamp_i64(value, 0, MAX_TRASH_RETENTION_DAYS).ok_or_else(bad)?;
                Value::from(if n == 0 {
                    0
                } else {
                    n.max(MIN_TRASH_RETENTION_DAYS)
                })
            }
            // Zero is "show everything" — the same off-switch shape as the trash
            // retention above, and for the same reason: the value a user picks to
            // turn a filter off must not be clamped into turning it on.
            Self::RunsHistoryDays => {
                Value::from(clamp_i64(value, 0, MAX_RUN_HISTORY_DAYS).ok_or_else(bad)?)
            }
            Self::TerminalCursorBlink
            | Self::TerminalShiftEnterNewline
            | Self::BrowserQuickSwitchResponsive
            | Self::BrowserQuickSwitchColorScheme => Value::from(value.as_bool().ok_or_else(bad)?),
            Self::TerminalCursorStyle => {
                one_of(value, &["block", "underline", "bar"]).ok_or_else(bad)?
            }
            Self::WorktreeMarkerStyle => one_of(value, &["color", "emoji"]).ok_or_else(bad)?,
            // A font family is free text, but **not** free-form: xterm's DOM
            // renderer interpolates it into a CSS *rule* —
            // `font-family: ${rawOptions.fontFamily};` inside a stylesheet's
            // textContent — so a `}` closes the rule and everything after it is
            // appended as arbitrary CSS to every `/ide` window, persistently,
            // because it is stored. The daemon is reachable same-origin from a
            // developer's own app through the helper's `/__veld__` proxy, so
            // "only our own UI writes this" is not true.
            //
            // Bounded rather than escaped: a font-family list needs none of these
            // characters, and a validator that tries to neutralise CSS is a
            // validator that will be wrong eventually.
            Self::TerminalFontFamily => {
                let s = value.as_str().ok_or_else(bad)?.trim();
                if s.is_empty()
                    || s.len() > MAX_FONT_FAMILY_LEN
                    || s.contains(['{', '}', ';', '<', '>', '\n', '\r'])
                {
                    return Err(bad());
                }
                Value::from(s)
            }
            // Preserved, but bounded — see MAX_UNKNOWN_* above.
            Self::Unknown(k) => {
                if k.len() > MAX_UNKNOWN_KEY_LEN
                    || serde_json::to_string(value)?.len() > MAX_UNKNOWN_VALUE_LEN
                {
                    return Err(bad());
                }
                value.clone()
            }
        })
    }
}

fn clamp_i64(value: &Value, lo: i64, hi: i64) -> Option<i64> {
    // `as_i64` rejects a float, and the JSON a browser sends for `14` may well be
    // `14.0` — so accept either and round, rather than 422ing on a value the user
    // cannot see or influence.
    let n = value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f.round() as i64))?;
    Some(n.clamp(lo, hi))
}

fn one_of(value: &Value, allowed: &[&str]) -> Option<Value> {
    let s = value.as_str()?;
    allowed.contains(&s).then(|| Value::from(s))
}

/// The one source of truth for every default.
///
/// [`Db::settings`] merges these under whatever is stored, so a client always
/// receives a complete document and never needs a default of its own.
pub fn defaults() -> BTreeMap<String, Value> {
    [
        (SettingKey::TerminalFontSize, Value::from(12)),
        (
            SettingKey::TerminalFontFamily,
            Value::from("\"JetBrains Mono Variable\", \"JetBrains Mono\", ui-monospace, monospace"),
        ),
        (SettingKey::TerminalCursorStyle, Value::from("block")),
        (SettingKey::TerminalCursorBlink, Value::from(true)),
        (
            SettingKey::TerminalScrollback,
            Value::from(DEFAULT_SCROLLBACK),
        ),
        // Ships on: #197 established `ESC CR` as the default because it is what
        // Claude Code's `/terminal-setup` configures, so matching it means no
        // extra setup. The toggle exists for anyone whose TUI binds meta-Enter.
        (SettingKey::TerminalShiftEnterNewline, Value::from(true)),
        (
            SettingKey::TerminalDetachGrace,
            Value::from(DEFAULT_DETACH_GRACE_MINUTES),
        ),
        // Colour is the new default marker; the emoji face stays stored, so this
        // is a rendering choice and switching back is lossless.
        (SettingKey::WorktreeMarkerStyle, Value::from("color")),
        // Both quick switches ship **on**. Whether two more buttons belong in a
        // pane's chrome for everyone is a real question — the bar already carries
        // most of a browser's toolbar and has to read at 300px — but the alternative is
        // worse: a control defaulted off is a control nobody finds, and the whole
        // point of these two is reach. The responsive viewport and the page's colour
        // scheme are three levels deep in the device menu and are changed dozens of
        // times an hour while working on a layout.
        //
        // Note what this preference is **not**: it is one global document, so turning
        // a switch off hides it in every pane, every window and every browser tab on
        // this daemon. It cannot answer the 300px case on its own — pane width is
        // per pane and changes on every split — and it is a standing choice about
        // whether you want the shortcut at all. Hiding the switches below a measured
        // bar width would answer the narrow-pane case directly and needs no key; it
        // is the better answer to that specific problem and is deliberately not what
        // this is. Each key says whether the *switch is shown*; what a pane is
        // emulating lives in that pane's layout.
        (SettingKey::BrowserQuickSwitchResponsive, Value::from(true)),
        (SettingKey::BrowserQuickSwitchColorScheme, Value::from(true)),
        // Keep until emptied. The trash deleting things on its own is opt-in, and
        // the default has to be the one that cannot surprise anybody.
        (
            SettingKey::WorktreeTrashRetention,
            Value::from(DEFAULT_TRASH_RETENTION_DAYS),
        ),
        // Three days — see the constant. The view reports how many it hid, so a
        // default that hides something cannot be mistaken for a run having vanished.
        (
            SettingKey::RunsHistoryDays,
            Value::from(DEFAULT_RUN_HISTORY_DAYS),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.as_str().to_string(), v))
    .collect()
}

impl Db {
    /// Every setting's **effective** value: [`defaults`] with the stored rows
    /// merged over the top, plus any unknown stored key preserved as-is.
    pub fn settings(&self) -> Result<BTreeMap<String, Value>, DbError> {
        let mut out = defaults();
        let conn = self.lock();
        let mut stmt = conn.prepare_cached("SELECT key, value FROM settings WHERE scope = ?1")?;
        let rows = stmt.query_map([SCOPE_GLOBAL], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (key, raw) = row?;
            // A corrupt payload degrades to the default instead of failing the
            // whole request — the same posture every other JSON-in-a-column read
            // in this module takes.
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                // Re-validated on the way out, not only on the way in. The
                // downgrade case this store is built around is a *newer* build
                // having written a value with a wider range — validating only on
                // write would hand that straight to a client. A value that fails
                // now degrades to the default, the same posture the corrupt-JSON
                // branch above takes.
                if let Ok(clean) = SettingKey::parse(&key).validate(&value) {
                    out.insert(key, clean);
                }
            }
        }
        Ok(out)
    }

    /// One setting's effective value, without materialising the whole document.
    pub fn setting(&self, key: &SettingKey) -> Result<Option<Value>, DbError> {
        let conn = self.lock();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE scope = ?1 AND key = ?2",
                params![SCOPE_GLOBAL, key.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        drop(conn);
        if let Some(v) = raw.as_deref().and_then(|s| serde_json::from_str(s).ok()) {
            return Ok(Some(v));
        }
        Ok(defaults().get(key.as_str()).cloned())
    }

    /// Apply a **patch**: only the keys present are written, each validated
    /// against its own rules.
    ///
    /// Every key is validated *before* anything is written, so a rejected value
    /// cannot leave a half-applied patch behind — the same both-or-neither rule
    /// `patch_worktree` follows for alias and emoji. The write runs in one
    /// `BEGIN IMMEDIATE` transaction because `Db::open` is per-request: two
    /// concurrent handlers in one daemon race exactly like two processes, and the
    /// connection mutex does not span them.
    ///
    /// No cross-key atomicity is offered beyond that, and none is needed: the grain
    /// of the write is the grain of the conflict, so two clients patching different
    /// keys both win and a same-key collision is last-write-wins.
    pub fn patch_settings(&self, patch: &BTreeMap<String, Value>) -> Result<(), DbError> {
        let validated: Vec<(String, String)> = patch
            .iter()
            .map(|(key, value)| {
                let parsed = SettingKey::parse(key);
                let value = parsed.validate(value)?;
                Ok((key.clone(), serde_json::to_string(&value)?))
            })
            .collect::<Result<_, DbError>>()?;

        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = now_str();
        for (key, value) in &validated {
            tx.execute(
                "INSERT INTO settings (scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(scope, key) DO UPDATE SET
                     value = excluded.value, updated_at = excluded.updated_at",
                params![SCOPE_GLOBAL, key, value, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The detach grace the daemon should enforce, as a `Duration`.
    ///
    /// The daemon reads this one itself, so it goes through the clamp rather than
    /// trusting the stored number: a row written by a newer build with a wider
    /// range must not be able to disable reaping here.
    pub fn detach_grace(&self) -> std::time::Duration {
        let minutes = self
            .setting(&SettingKey::TerminalDetachGrace)
            .ok()
            .flatten()
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_DETACH_GRACE_MINUTES)
            .clamp(MIN_DETACH_GRACE_MINUTES, MAX_DETACH_GRACE_MINUTES);
        std::time::Duration::from_secs(minutes as u64 * 60)
    }

    /// How long a worktree stays in the trash before it is deleted, or `None` for
    /// "keep until emptied" — the default, and what this returns for anything it
    /// cannot read.
    ///
    /// Re-read per GC pass rather than cached, so turning automatic deletion off
    /// takes effect at the next pass instead of at the next daemon restart. Turning
    /// *off* the one thing that deletes a checkout unprompted must not need a
    /// restart.
    pub fn trash_retention(&self) -> Option<std::time::Duration> {
        let days = self
            .setting(&SettingKey::WorktreeTrashRetention)
            .ok()
            .flatten()
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_TRASH_RETENTION_DAYS);
        if days <= 0 {
            return None;
        }
        let days = days.clamp(MIN_TRASH_RETENTION_DAYS, MAX_TRASH_RETENTION_DAYS);
        Some(std::time::Duration::from_secs(days as u64 * 86_400))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    fn patch(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn every_known_key_round_trips_and_has_a_default() {
        // The guard for the two ways adding a key goes quietly wrong: `parse`'s
        // catch-all turning a typo'd arm into an unvalidated `Unknown`, and a
        // missing `defaults` entry handing the client an incomplete document.
        for key in SettingKey::ALL {
            assert_eq!(
                &SettingKey::parse(key.as_str()),
                key,
                "{:?} has no `parse` arm — it would be stored unvalidated",
                key
            );
            assert!(
                defaults().contains_key(key.as_str()),
                "{} has no default — the effective document would be incomplete",
                key.as_str()
            );
        }
        // And nothing in `defaults` that `parse` does not know, which would be a
        // default for a key no validator covers.
        for key in defaults().keys() {
            assert!(
                !matches!(SettingKey::parse(key), SettingKey::Unknown(_)),
                "{key} is defaulted but unknown to `parse`"
            );
        }
        assert_eq!(defaults().len(), SettingKey::ALL.len());
    }

    #[test]
    fn defaults_are_returned_when_nothing_is_stored() {
        let (_dir, db) = test_db();
        let s = db.settings().unwrap();
        assert_eq!(s["terminal.fontSize"], Value::from(12));
        assert_eq!(s["terminal.scrollback"], Value::from(DEFAULT_SCROLLBACK));
        assert_eq!(s["worktree.markerStyle"], Value::from("color"));
        // Every declared key is present, so a client never has to supply one.
        // Counted against the variant list rather than against `defaults()` itself,
        // which would compare a value with itself.
        assert_eq!(s.len(), SettingKey::ALL.len());
    }

    #[test]
    fn patch_writes_only_the_keys_it_carries() {
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("terminal.fontSize", Value::from(15))]))
            .unwrap();
        let s = db.settings().unwrap();
        assert_eq!(s["terminal.fontSize"], Value::from(15));
        // Untouched keys keep their defaults rather than being cleared by a
        // whole-document write.
        assert_eq!(s["terminal.cursorStyle"], Value::from("block"));
    }

    #[test]
    fn numbers_clamp_and_enums_reject() {
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("terminal.fontSize", Value::from(9999))]))
            .unwrap();
        assert_eq!(db.settings().unwrap()["terminal.fontSize"], Value::from(72));

        let err = db
            .patch_settings(&patch(&[("terminal.cursorStyle", Value::from("wobble"))]))
            .unwrap_err();
        assert!(matches!(err, DbError::InvalidSetting { .. }));
    }

    #[test]
    fn scrollback_clamps_at_the_maximum() {
        // The bound exists so a number in a box cannot wedge the renderer:
        // 100_000 lines is already ~144 MB for one terminal at 120 columns.
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("terminal.scrollback", Value::from(10_000_000))]))
            .unwrap();
        assert_eq!(
            db.settings().unwrap()["terminal.scrollback"],
            Value::from(MAX_SCROLLBACK)
        );
        // Zero is legal — "no scrollback" is a real preference, not an error.
        db.patch_settings(&patch(&[("terminal.scrollback", Value::from(0))]))
            .unwrap();
        assert_eq!(
            db.settings().unwrap()["terminal.scrollback"],
            Value::from(0)
        );
    }

    #[test]
    fn a_float_font_size_is_accepted() {
        // JSON has one number type; a browser may well send `14.0`. Rejecting it
        // would 422 on a value the user cannot see or influence.
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("terminal.fontSize", Value::from(14.0))]))
            .unwrap();
        assert_eq!(db.settings().unwrap()["terminal.fontSize"], Value::from(14));
    }

    #[test]
    fn a_rejected_key_leaves_the_whole_patch_unapplied() {
        let (_dir, db) = test_db();
        let err = db
            .patch_settings(&patch(&[
                ("terminal.fontSize", Value::from(15)),
                ("terminal.cursorStyle", Value::from("wobble")),
            ]))
            .unwrap_err();
        assert!(matches!(err, DbError::InvalidSetting { .. }));
        // Both-or-neither: the valid half of a rejected patch must not survive.
        assert_eq!(db.settings().unwrap()["terminal.fontSize"], Value::from(12));
    }

    #[test]
    fn a_quick_switch_takes_a_bool_and_nothing_else() {
        // The only server-side behaviour these two keys have. Rejected rather than
        // coerced: `Value::from(truthy)` would store a `1` that the UI's `bool`
        // reader then ignores in favour of the fallback, so the daemon would report
        // a saved setting the app does not honour.
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[(
            "browser.quickSwitch.colorScheme",
            Value::from(false),
        )]))
        .unwrap();
        assert_eq!(
            db.settings().unwrap()["browser.quickSwitch.colorScheme"],
            Value::from(false)
        );
        let err = db
            .patch_settings(&patch(&[(
                "browser.quickSwitch.responsive",
                Value::from("yes"),
            )]))
            .unwrap_err();
        assert!(matches!(err, DbError::InvalidSetting { .. }));
    }

    #[test]
    fn an_unknown_key_round_trips_instead_of_being_rejected() {
        // The downgrade case: a newer client wrote a key this binary has never
        // heard of. It must survive being read and rewritten by us, not be
        // deleted by us.
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[(
            "terminal.futureThing",
            Value::from("from-a-newer-build"),
        )]))
        .unwrap();
        let s = db.settings().unwrap();
        assert_eq!(s["terminal.futureThing"], Value::from("from-a-newer-build"));
    }

    #[test]
    fn a_corrupt_stored_value_degrades_to_the_default() {
        let (_dir, db) = test_db();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO settings (scope, key, value, updated_at)
                 VALUES ('global', 'terminal.fontSize', 'not json', '2026-01-01T00:00:00.000000Z')",
                [],
            )
            .unwrap();
        }
        // One unreadable row must not fail the whole document.
        assert_eq!(db.settings().unwrap()["terminal.fontSize"], Value::from(12));
    }

    #[test]
    fn detach_grace_is_clamped_even_when_stored_out_of_range() {
        let (_dir, db) = test_db();
        assert_eq!(
            db.detach_grace(),
            std::time::Duration::from_secs(DEFAULT_DETACH_GRACE_MINUTES as u64 * 60)
        );
        {
            // Written directly, bypassing the accessor's clamp — the shape a
            // newer build with a wider range would leave behind.
            let conn = db.lock();
            conn.execute(
                "INSERT INTO settings (scope, key, value, updated_at)
                 VALUES ('global', 'terminal.detachGraceMinutes', '999999', '2026-01-01T00:00:00.000000Z')",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            db.detach_grace(),
            std::time::Duration::from_secs(MAX_DETACH_GRACE_MINUTES as u64 * 60)
        );
    }

    #[test]
    fn trash_retention_defaults_to_keep_forever_and_zero_survives_the_clamp() {
        let (_dir, db) = test_db();
        assert_eq!(
            db.trash_retention(),
            None,
            "the only thing that deletes a checkout unprompted must be opt-in"
        );

        // Zero means "keep until emptied" and is deliberately outside the clamped
        // range. Clamping it up to MIN would arm automatic deletion for a user
        // trying to turn it off — the one numeric setting where the nearest legal
        // value is the wrong answer.
        db.patch_settings(&patch(&[("worktree.trashRetentionDays", Value::from(0))]))
            .unwrap();
        assert_eq!(db.trash_retention(), None);

        db.patch_settings(&patch(&[("worktree.trashRetentionDays", Value::from(14))]))
            .unwrap();
        assert_eq!(
            db.trash_retention(),
            Some(std::time::Duration::from_secs(14 * 86_400))
        );

        // Above the ceiling clamps down, as every other number does.
        db.patch_settings(&patch(&[(
            "worktree.trashRetentionDays",
            Value::from(MAX_TRASH_RETENTION_DAYS + 500),
        )]))
        .unwrap();
        assert_eq!(
            db.settings().unwrap().get("worktree.trashRetentionDays"),
            Some(&Value::from(MAX_TRASH_RETENTION_DAYS))
        );
    }

    #[test]
    fn run_history_days_defaults_to_all_and_clamps_to_the_gc_window() {
        let (_dir, db) = test_db();
        assert_eq!(
            db.settings().unwrap().get("runs.historyDays"),
            Some(&Value::from(DEFAULT_RUN_HISTORY_DAYS)),
            "a fresh install shows the last few days of history"
        );

        // Zero is "show everything", so it must survive the clamp for the same reason
        // the trash retention's does: it is the off switch, not the minimum.
        db.patch_settings(&patch(&[("runs.historyDays", Value::from(0))]))
            .unwrap();
        assert_eq!(
            db.settings().unwrap().get("runs.historyDays"),
            Some(&Value::from(0))
        );

        // Past the ceiling clamps down to it. A horizon longer than the GC window
        // would promise days of history that has already been deleted.
        db.patch_settings(&patch(&[(
            "runs.historyDays",
            Value::from(MAX_RUN_HISTORY_DAYS + 30),
        )]))
        .unwrap();
        assert_eq!(
            db.settings().unwrap().get("runs.historyDays"),
            Some(&Value::from(MAX_RUN_HISTORY_DAYS))
        );

        db.patch_settings(&patch(&[("runs.historyDays", Value::from(2))]))
            .unwrap();
        assert_eq!(
            db.settings().unwrap().get("runs.historyDays"),
            Some(&Value::from(2))
        );
    }

    #[test]
    fn documented_detach_grace_matches_the_default() {
        // The grace is quoted in prose in two tracked files. This is the drift
        // gate: change the constant and this test names the files to update,
        // rather than letting the docs quietly describe the old behaviour.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let expected = format!("{DEFAULT_DETACH_GRACE_MINUTES} minutes");
        for rel in ["README.md", "website/llms-full.txt"] {
            let path = root.join(rel);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            assert!(
                text.contains(&expected),
                "{rel} does not mention the detach grace as {expected:?} — update it \
                 alongside DEFAULT_DETACH_GRACE_MINUTES"
            );
        }
    }

    #[test]
    fn documented_run_history_horizon_matches_the_constants() {
        // Same drift gate as the detach grace above, for the same reason: both numbers
        // are quoted as *prose* in tracked files, and prose is the one place a constant
        // can change without anything failing. `README.md` explains the 7-day ceiling by
        // naming the housekeeping window it comes from, so a future bump to either
        // constant has to update the sentence that justifies it.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        // The needle is the *number with its unit*, not a whole sentence: pinning
        // phrasing would make every copy-edit a test failure, which is how a drift gate
        // gets deleted instead of maintained.
        let max = format!("{MAX_RUN_HISTORY_DAYS} days");
        let default = format!("{DEFAULT_RUN_HISTORY_DAYS} days");
        // The settings dialog's `max=` is the same class of copy as the prose: a literal
        // in another language that silently disagrees once this constant moves. Its own
        // comment claimed a test tied the two, and none did — a false claim of a gate is
        // worse than an admitted gap, so here is the gate.
        let dialog_max = format!("max={{{MAX_RUN_HISTORY_DAYS}}}");
        for (rel, needle) in [
            ("README.md", &max),
            ("README.md", &default),
            ("website/llms-full.txt", &max),
            ("website/llms-full.txt", &default),
            (
                "crates/veld-daemon/ui/src/components/SettingsDialog.tsx",
                &dialog_max,
            ),
        ] {
            let path = root.join(rel);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            assert!(
                text.contains(needle.as_str()),
                "{rel} does not contain {needle:?} — update it alongside \
                 DEFAULT_RUN_HISTORY_DAYS / MAX_RUN_HISTORY_DAYS"
            );
        }
    }
}
