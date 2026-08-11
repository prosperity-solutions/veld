//! User preferences: the home for every behaviour that should be settable
//! rather than decided for the user.
//!
//! # Why this is untyped at rest and typed at the edges
//!
//! The `settings` table stores `(scope, key, value)` with `value` a JSON document
//! — a scalar for all but one key ([`SettingKey::BrowserExternalOrigins`] holds an
//! array of origin patterns, because a list of hosts is what that preference *is*;
//! a delimited string would be the same data with a parser bolted on).
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

/// Where a new worktree's branch is cut from — the `git.createFrom` setting.
///
/// `Origin` fetches the remote and bases the new branch on `origin/<default>`,
/// so a worktree is never born behind the remote. `Local` uses the main
/// checkout's current HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitCreateSource {
    Origin,
    Local,
}

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

/// How many times a terminal whose socket dropped reconnects to the same shell
/// by itself before giving up and waiting for a click.
///
/// The shell survives the pipe (that is what the holder process and the detach
/// grace exist for), so a dropped socket is usually a transient network blip —
/// the machine slept, a proxy timed out, the daemon restarted mid-`veld update`
/// — and a few automatic attempts reconnect without the user noticing. After the
/// budget is spent the terminal settles in its `error` state and offers the
/// manual Reconnect button, so this is a courtesy that can never wedge a shell
/// behind an unreachable daemon.
///
/// **Zero disables auto-reconnect** — the previous release's behaviour, where a
/// dropped socket always waited for a click. The off switch must not be clamped
/// to a retry: the whole point of the setting is that some people would rather
/// decide themselves.
pub const DEFAULT_RECONNECT_TRIES: i64 = 3;

/// Lower bound on the reconnect budget. Zero is the off switch (see
/// [`DEFAULT_RECONNECT_TRIES`]); one is "try once, then stop".
pub const MIN_RECONNECT_TRIES: i64 = 0;
/// Upper bound on the reconnect budget.
///
/// Each attempt is a WebSocket handshake and, on the first one, a reattach; past
/// a couple of dozen the session is effectively unreachable and every further
/// attempt is just a heartbeat at the daemon it cannot reach. A cap keeps a
/// mis-set preference from turning a dead pipe into a permanent reconnect storm.
pub const MAX_RECONNECT_TRIES: i64 = 20;

/// Seconds between auto-reconnect attempts **after the first**.
///
/// The first attempt is the one that fixes the common cases (a sleep, a dropped
/// proxy) and is deliberately near-immediate (see
/// [`DEFAULT_RECONNECT_FIRST_DELAY_SECONDS`]); the backoff is what a still-failing
/// session waits between attempts so it is not hammering a daemon that is itself
/// restarting.
pub const DEFAULT_RECONNECT_BACKOFF_SECONDS: i64 = 5;

/// Lower bound on the reconnect backoff. Sub-second spacing between reconnect
/// attempts is a reconnect storm even at the default budget.
pub const MIN_RECONNECT_BACKOFF_SECONDS: i64 = 1;
/// Upper bound on the reconnect backoff. Past a few minutes the session is not
/// coming back on a timescale the backoff can span usefully; the manual Reconnect
/// button is the honest answer there.
pub const MAX_RECONNECT_BACKOFF_SECONDS: i64 = 300;

/// Seconds before the **first** auto-reconnect attempt fires after a socket drops.
///
/// This is the "nearly immediately" of the feature: the first reconnect should
/// feel automatic, not like the session is gone. It is a setting rather than a
/// constant because "nearly" is a preference — a flaky network might want the
/// first attempt a little later rather than racing every blip.
pub const DEFAULT_RECONNECT_FIRST_DELAY_SECONDS: i64 = 1;

/// Lower bound on the first reconnect delay.
pub const MIN_RECONNECT_FIRST_DELAY_SECONDS: i64 = 1;
/// Upper bound on the first reconnect delay.
///
/// The first reconnect is the "is it really gone?" probe, and past half a minute
/// it stops feeling automatic and starts feeling like the session is unreachable
/// — which is what the manual button is for. The backoff setting below covers
/// longer waits between the attempts that follow.
pub const MAX_RECONNECT_FIRST_DELAY_SECONDS: i64 = 30;

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

/// Terminal bell volume, as a percentage 0–100.
///
/// Scales the Web-Audio tone a `BEL` plays. A percentage rather than a linear
/// amplitude because that is what a slider is: 0 is silent, 100 is the loudest
/// this build will play. `playBell` in the UI maps it onto a gain.
pub const DEFAULT_BELL_VOLUME: i64 = 50;

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

/// Most entries `browser.externalOrigins` may hold.
///
/// An exempt list is a handful of SSO and banking hosts. A cap keeps the settings
/// document — which every client re-reads on every window focus — from becoming a
/// place to store a blocklist.
pub const MAX_EXTERNAL_ORIGINS: usize = 64;

/// Longest one origin pattern may be. A hostname is capped at 253 bytes, and the
/// scheme and port add a dozen; past that it is not an origin.
const MAX_ORIGIN_LEN: usize = 280;

/// Where a browser pane sends words that are not an address.
///
/// A pane's address bar takes http(s) URLs and nothing else
/// ([`crate::url`]'s rules are not involved; the UI's `normalizeBrowserUrl` is).
/// `react hooks docs` therefore used to be an error, which made the one thing a
/// blank pane is *for* — reading something while you work — the one thing it
/// refused. This template is where that text goes instead.
///
/// `%s` is the query, percent-encoded by the caller. Google because it is the
/// engine a browser's own bar would have used, and the alternative to picking one
/// is a blank pane that still dead-ends until a user finds a setting they have no
/// reason to look for.
///
/// **Empty string means "no search"**, and that is a supported value rather than a
/// broken one: the address bar goes back to refusing non-addresses, with an error
/// that says what a full address looks like. It is the off switch for anyone who
/// does not want a keystroke in a dev tool reaching an engine at all.
pub const DEFAULT_SEARCH_URL: &str = "https://www.google.com/search?q=%s";

