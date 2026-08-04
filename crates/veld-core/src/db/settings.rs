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

/// The scope a setting is stored under. Only [`Scope::Global`] is written today;
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
    TerminalCopyOnSelect,
    TerminalMiddleClickPaste,
    TerminalDetachGrace,
    WorktreeMarkerStyle,
    BrowserQuickSwitchResponsive,
    BrowserQuickSwitchColorScheme,
    Unknown(String),
}

impl SettingKey {
    pub fn as_str(&self) -> &str {
        match self {
            Self::TerminalFontSize => "terminal.fontSize",
            Self::TerminalFontFamily => "terminal.fontFamily",
            Self::TerminalCursorStyle => "terminal.cursorStyle",
            Self::TerminalCursorBlink => "terminal.cursorBlink",
            Self::TerminalScrollback => "terminal.scrollback",
            Self::TerminalShiftEnterNewline => "terminal.shiftEnterNewline",
            Self::TerminalCopyOnSelect => "terminal.copyOnSelect",
            Self::TerminalMiddleClickPaste => "terminal.middleClickPaste",
            Self::TerminalDetachGrace => "terminal.detachGraceMinutes",
            Self::WorktreeMarkerStyle => "worktree.markerStyle",
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
            "terminal.copyOnSelect" => Self::TerminalCopyOnSelect,
            "terminal.middleClickPaste" => Self::TerminalMiddleClickPaste,
            "terminal.detachGraceMinutes" => Self::TerminalDetachGrace,
            "worktree.markerStyle" => Self::WorktreeMarkerStyle,
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
            Self::TerminalScrollback => Value::from(clamp_i64(value, 0, 500_000).ok_or_else(bad)?),
            Self::TerminalDetachGrace => Value::from(
                clamp_i64(value, MIN_DETACH_GRACE_MINUTES, MAX_DETACH_GRACE_MINUTES)
                    .ok_or_else(bad)?,
            ),
            Self::TerminalCursorBlink
            | Self::TerminalShiftEnterNewline
            | Self::TerminalCopyOnSelect
            | Self::TerminalMiddleClickPaste
            | Self::BrowserQuickSwitchResponsive
            | Self::BrowserQuickSwitchColorScheme => Value::from(value.as_bool().ok_or_else(bad)?),
            Self::TerminalCursorStyle => {
                one_of(value, &["block", "underline", "bar"]).ok_or_else(bad)?
            }
            Self::WorktreeMarkerStyle => one_of(value, &["color", "emoji"]).ok_or_else(bad)?,
            // A font family is free text; an empty string would render as the
            // browser default and read as a bug, so it falls back to the default.
            Self::TerminalFontFamily => {
                let s = value.as_str().ok_or_else(bad)?.trim();
                if s.is_empty() {
                    return Err(bad());
                }
                Value::from(s)
            }
            Self::Unknown(_) => value.clone(),
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
        (SettingKey::TerminalScrollback, Value::from(5000)),
        // Ships on: #197 established `ESC CR` as the default because it is what
        // Claude Code's `/terminal-setup` configures, so matching it means no
        // extra setup. The toggle exists for anyone whose TUI binds meta-Enter.
        (SettingKey::TerminalShiftEnterNewline, Value::from(true)),
        (SettingKey::TerminalCopyOnSelect, Value::from(false)),
        (SettingKey::TerminalMiddleClickPaste, Value::from(false)),
        (
            SettingKey::TerminalDetachGrace,
            Value::from(DEFAULT_DETACH_GRACE_MINUTES),
        ),
        // Colour is the new default marker; the emoji face stays stored, so this
        // is a rendering choice and switching back is lossless.
        (SettingKey::WorktreeMarkerStyle, Value::from("color")),
        (SettingKey::BrowserQuickSwitchResponsive, Value::from(true)),
        (SettingKey::BrowserQuickSwitchColorScheme, Value::from(true)),
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
                out.insert(key, value);
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

    /// Drop a setting, returning it to its default. Distinct from writing the
    /// default value: a stored default would survive a future change to
    /// [`defaults`], which is not what "reset" means.
    pub fn reset_setting(&self, key: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM settings WHERE scope = ?1 AND key = ?2",
            params![SCOPE_GLOBAL, key],
        )?;
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
    fn defaults_are_returned_when_nothing_is_stored() {
        let (_dir, db) = test_db();
        let s = db.settings().unwrap();
        assert_eq!(s["terminal.fontSize"], Value::from(12));
        assert_eq!(s["terminal.scrollback"], Value::from(5000));
        assert_eq!(s["worktree.markerStyle"], Value::from("color"));
        // Every declared key is present, so a client never has to supply one.
        assert_eq!(s.len(), defaults().len());
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
    fn reset_returns_the_default_rather_than_storing_it() {
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("terminal.fontSize", Value::from(20))]))
            .unwrap();
        db.reset_setting("terminal.fontSize").unwrap();
        assert_eq!(db.settings().unwrap()["terminal.fontSize"], Value::from(12));
        // Stored rows are gone, not overwritten with the current default — a
        // stored default would survive a later change to `defaults()`.
        let conn = db.lock();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
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
}