/// The token a search template substitutes the query for — the convention every
/// browser's custom-engine field uses, so a URL copied from one works here.
pub const SEARCH_QUERY_TOKEN: &str = "%s";

/// Longest accepted `browser.searchUrl`. Engine URLs carry parameters, so this is
/// looser than an origin; past it, it is not a search template.
const MAX_SEARCH_URL_LEN: usize = 400;

/// Bounds on a key this binary does not recognise.
///
/// Unknown keys are preserved rather than rejected (see the module docs), which
/// makes them the one unbounded thing a client can put in this table. Preserving a
/// newer build's preference does not require accepting an arbitrary blob: the whole
/// document is returned by `GET` to every client on every window focus and mirrored
/// into `localStorage`, so an unbounded write is a cost every future read pays.
const MAX_UNKNOWN_KEY_LEN: usize = 128;
const MAX_UNKNOWN_VALUE_LEN: usize = 4096;

/// Which zone a log timestamp is *shown* in.
///
/// Storage is not affected and cannot be: every `log_lines.ts` is UTC because
/// `super::ts_to_str` exists to make lexicographic order equal chronological order,
/// which `logs_since`, the GC's pruning and both readers' interleave sorts all rely
/// on. This is a rendering choice, and these are the surfaces that honour it:
/// `veld logs`, `veld start`'s foreground log streaming (`--attach`), and the `/ide`
/// logs view. A new surface that prints `row.ts` raw shows a different clock than the
/// terminal beside it, so it should read this key too.
///
/// One human render site deliberately does **not**: the first-generation dashboard
/// (`veld-daemon/assets/management-ui.html`) renders local unconditionally, because it
/// is a frozen page that fetches no settings at all. So with this key on `utc`, that
/// page is the one still showing local — written down in `docs/configuration.md`
/// § Log Timestamps rather than left for someone to discover.
///
/// A string enum rather than a `logs.localTime` boolean because the obvious next
/// value is a *named* zone (read a colleague's logs in theirs), and `"local"`/`"utc"`
/// leaves room for one where a boolean would have to be replaced and migrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogTimeZone {
    /// The reader's own zone: the machine's for the CLI, the browser's for the UI.
    /// The default — a log line is almost always read against the clock on the wall.
    #[default]
    Local,
    /// UTC, exactly as stored.
    Utc,
}

impl LogTimeZone {
    /// Every zone this binary understands.
    ///
    /// The validator's allow-list and both **Rust** readers derive from this, so a new
    /// variant cannot validate on write and then be ignored by `veld logs` or
    /// `veld start` — which is the trap the string-enum shape invites: `one_of`
    /// hand-listing `["local","utc"]` beside a `_ => Local` reader would accept
    /// `"Europe/Berlin"` and silently render local.
    ///
    /// **It does not reach the `/ide` reader**, which hand-lists the same two spellings
    /// in TypeScript (`ui/src/shared/settings.ts`, `logsTimeZone`). Nothing ties the two
    /// lists together, so adding a variant here means editing that file in the same
    /// change or the UI will validate the value and render local anyway. That is the
    /// same Rust↔TS gap this module's header already names around `FALLBACK`.
    pub const ALL: &'static [LogTimeZone] = &[Self::Local, Self::Utc];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Utc => "utc",
        }
    }

    /// The inverse of [`Self::as_str`], exhaustive by construction over [`Self::ALL`].
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|tz| tz.as_str() == s)
    }
}

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
    TerminalBellVolume,
    TerminalDetachGrace,
    TerminalReconnectTries,
    TerminalReconnectBackoffSeconds,
    TerminalReconnectFirstDelaySeconds,
    TerminalOpenUrlsInApp,
    TerminalInterceptSystemOpen,
    WorktreeMarkerStyle,
    WorktreeTrashRetention,
    RunsHistoryDays,
    LogsTimeZone,
    BrowserQuickSwitchResponsive,
    BrowserQuickSwitchColorScheme,
    BrowserExternalOrigins,
    BrowserSearchUrl,
    UiHideDisabledActions,
    GitCreateFrom,
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
        Self::TerminalBellVolume,
        Self::TerminalDetachGrace,
        Self::TerminalReconnectTries,
        Self::TerminalReconnectBackoffSeconds,
        Self::TerminalReconnectFirstDelaySeconds,
        Self::TerminalOpenUrlsInApp,
        Self::TerminalInterceptSystemOpen,
        Self::WorktreeMarkerStyle,
        Self::WorktreeTrashRetention,
        Self::RunsHistoryDays,
        Self::LogsTimeZone,
        Self::BrowserQuickSwitchResponsive,
        Self::BrowserQuickSwitchColorScheme,
        Self::BrowserExternalOrigins,
        Self::BrowserSearchUrl,
        Self::UiHideDisabledActions,
        Self::GitCreateFrom,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            Self::TerminalFontSize => "terminal.fontSize",
            Self::TerminalFontFamily => "terminal.fontFamily",
            Self::TerminalCursorStyle => "terminal.cursorStyle",
            Self::TerminalCursorBlink => "terminal.cursorBlink",
            Self::TerminalScrollback => "terminal.scrollback",
            Self::TerminalShiftEnterNewline => "terminal.shiftEnterNewline",
            Self::TerminalBellVolume => "terminal.bellVolume",
            Self::TerminalDetachGrace => "terminal.detachGraceMinutes",
            Self::TerminalReconnectTries => "terminal.reconnectTries",
            Self::TerminalReconnectBackoffSeconds => "terminal.reconnectBackoffSeconds",
            Self::TerminalReconnectFirstDelaySeconds => "terminal.reconnectFirstDelaySeconds",
            Self::TerminalOpenUrlsInApp => "terminal.openUrlsInApp",
            Self::TerminalInterceptSystemOpen => "terminal.interceptSystemOpen",
            Self::WorktreeMarkerStyle => "worktree.markerStyle",
            Self::WorktreeTrashRetention => "worktree.trashRetentionDays",
            Self::RunsHistoryDays => "runs.historyDays",
            Self::LogsTimeZone => "logs.timeZone",
            Self::BrowserQuickSwitchResponsive => "browser.quickSwitch.responsive",
            Self::BrowserQuickSwitchColorScheme => "browser.quickSwitch.colorScheme",
            Self::BrowserExternalOrigins => "browser.externalOrigins",
            Self::BrowserSearchUrl => "browser.searchUrl",
            Self::UiHideDisabledActions => "ui.hideDisabledActions",
            Self::GitCreateFrom => "git.createFrom",
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
            "terminal.bellVolume" => Self::TerminalBellVolume,
            "terminal.detachGraceMinutes" => Self::TerminalDetachGrace,
            "terminal.reconnectTries" => Self::TerminalReconnectTries,
            "terminal.reconnectBackoffSeconds" => Self::TerminalReconnectBackoffSeconds,
            "terminal.reconnectFirstDelaySeconds" => Self::TerminalReconnectFirstDelaySeconds,
            "terminal.openUrlsInApp" => Self::TerminalOpenUrlsInApp,
            "terminal.interceptSystemOpen" => Self::TerminalInterceptSystemOpen,
            "worktree.markerStyle" => Self::WorktreeMarkerStyle,
            "worktree.trashRetentionDays" => Self::WorktreeTrashRetention,
            "runs.historyDays" => Self::RunsHistoryDays,
            "logs.timeZone" => Self::LogsTimeZone,
            "browser.quickSwitch.responsive" => Self::BrowserQuickSwitchResponsive,
            "browser.quickSwitch.colorScheme" => Self::BrowserQuickSwitchColorScheme,
            "browser.externalOrigins" => Self::BrowserExternalOrigins,
            "browser.searchUrl" => Self::BrowserSearchUrl,
            "ui.hideDisabledActions" => Self::UiHideDisabledActions,
            "git.createFrom" => Self::GitCreateFrom,
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
            Self::TerminalBellVolume => Value::from(clamp_i64(value, 0, 100).ok_or_else(bad)?),
            Self::TerminalDetachGrace => Value::from(
                clamp_i64(value, MIN_DETACH_GRACE_MINUTES, MAX_DETACH_GRACE_MINUTES)
                    .ok_or_else(bad)?,
            ),
            // Zero is the off switch and is deliberately inside the range (it is
            // the minimum) — see DEFAULT_RECONNECT_TRIES. The one numeric setting
            // whose lowest value is the answer to "I want none", not a lower
            // bound to clamp up from.
            Self::TerminalReconnectTries => Value::from(
                clamp_i64(value, MIN_RECONNECT_TRIES, MAX_RECONNECT_TRIES).ok_or_else(bad)?,
            ),
            Self::TerminalReconnectBackoffSeconds => Value::from(
                clamp_i64(
                    value,
                    MIN_RECONNECT_BACKOFF_SECONDS,
                    MAX_RECONNECT_BACKOFF_SECONDS,
                )
                .ok_or_else(bad)?,
            ),
            Self::TerminalReconnectFirstDelaySeconds => Value::from(
                clamp_i64(
                    value,
                    MIN_RECONNECT_FIRST_DELAY_SECONDS,
                    MAX_RECONNECT_FIRST_DELAY_SECONDS,
                )
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
            | Self::TerminalOpenUrlsInApp
            | Self::TerminalInterceptSystemOpen
            | Self::BrowserQuickSwitchResponsive
            | Self::BrowserQuickSwitchColorScheme
            | Self::UiHideDisabledActions => Value::from(value.as_bool().ok_or_else(bad)?),
            // The one list-valued setting, and the one whose entries are checked
            // by a parser that lives elsewhere: `veld_core::ide::parse_origin` is
            // what `ide.externalOrigins` in a project config goes through, and the
            // two halves of this exempt list are unioned — so a pattern accepted
            // here and refused there (or the reverse) would mean the same string
            // meant two things depending on where it was written.
            //
            // Rejected rather than filtered: an unparseable origin has no nearest
            // sensible value, and silently dropping one leaves a user looking at a
            // list they believe is in force. The whole patch fails, which is how
            // `patch_settings` reports a bad value everywhere else.
            Self::BrowserExternalOrigins => {
                let items = value.as_array().ok_or_else(bad)?;
                if items.len() > MAX_EXTERNAL_ORIGINS {
                    return Err(bad());
                }
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let raw = item.as_str().ok_or_else(bad)?.trim();
                    if raw.len() > MAX_ORIGIN_LEN || crate::ide::parse_origin(raw).is_err() {
                        return Err(bad());
                    }
                    // Stored as the author wrote it (trimmed), not as the parsed
                    // shape: this is the value a text field round-trips, and the
                    // parse happens on every read anyway.
                    if !out.iter().any(|v| v == &Value::from(raw)) {
                        out.push(Value::from(raw));
                    }
                }
                Value::Array(out)
            }
            // Rejected rather than coerced: there is no nearest sensible engine, and a
            // template that stored but never worked would send every query nowhere
            // with nothing to say why. Stored trimmed, which is the one
            // normalisation — the field round-trips its own value.
            Self::BrowserSearchUrl => {
                let s = value.as_str().ok_or_else(bad)?;
                Value::from(parse_search_template(s).map_err(|_| bad())?)
            }
            Self::TerminalCursorStyle => {
                one_of(value, &["block", "underline", "bar"]).ok_or_else(bad)?
            }
            Self::WorktreeMarkerStyle => one_of(value, &["color", "emoji"]).ok_or_else(bad)?,
            // Where a *new* worktree's branch is cut from. Rejected rather than
            // coerced (same as the other enums here): the daemon acts on this
            // directly in `create_worktree`, so a stored value neither surface
            // honours would silently change where branches come from.
            Self::GitCreateFrom => one_of(value, &["origin", "local"]).ok_or_else(bad)?,
            // Rejected rather than coerced, like every other enum here: the CLI reads
            // this key too, and a stored `"UTC"` that the reader then treats as the
            // default would mean the daemon reporting a saved preference neither
            // surface honours.
            Self::LogsTimeZone => {
                let s = value.as_str().ok_or_else(bad)?;
                Value::from(LogTimeZone::parse(s).ok_or_else(bad)?.as_str())
            }
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

/// Check a `browser.searchUrl` template, returning it trimmed.
///
/// Hand-rolled rather than parsed, for the reason [`crate::ide::parse_origin`] is:
/// `veld-core` has no URL crate, and the two validators would disagree about
/// oddities anyway. The rules are the small set that makes a template safe to
/// navigate to.
///
/// The one that is not obvious: **`%s` may not appear in the host.**
/// `https://%s.example.com/` would hand every word typed into an address bar the
/// choice of which host to reach — a redirect gadget built out of a preference,
/// and typing is how a user *avoids* thinking about hosts. The token belongs in
/// the path or the query, which is where every real engine puts it.
///
/// An empty template is accepted and means search is off — see
/// [`DEFAULT_SEARCH_URL`].
pub fn parse_search_template(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    // Bytes, and the message says bytes: `t.len()` is a byte count, so an engine on an
    // IDN host (`https://поиск.рф/?q=%s`) gets half the budget a character count would
    // give it. Saying "characters" made the refusal read as a lie about a template the
    // author can see is shorter than that.
    if t.len() > MAX_SEARCH_URL_LEN {
        return Err(format!("longer than {MAX_SEARCH_URL_LEN} bytes"));
    }
    // Whitespace and controls, before anything else: a template carrying either is
    // not a URL, and `char::is_control` also catches the newline that would let one
    // stored value pose as two.
    if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("must not contain spaces or control characters".to_owned());
    }
    if !t.contains(SEARCH_QUERY_TOKEN) {
        return Err(format!(
            "must contain {SEARCH_QUERY_TOKEN} where the query goes"
        ));
    }
    // Case-insensitively, because a scheme is: `HTTPS://duckduckgo.com/?q=%s` is a
    // valid URL that the *client* accepts (`/^https?:\/\//i` in `panes/model.ts`), so a
    // case-sensitive `strip_prefix` here refused it with "must start with http:// or
    // https://" — a message the author can see is false.
    let lower = t.to_ascii_lowercase();
    let scheme_len = if lower.starts_with("https://") {
        "https://".len()
    } else if lower.starts_with("http://") {
        "http://".len()
    } else {
        return Err("must start with http:// or https://".to_owned());
    };
    let host = t[scheme_len..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_owned();
    if host.is_empty() {
        return Err("has no host".to_owned());
    }
    if host.contains(SEARCH_QUERY_TOKEN) {
        return Err(format!(
            "must not put {SEARCH_QUERY_TOKEN} in the host — it belongs in the path or query"
        ));
    }
    // **The host has to be a host, not merely a non-empty span.** A template that
    // passes here and then cannot be parsed by the client fails at the point of *use*,
    // where the only thing the pane can say is "not an http(s) address: <what the user
    // typed>" — it blames the query for a broken setting, and there is no path from
    // that message back to this field. So the whole class is refused here: an empty
    // authority (`https://:8080/`), a non-numeric or out-of-range port
    // (`https://e.com:abc/`, `https://e.com:99999/`), and anything outside the
    // characters a host may hold (`https://e.com]/`, `https://%/`).
    check_search_host(&host)?;
    Ok(t.to_owned())
}

/// The host span of a search template: `host[:port]`, or a bracketed IPv6 literal.
///
/// **Not a reimplementation of a URL parser, and not equivalent to one.** An earlier
/// version of this comment claimed every spelling rejected here is one `new URL()`
/// rejects too. That was false in three places and the third one cost a feature:
/// `new URL()` accepts a port of `0`, accepts `https://../`, and **punycodes a
/// non-ASCII hostname** — so an ASCII-only charset check refused
/// `https://поиск.рф/?q=%s` outright, which is the very example the length rule above
/// discusses as a case worth getting right.
///
/// So the charset test is a deny-list of the punctuation a hostname cannot hold, not an
/// allow-list of ASCII: a browser resolves an IDN engine perfectly well, and this is not
/// the place to have an opinion about scripts. What remains deliberately stricter than
/// the parser is a port of `0` — it parses and cannot be connected to, and refusing it
/// on the settings screen beats a template that silently never works.
///
/// **This function is the second line of defence, not the first.** Review found a defect
/// in it in three consecutive rounds (`https://:8080/`, an ASCII-only host, then trailing
/// junk after an IPv6 `]` plus an overflowing port), which is what hand-rolling a URL
/// grammar earns. The first line is the settings dialog, which runs the *real* parser the
/// pane will use (`searchTarget` in `panes/model.ts`) before it saves — see
/// `SettingsDialog.tsx`. What is left here matters only for a write that does not come
/// from our own UI, and its job is to reject the shapes it can state exactly rather than
/// to be a URL parser.
fn check_search_host(host: &str) -> Result<(), String> {
    let (name, port) = match host.strip_prefix('[') {
        // IPv6 literal: the brackets are what separate the address' own colons from the
        // port separator, so the split has to happen after them.
        Some(rest) => match rest.split_once(']') {
            Some((inner, after)) => {
                if inner.is_empty()
                    || !inner
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
                {
                    return Err("is not a valid IPv6 address".to_owned());
                }
                // **Whatever follows the `]` has to be nothing or a port.** Mapping it
                // through `strip_prefix(':')` alone turned trailing junk into "no port"
                // and accepted `https://[::1]xyz/`, which the client's parser throws on.
                let port = match after {
                    "" => None,
                    rest => Some(
                        rest.strip_prefix(':')
                            .ok_or_else(|| {
                                format!("has {rest:?} after the ] where a port should be")
                            })?
                            .to_owned(),
                    ),
                };
                (String::new(), port)
            }
            None => return Err("has an unclosed [ in the host".to_owned()),
        },
        None => match host.split_once(':') {
            Some((name, port)) => (name.to_owned(), Some(port.to_owned())),
            None => (host.to_owned(), None),
        },
    };
    if let Some(port) = port {
        // `matches!` over the parse, not `is_ok_and`: an all-digit port too big for a
        // `u32` (`4294967296`) parses to `Err`, and `is_ok_and` reads `Err` as "not out
        // of range" — so the one value most obviously out of range was the one accepted.
        // The digit test stays, because `"+5".parse::<u32>()` is `Ok(5)` while the URL
        // parser throws on it.
        if port.is_empty()
            || !port.chars().all(|c| c.is_ascii_digit())
            || !matches!(port.parse::<u32>(), Ok(p) if (1..=65535).contains(&p))
        {
            return Err(format!("has {port:?} where a port number should be"));
        }
    }
    if host.starts_with(':') {
        return Err("has a port but no host".to_owned());
    }
    // Empty only for the IPv6 branch, which validated its own address above. A
    // deny-list, for the reason in this function's docs: these are the characters that
    // make `new URL()` throw or that would silently mean something other than a host
    // (an escape, an authority delimiter, a second path). Everything else — including
    // every non-ASCII script — is a hostname a browser can resolve.
    // `/ ? #` are already gone (the host span was split on them); they stay for the
    // reader, since this list is meant to be read as "what a host may not hold".
    if name.chars().any(|c| {
        matches!(
            c,
            '%' | '[' | ']' | '\\' | '/' | '?' | '#' | '@' | ':' | '<' | '>' | '^' | '|'
        )
    }) {
        return Err("has characters in the host that a hostname cannot hold".to_owned());
    }
    Ok(())
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
        (
            SettingKey::TerminalBellVolume,
            Value::from(DEFAULT_BELL_VOLUME),
        ),
        // Ships on: #197 established `ESC CR` as the default because it is what
        // Claude Code's `/terminal-setup` configures, so matching it means no
        // extra setup. The toggle exists for anyone whose TUI binds meta-Enter.
        (SettingKey::TerminalShiftEnterNewline, Value::from(true)),
        (
            SettingKey::TerminalDetachGrace,
            Value::from(DEFAULT_DETACH_GRACE_MINUTES),
        ),
        // Auto-reconnect ships on at three tries: a dropped socket is the common
        // transient (a sleep, a proxy timeout, a daemon restart the holder
        // outlives) and a shell still running is exactly what the holder process
        // exists to preserve. A manual Reconnect is one click away if three tries
        // are not enough. Zero turns the whole thing off.
        (
            SettingKey::TerminalReconnectTries,
            Value::from(DEFAULT_RECONNECT_TRIES),
        ),
        (
            SettingKey::TerminalReconnectBackoffSeconds,
            Value::from(DEFAULT_RECONNECT_BACKOFF_SECONDS),
        ),
        (
            SettingKey::TerminalReconnectFirstDelaySeconds,
            Value::from(DEFAULT_RECONNECT_FIRST_DELAY_SECONDS),
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
        // A URL a terminal produces opens in a pane by default — that is the
        // feature, and a routing feature defaulted off is one nobody finds. The
        // escape hatches are the exempt list below (per host, and per project) and
        // this switch (all of it, everywhere).
        (SettingKey::TerminalOpenUrlsInApp, Value::from(true)),
        // On, because the case it exists for is an agent running `open <url>` in a
        // terminal pane, and that is the one a user cannot work around themselves.
        // It is the only setting that puts veld in a shell's startup, which is why
        // it is a setting at all — see the key's own docs.
        (SettingKey::TerminalInterceptSystemOpen, Value::from(true)),
        // Empty: veld ships no opinion about which hosts need the real browser.
        // A default entry would be a guess about someone else's SSO provider.
        (SettingKey::BrowserExternalOrigins, Value::Array(Vec::new())),
        // An engine *is* shipped, unlike the exempt list above, and the difference is
        // which way the empty default fails. An empty exempt list works — every host
        // opens in a pane, which is the feature. An empty search template makes a
        // blank pane refuse the first thing anyone types into it, and the fix is a
        // setting nobody knows to look for. See the constant.
        (
            SettingKey::BrowserSearchUrl,
            Value::from(DEFAULT_SEARCH_URL),
        ),
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
        // Local, because a log line is read against the clock the reader is looking
        // at. UTC is what veld *stores* — a correctness requirement, not a display
        // preference — and printing storage at a human was the previous behaviour
        // only because nothing had decided otherwise.
        (
            SettingKey::LogsTimeZone,
            Value::from(LogTimeZone::default().as_str()),
        ),
        // Hide a top-bar action that is currently inapplicable (restart with no
        // live run, the machine-vars button for a project that asks for nothing, a
        // URLs button with nothing to open) rather than showing it greyed out. The
        // alternative — keep every button, disable the ones that cannot fire — is
        // the value a user who wants a stable bar picks, so the default is the
        // other one: the bar is the densest row in the app, and an icon that does
        // nothing still has to be read before it is dismissed. This is a rendering
        // choice only; nothing the daemon enforces reads it.
        (SettingKey::UiHideDisabledActions, Value::from(true)),
        // Where a new worktree's branch is cut from. `origin` is the point of
        // this setting: a worktree created from a stale local `main` is born
        // behind the remote — missing the latest DB migrations, conflicting with
        // open PRs — and it compounds, because nobody goes back to update `main`.
        // Fetch-then-base-on-origin makes each new worktree current at birth. The
        // daemon acts on this in `create_worktree`, so it is validated above like
        // `TerminalDetachGrace`, not trusted from the wire.
        (SettingKey::GitCreateFrom, Value::from("origin")),
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

    /// Whether a URL a terminal produces opens in a Veld browser pane.
    ///
    /// Read by the daemon (it is the daemon that routes the URL — see
    /// `veld_core::ide::route_url`), so it goes through the same "anything that is
    /// not a real `true`/`false` is the default" path the other daemon-read keys
    /// take, rather than trusting the stored bytes.
    pub fn terminal_open_urls_in_app(&self) -> bool {
        self.setting(&SettingKey::TerminalOpenUrlsInApp)
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Whether a terminal session gets the shim directory on its `PATH`, so that a
    /// program calling `open`/`xdg-open` (rather than reading `$BROWSER`) is routed
    /// too — an agent's `Bash(open "https://…")` being the case that matters.
    ///
    /// Read by the daemon, which is what builds the session's environment. Separate
    /// from [`SettingKey::TerminalOpenUrlsInApp`] because it is a different question:
    /// that one is *where a URL opens*, this one is *whether veld arranges to see
    /// the call at all*, and the mechanism (a `.zshenv` of veld's own that hands
    /// `ZDOTDIR` straight back and registers one hook) is the only place veld runs
    /// inside a user's shell startup. Anything that touches a shell's startup gets
    /// an off switch.
    pub fn terminal_intercept_system_open(&self) -> bool {
        self.setting(&SettingKey::TerminalInterceptSystemOpen)
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Where a new worktree's branch is cut from (`git.createFrom`).
    ///
    /// Read by the daemon's `create_worktree`, so it goes through the same
    /// "anything not a real value is the default" path the other daemon-read
    /// keys take rather than trusting the stored bytes. The stored value is
    /// validated by [`SettingKey::GitCreateFrom`], so a value that is not
    /// `"local"` here is `"origin"` (the default).
    pub fn git_create_from(&self) -> GitCreateSource {
        if self
            .setting(&SettingKey::GitCreateFrom)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_owned))
            .as_deref()
            == Some("local")
        {
            GitCreateSource::Local
        } else {
            GitCreateSource::Origin
        }
    }

    /// The global half of the exempt list: origins that must open in the system
    /// browser rather than in a pane.
    ///
    /// Re-parsed on read rather than cached, and **entries that no longer parse are
    /// dropped** — the same degrade-to-safe posture `settings()` takes for a value
    /// a newer build wrote. Dropping an entry means that host opens in a pane, which
    /// is the visible failure; keeping an unparseable one would mean a list that
    /// silently matches nothing, which is not.
    pub fn external_origins(&self) -> Vec<crate::ide::OriginPattern> {
        self.setting(&SettingKey::BrowserExternalOrigins)
            .ok()
            .flatten()
            .and_then(|v| match v {
                Value::Array(items) => Some(items),
                _ => None,
            })
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(|raw| crate::ide::parse_origin(raw).ok())
            .collect()
    }

    /// Which zone a log timestamp is rendered in — read by `veld logs`,
    /// `veld start --attach`'s streaming, and (over the settings API) the `/ide` view.
    ///
    /// Has an accessor rather than being read raw because the **CLI** reads this one —
    /// it is not a value the daemon merely stores for a browser — and [`Db::setting`]
    /// returns the stored bytes without revalidating them. Anything that is not one of
    /// the two spellings resolves to [`LogTimeZone::Local`], the same
    /// degrade-to-the-default posture [`Db::terminal_open_urls_in_app`] takes for a
    /// value a newer build wrote.
    pub fn logs_time_zone(&self) -> LogTimeZone {
        self.setting(&SettingKey::LogsTimeZone)
            .ok()
            .flatten()
            .as_ref()
            .and_then(|v| v.as_str())
            .and_then(LogTimeZone::parse)
            .unwrap_or_default()
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
    fn the_exempt_list_is_validated_by_the_config_parser_and_read_back_parsed() {
        let (_dir, db) = test_db();
        // Empty by default, and the master switch is on — the feature's point.
        assert!(db.external_origins().is_empty());
        assert!(db.terminal_open_urls_in_app());

        db.patch_settings(&patch(&[(
            "browser.externalOrigins",
            // A duplicate collapses; whitespace is trimmed.
            serde_json::json!([
                "https://accounts.google.com",
                " https://*.okta.com ",
                "https://accounts.google.com"
            ]),
        )]))
        .unwrap();
        let stored = db.settings().unwrap()["browser.externalOrigins"].clone();
        assert_eq!(
            stored,
            serde_json::json!(["https://accounts.google.com", "https://*.okta.com"])
        );
        // Read back as parsed patterns, which is what the router matches against.
        let parsed = db.external_origins();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].host, "accounts.google.com");
        assert!(parsed[1].wildcard);

        // Every refusal `ide.externalOrigins` makes in a project config, this key
        // makes too — one parser, so a pattern cannot mean two things depending on
        // where it was written. And a rejected entry fails the whole patch rather
        // than being filtered out of a list the user believes is in force.
        for bad in [
            serde_json::json!(["accounts.google.com"]), // no scheme
            serde_json::json!(["https://*.com"]),       // a whole TLD
            serde_json::json!(["https://a.com/path"]),  // not an origin
            serde_json::json!(["http://evil.com@localhost"]), // credentials
            serde_json::json!([7]),                     // not a string
            serde_json::json!("https://a.com"),         // not an array
            serde_json::json!([format!("https://{}.com", "a".repeat(MAX_ORIGIN_LEN))]),
            serde_json::json!(vec!["https://a.com"; MAX_EXTERNAL_ORIGINS + 1]),
        ] {
            let e = db
                .patch_settings(&patch(&[("browser.externalOrigins", bad.clone())]))
                .expect_err(&format!("{bad} must be refused"));
            assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
        }
        // …and the good value above survived every rejection.
        assert_eq!(db.external_origins().len(), 2);

        // The switch is a bool and nothing else.
        assert!(
            db.patch_settings(&patch(&[("terminal.openUrlsInApp", Value::from("yes"))]))
                .is_err()
        );
        db.patch_settings(&patch(&[("terminal.openUrlsInApp", Value::from(false))]))
            .unwrap();
        assert!(!db.terminal_open_urls_in_app());
    }

    #[test]
    fn the_search_template_ships_an_engine_and_refuses_an_unnavigable_one() {
        let (_dir, db) = test_db();
        // Shipped on, unlike the exempt list: a blank pane that refuses the first
        // thing typed into it is the bug this key exists to fix.
        assert_eq!(
            db.settings().unwrap()["browser.searchUrl"],
            Value::from(DEFAULT_SEARCH_URL)
        );

        // Trimmed on the way in, and an empty template is the off switch rather than
        // a rejection.
        db.patch_settings(&patch(&[(
            "browser.searchUrl",
            Value::from("  https://duckduckgo.com/?q=%s  "),
        )]))
        .unwrap();
        assert_eq!(
            db.settings().unwrap()["browser.searchUrl"],
            Value::from("https://duckduckgo.com/?q=%s")
        );
        // A scheme is case-insensitive, and the client's reader accepts this spelling —
        // so refusing it here would make one stored value legal in one half of the app.
        db.patch_settings(&patch(&[(
            "browser.searchUrl",
            Value::from("HTTPS://duckduckgo.com/?q=%s"),
        )]))
        .unwrap();
        // The host shapes that *are* navigable stay accepted — the host check is a gate
        // on the class of broken template above, not a narrowing of what an engine may
        // be hosted on.
        for good in [
            "http://localhost:8080/search?q=%s",
            "https://search.example.co.uk/?q=%s&hl=en",
            "https://my_engine.internal/?q=%s",
            "http://127.0.0.1:1234/?q=%s",
            "http://[::1]:8080/?q=%s",
            // An IDN engine. A browser punycodes this and resolves it; an ASCII-only
            // charset check refused it, which removed a working engine for no reason —
            // and contradicted the length rule's own worked example.
            "https://поиск.рф/?q=%s",
        ] {
            db.patch_settings(&patch(&[("browser.searchUrl", Value::from(good))]))
                .unwrap_or_else(|e| panic!("{good} must be accepted: {e}"));
        }
        db.patch_settings(&patch(&[("browser.searchUrl", Value::from(""))]))
            .unwrap();
        assert_eq!(db.settings().unwrap()["browser.searchUrl"], Value::from(""));

        for bad in [
            Value::from("https://example.com/search"), // no %s: searches nothing
            Value::from("example.com/?q=%s"),          // no scheme
            Value::from("ftp://example.com/?q=%s"),    // not http(s)
            Value::from("https:///?q=%s"),             // no host
            // Every one of these passes the %s/scheme/whitespace rules and is then
            // refused by the *client's* URL parser, so it used to fail at the point of
            // use with "not an http(s) address: <the user's own query>" — blaming the
            // query for a broken setting, with no path back to the field. A setting
            // that cannot work has to fail where it is typed.
            Value::from("https://:8080/?q=%s"), // port, no host
            Value::from("https://e.com:abc/?q=%s"), // port that is not a number
            Value::from("https://e.com:99999/?q=%s"), // port out of range
            Value::from("https://e.com:0/?q=%s"), // port zero
            Value::from("https://e.com]/?q=%s"), // stray bracket
            Value::from("https://%/?q=%s"),     // not a hostname
            Value::from("https://[:1/?q=%s"),   // unclosed IPv6 bracket
            Value::from("https://[zz::1]/?q=%s"), // not hex in an IPv6 literal
            // The two this check accepted for three rounds. Trailing junk after the `]`
            // was mapped to "no port" instead of refused, and an all-digit port too big
            // for a `u32` parsed to `Err`, which the range test read as "in range".
            Value::from("https://[::1]xyz/?q=%s"),
            Value::from("https://[::1]a:8080/?q=%s"),
            Value::from("https://e.com:4294967296/?q=%s"),
            Value::from("https://e.com:99999999999/?q=%s"),
            // `"+5".parse::<u32>()` is `Ok(5)`, so the digit test is what refuses this —
            // the URL parser throws on it.
            Value::from("https://e.com:+5/?q=%s"),
            Value::from("https://%s.example.com/"), // the query picks the host
            Value::from("https://e.com/?q=%s x"),   // whitespace
            Value::from("https://e.com/?q=%s\nhttps://e2.com/?q=%s"), // two values in one
            Value::from(7),                         // not a string
            Value::from(format!(
                "https://e.com/?q=%s&pad={}",
                "a".repeat(MAX_SEARCH_URL_LEN)
            )),
        ] {
            let e = db
                .patch_settings(&patch(&[("browser.searchUrl", bad.clone())]))
                .expect_err(&format!("{bad} must be refused"));
            assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
        }
        // …and the off switch set above survived every rejection.
        assert_eq!(db.settings().unwrap()["browser.searchUrl"], Value::from(""));
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
    fn reconnect_prefs_default_and_clamp() {
        let (_dir, db) = test_db();
        let s = db.settings().unwrap();
        assert_eq!(
            s["terminal.reconnectTries"],
            Value::from(DEFAULT_RECONNECT_TRIES),
            "auto-reconnect ships on at three tries"
        );
        assert_eq!(
            s["terminal.reconnectBackoffSeconds"],
            Value::from(DEFAULT_RECONNECT_BACKOFF_SECONDS)
        );
        assert_eq!(
            s["terminal.reconnectFirstDelaySeconds"],
            Value::from(DEFAULT_RECONNECT_FIRST_DELAY_SECONDS)
        );

        // Zero is the off switch and must survive the clamp, not be raised to
        // one — the same shape as the trash retention's zero.
        db.patch_settings(&patch(&[("terminal.reconnectTries", Value::from(0))]))
            .unwrap();
        assert_eq!(
            db.settings().unwrap()["terminal.reconnectTries"],
            Value::from(0)
        );

        // Above the ceilings clamps down.
        db.patch_settings(&patch(&[(
            "terminal.reconnectTries",
            Value::from(MAX_RECONNECT_TRIES + 50),
        )]))
        .unwrap();
        assert_eq!(
            db.settings().unwrap()["terminal.reconnectTries"],
            Value::from(MAX_RECONNECT_TRIES)
        );

        // A negative (or wrong-typed) value degrades to the default on read.
        db.patch_settings(&patch(&[(
            "terminal.reconnectBackoffSeconds",
            Value::from("soon"),
        )]))
        .unwrap_err();
        assert_eq!(
            db.settings().unwrap()["terminal.reconnectBackoffSeconds"],
            Value::from(DEFAULT_RECONNECT_BACKOFF_SECONDS)
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
    fn logs_time_zone_defaults_to_local_and_rejects_anything_but_the_two_spellings() {
        let (_dir, db) = test_db();
        // Local by default: the stored zone is UTC because ordering depends on it,
        // which is not a reason to show a human UTC.
        assert_eq!(db.logs_time_zone(), LogTimeZone::Local);
        assert_eq!(
            db.settings().unwrap()["logs.timeZone"],
            Value::from("local")
        );

        db.patch_settings(&patch(&[("logs.timeZone", Value::from("utc"))]))
            .unwrap();
        assert_eq!(db.logs_time_zone(), LogTimeZone::Utc);

        // An enum, so a near-miss is refused rather than coerced — the CLI and the UI
        // both read this, and a value only one of them honoured would be worse than
        // an error at the point of writing.
        for bad in [
            Value::from("UTC"),
            Value::from("Local"),
            Value::from("Europe/Berlin"),
            Value::from(true),
        ] {
            let e = db
                .patch_settings(&patch(&[("logs.timeZone", bad.clone())]))
                .expect_err(&format!("{bad} must be refused"));
            assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
        }
        // …and the accepted value above survived every rejection.
        assert_eq!(db.logs_time_zone(), LogTimeZone::Utc);
    }

    #[test]
    fn every_log_time_zone_round_trips_through_its_spelling() {
        // `ALL` is hand-maintained and everything else derives from it: the validator's
        // allow-list, `parse`, and both readers. A variant missing from it is refused on
        // write rather than accepted-then-ignored, which is the failure this shape
        // exists to prevent — so pin the round trip.
        for tz in LogTimeZone::ALL {
            assert_eq!(LogTimeZone::parse(tz.as_str()), Some(*tz));
        }
        assert_eq!(LogTimeZone::ALL.len(), 2);
        assert_eq!(LogTimeZone::default(), LogTimeZone::Local);
        assert!(LogTimeZone::parse("Europe/Berlin").is_none());
        assert!(LogTimeZone::parse("UTC").is_none(), "spelling is exact");
    }

    #[test]
    fn an_unreadable_logs_time_zone_reads_as_local() {
        // The shape a newer build with a third zone would leave behind. `setting`
        // does not revalidate, so the accessor is the only guard — and the CLI is a
        // caller, which makes "degrade to the default" the whole point.
        let (_dir, db) = test_db();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO settings (scope, key, value, updated_at)
                 VALUES ('global', 'logs.timeZone', '\"Asia/Tokyo\"', '2026-01-01T00:00:00.000000Z')",
                [],
            )
            .unwrap();
        }
        assert_eq!(db.logs_time_zone(), LogTimeZone::Local);
        // And the effective document degrades the same way, so the UI agrees.
        assert_eq!(
            db.settings().unwrap()["logs.timeZone"],
            Value::from("local")
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
