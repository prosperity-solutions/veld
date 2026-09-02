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

use super::settings_catalog::{
    CURSOR_STYLES, Choice, GIT_CREATE_SOURCES, MARKER_STYLES, WORKTREE_STORAGE_MODES,
};
use super::{Db, DbError, now_str};

/// The scope a setting is stored under. Only [`SCOPE_GLOBAL`] is written today;
/// the column exists so per-project overrides do not need a migration later.
pub const SCOPE_GLOBAL: &str = "global";

/// Where a new worktree's branch is cut from — the `git.createFrom` setting.
///
/// `Origin` fetches the remote and bases the new branch on `origin/<default>`,
/// so a worktree is never born behind the remote. `Local` uses the main
/// checkout's current HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitCreateSource {
    #[default]
    Origin,
    Local,
}

impl GitCreateSource {
    /// Every source this binary understands. Mirrors [`ConfigSource::ALL`] and
    /// [`LogTimeZone::ALL`], and for the reason those two spell out: the
    /// validator's allow-list and the daemon's reader both derive from this, so a
    /// third variant cannot validate on write and then be read back as `Origin`
    /// by a hand-written comparison that never learned about it. This key had
    /// exactly that shape — `== Some("local")` in [`Db::git_create_from`] beside
    /// a `one_of` listing both spellings — which is the defect class
    /// `a_config_source_is_never_hand_compared` was written for.
    pub const ALL: &'static [GitCreateSource] = &[Self::Origin, Self::Local];

    pub const ORIGIN: &'static str = "origin";
    pub const LOCAL: &'static str = "local";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Origin => Self::ORIGIN,
            Self::Local => Self::LOCAL,
        }
    }

    /// The inverse of [`Self::as_str`], exhaustive by construction over
    /// [`Self::ALL`].
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|src| src.as_str() == s)
    }
}

/// Which checkout's `veld.json` a project-declared surface reads its
/// declarations from — `extensions.source` and `news.source`.
///
/// **`Main` is the default for both**, which is a reversal for extensions: see
/// the 2026-08-13 "Extensions are worktree-based" entry in
/// `docs/extensions-vision.md`. That decision shipped the same day it was made
/// and was itself reversed once its cost showed up in practice — a new user
/// onboarding onto Veld's IDE sees the "extensions" news card, but their
/// pre-existing worktrees were cloned before the project's `veld.json` gained
/// an `ide.extensions` block, so nothing renders until they create a fresh
/// worktree. Reading from the **main** checkout instead means every worktree
/// of a project sees whatever the project has merged, regardless of when that
/// worktree was created — matching how `ide.news` already worked, and for the
/// same underlying reason: a card or a badge that predates a worktree's own
/// clone must still reach it.
///
/// **`Worktree` exists for testing an extension (or a news card) before it
/// merges** — the workflow the original worktree-based decision was written to
/// protect (`docs/extensions-vision.md`, same entry): check out a branch, add
/// or edit the declaration, see it render in that same worktree, then merge.
/// Flipping this setting to `Worktree` restores exactly that loop; the
/// trade-off is the one the reversed decision accepted — an untrusted branch's
/// own badge commands do not run automatically in `Main` mode (its
/// declarations are ignored until merged), which is a net security
/// improvement over the worktree-based default, not a regression: reviewing a
/// fork pull request no longer means trusting its `veld.json` to run
/// unattended.
///
/// Three things worth stating plainly rather than leaving implicit:
///
/// - **"Main" means the main *checkout*, not the default *branch*** — the same
///   distinction `ide.news` already draws (`docs/configuration.md`). If the
///   primary clone is itself sitting on an untrusted branch (a `gh pr
///   checkout` run there, or main simply left on a feature branch), `Main`
///   mode resolves declarations from *that* checkout's config — the safety
///   property holds only as long as the main checkout is actually on the
///   project's default branch.
/// - **The command still executes with the *viewed* worktree as its working
///   directory**, even when its declaration came from main. A main-declared
///   badge that shells out to a repo-relative script (this project's own
///   `scripts/veld/pr-badge.sh` example) still runs whatever that path
///   resolves to in the untrusted checkout, not in main's.
/// - **This setting is machine-global, not per-repo.** Flipping it to
///   `Worktree` to test one project's extension puts every other open
///   repo's worktrees back on worktree-sourced declarations too, for as long
///   as it is left that way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Main,
    Worktree,
}

impl ConfigSource {
    /// Every source this binary understands. Mirrors [`LogTimeZone::ALL`]:
    /// the validator's allow-list and the daemon's readers both derive from
    /// this, so a third variant cannot validate on write and then be
    /// silently read back as `Main` by a hand-written `== Some("worktree")`
    /// check that never learned about it.
    pub const ALL: &'static [ConfigSource] = &[Self::Main, Self::Worktree];

    /// The stored spellings, named so the catalog's [`Choice`](super::settings_catalog::Choice)
    /// lists can cite the same literal this type's `as_str` returns rather than
    /// writing `"main"` a second time. Two labels for one value is a real case
    /// here — `extensions.source` and `news.source` describe `worktree`
    /// differently — so the *labels* cannot be shared, which is exactly why the
    /// values must be.
    pub const MAIN: &'static str = "main";
    pub const WORKTREE: &'static str = "worktree";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => Self::MAIN,
            Self::Worktree => Self::WORKTREE,
        }
    }

    /// The inverse of [`Self::as_str`], exhaustive by construction over
    /// [`Self::ALL`].
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|source| source.as_str() == s)
    }
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

/// Terminal font size, in CSS pixels.
///
/// Named rather than written inline because the catalog cites the same bounds to
/// tell a client what to offer — see `settings_catalog`'s *Why this is a
/// projection*. Six is the smallest size xterm's renderer measures reliably;
/// seventy-two is a presentation, not a terminal.
pub const DEFAULT_FONT_SIZE: i64 = 12;
pub const MIN_FONT_SIZE: i64 = 6;
pub const MAX_FONT_SIZE: i64 = 72;

/// Terminal bell volume, as a percentage 0–100.
///
/// Scales the Web-Audio tone a `BEL` plays. A percentage rather than a linear
/// amplitude because that is what a slider is: 0 is silent, 100 is the loudest
/// this build will play. `playBell` in the UI maps it onto a gain.
///
/// **75, not half.** `playBell` multiplies this by a 0.5 peak (`terminalHost.ts`),
/// so the slider's 100 is already a deliberately soft tone rather than a alarm —
/// which made a 50 default quiet enough to miss in a room with anything else going
/// on, for a signal whose entire job is to be noticed while you are looking
/// somewhere else. Anyone who wants it softer has the slider.
pub const DEFAULT_BELL_VOLUME: i64 = 75;

/// Bounds on the bell volume. A percentage, so these are what a percentage is —
/// named for the same reason the font-size bounds above are.
pub const MIN_BELL_VOLUME: i64 = 0;
pub const MAX_BELL_VOLUME: i64 = 100;

/// Bounds on the scrollback. Zero is a terminal with no history, which is a
/// legitimate thing to want on a memory-tight machine.
pub const MIN_SCROLLBACK: i64 = 0;

/// Bounds on the run-history horizon. Zero means "show everything" — see
/// [`DEFAULT_RUN_HISTORY_DAYS`].
pub const MIN_RUN_HISTORY_DAYS: i64 = 0;

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

/// How often the daemon writes a copy of the database, in minutes.
///
/// **The floor is what makes this a preference rather than a foot-gun.** A backup
/// is cheap because it omits the bulk tables (see
/// [`veld_core::db::backup`](super::backup)), but it is not free: it opens a second
/// connection, walks every remaining table and writes a file. A one-minute interval
/// on a machine with a large registry would spend more of the daemon's time copying
/// state than serving it, and buys a minute of freshness nobody has ever wanted. The
/// ceiling is a day, past which "periodic" stops being true.
pub const MIN_BACKUP_INTERVAL_MINUTES: i64 = 5;
pub const MAX_BACKUP_INTERVAL_MINUTES: i64 = 24 * 60;

/// Hourly-ish, which is the interval whose worst case is the amount of work a
/// person will happily redo: the state this protects is *arrangement* — which repos
/// are registered, which worktrees exist, how the lanes and panes are laid out —
/// and an hour of that is a few minutes to recreate. Five minutes was the interval
/// the filed issue sketched; it is available, and it is not the default, because the
/// cost of the shorter interval is paid every hour of every day while the benefit is
/// paid out once, if ever.
pub const DEFAULT_BACKUP_INTERVAL_MINUTES: i64 = 60;

/// How many of the most recent backups are kept.
///
/// One is not enough and is the trap this bound exists to refuse: the failure being
/// protected against is a database that became unreadable, and nothing guarantees
/// the daemon noticed before it copied it. A handful of generations is what lets
/// somebody step back past a bad one.
pub const MIN_BACKUP_KEEP: i64 = 2;
pub const MAX_BACKUP_KEEP: i64 = 500;

/// Twelve recent copies — half a day at the default interval, and enough
/// generations to step back past a few bad ones.
pub const DEFAULT_BACKUP_KEEP: i64 = 12;

/// How many days keep one backup each, beyond the recent ones.
///
/// **Zero is off, and is deliberately inside the range** — the same shape as
/// `worktree.trashRetentionDays`: a user turning the daily tail off must not have
/// their zero clamped up into turning it on. A count alone bounds disk and not
/// *time*: twelve copies at a five-minute interval is an hour of history, so a
/// corruption noticed the next morning has nothing to restore from. One backup per
/// day, kept for a fortnight, is what covers "I only noticed on Monday".
pub const MIN_BACKUP_KEEP_DAILY: i64 = 0;
pub const MAX_BACKUP_KEEP_DAILY: i64 = 365;

/// A fortnight of dailies. Each is a few megabytes, so the whole tail is smaller
/// than one screenshot-heavy feedback thread.
pub const DEFAULT_BACKUP_KEEP_DAILY: i64 = 14;

/// Longest accepted `backup.dir`. Same reasoning and same bound as
/// [`MAX_WORKTREE_STORAGE_DIR_LEN`] — a filesystem path, generous but bounded,
/// because the whole settings document round-trips through every client.
const MAX_BACKUP_DIR_LEN: usize = 1024;

/// How long an automatic keep-awake may hold, in minutes, per power source.
///
/// These are the *caps on a hold nobody pressed a button for*, which is what makes
/// them different in kind from the durations in the coffee menu. The floor is five
/// minutes rather than zero because zero is not a shorter hold, it is the switch
/// beside the number already being off — and a cap of zero would arm an inhibitor
/// and drop it on the same tick, spawning a process per share for no coverage. The
/// ceiling matches the longest duration the menu itself offers: a machine held
/// awake for longer than a working day, with no press behind it, is the outcome
/// this whole feature is shaped to avoid.
pub const MIN_KEEP_AWAKE_MINUTES: i64 = 5;
pub const MAX_KEEP_AWAKE_MINUTES: i64 = 8 * 60;

/// Defaults for the two automatic caps, which are deliberately *not* equal.
///
/// Mains is the generous one: nothing is being spent, and `caffeinate -s` is
/// valid on AC power only so it needs no privileged helper. Battery is the short
/// one for the obvious reason — the cost is somebody's charge — and 30 minutes is
/// about the length of the thing a battery-backed share actually is: showing
/// someone a page.
///
/// **These are shorter than the share TTLs they cap** (see
/// [`DEFAULT_SHARING_PEER_TTL_MINUTES`], 4h peer / 2h web), and that is the
/// deliberate shape rather than an oversight. The hold's deadline is
/// `min(cap, latest share expiry)`, so on a default machine the *cap* is what ends
/// it: veld holds the hardware awake for as long as it is willing to do so
/// unasked, and the link then survives on its own for whatever remains — reachable
/// while the machine is up, and no longer keeping it up. An earlier revision set
/// the mains cap equal to the peer TTL, which made the two deadlines coincide and
/// left which one "won" decided by reconcile latency; that is the ambiguity
/// `sharing_bound_by_share` and its 60-second material gap exist to keep out of
/// the UI.
pub const DEFAULT_KEEP_AWAKE_SHARING_ON_POWER_MINUTES: i64 = 120;
pub const DEFAULT_KEEP_AWAKE_SHARING_ON_BATTERY_MINUTES: i64 = 30;

/// How long a share link lives, by default, per sharing mode.
///
/// **These are the numbers that actually end a default share**, and until they
/// were settings they were two `const`s inside the daemon's share API with no
/// surface at all. That mattered more than it looked: the automatic keep-awake's
/// deadline is `min(cap, latest share expiry)`, so whichever of the two is
/// shorter is the one a countdown is really reporting — and when the share was
/// always the shorter one, "keep this machine awake while sharing, for at most 4
/// hours" counted down from 2h with nothing saying why. Making the other half of
/// that `min` configurable is what lets the two be reasoned about together.
///
/// A peer link now outlives the **mains** keep-awake cap
/// ([`DEFAULT_KEEP_AWAKE_SHARING_ON_POWER_MINUTES`], 2h) rather than coinciding
/// with it, so on a default machine the *cap* is what ends the hold and the share
/// stays reachable afterwards for as long as the machine happens to be up. That
/// is the intended shape: the cap bounds what veld does to somebody's hardware
/// without being asked, and the link's life is a separate question about how long
/// a colleague has to open it.
///
/// Web is the shorter one for the reason it always was (§6.1): its audience is
/// the open internet, so an idle share should die sooner.
pub const DEFAULT_SHARING_PEER_TTL_MINUTES: i64 = 240;
pub const DEFAULT_SHARING_WEB_TTL_MINUTES: i64 = 120;

/// Bounds on a **stored default** share TTL.
///
/// Numerically equal to the keep-awake pair today and deliberately *not* the same
/// constants: these bound how long a share link lives, that pair bounds how long
/// a machine is held awake, and a future change to one must not silently move the
/// other. The floor is five minutes because a share nobody can finish opening is
/// not a share; the ceiling is a working day because this is the value applied to
/// **every** share without anybody typing it. `veld share --ttl` keeps the
/// deliberate exception: no upper bound at all, floored only at 60 seconds so it
/// cannot mint a share that expired before its link could be opened.
pub const MIN_SHARE_TTL_MINUTES: i64 = 5;
pub const MAX_SHARE_TTL_MINUTES: i64 = 8 * 60;

/// The five keep-awake settings as one value. See [`Db::keep_awake`].
///
/// The two `sharing_*` pairs govern a hold **nobody asked for**; `manual_on_battery`
/// governs how far a hold somebody *did* ask for is allowed to reach. Keeping them
/// in one struct is what makes that asymmetry visible at every call site, because
/// the rule that falls out of it is the feature's load-bearing one: an automatic
/// hold never asks the privileged helper for anything, on either power source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepAwakePrefs {
    pub sharing_on_power: bool,
    pub sharing_on_power_minutes: i64,
    pub sharing_on_battery: bool,
    pub sharing_on_battery_minutes: i64,
    pub manual_on_battery: bool,
}

/// The five `backup.*` settings as one value. See [`Db::backup_prefs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPrefs {
    pub enabled: bool,
    pub interval_minutes: i64,
    pub keep: i64,
    pub keep_daily: i64,
    /// Where artifacts are written — the configured `backup.dir` if it is an
    /// absolute path, else the derived default. `None` only when this platform
    /// reports no data directory at all.
    pub dir: Option<std::path::PathBuf>,
}

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

/// Most entries `files.viewPatterns` may hold, and the longest one pattern may be.
///
/// Same reasoning as the exempt list above — this document is re-read by every
/// client on every window focus — with one addition specific to globs: each pattern
/// is matched against every candidate path in a recency scan, so the cap bounds
/// that product rather than only the document's size.
pub const MAX_VIEW_PATTERNS: usize = 64;
/// See [`MAX_VIEW_PATTERNS`]. A path is 4096 bytes on Linux and 1024 on macOS; a
/// *pattern* long enough to need more than a quarter of the smaller one is not a
/// pattern, it is a path someone pasted.
const MAX_VIEW_PATTERN_LEN: usize = 256;

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

/// Longest accepted `worktree.storageDir`. Generous — it is a filesystem path,
/// not a hostname — but still bounded, for the same reason every stored string
/// in this file is: the whole document round-trips through every client on
/// every window focus.
const MAX_WORKTREE_STORAGE_DIR_LEN: usize = 1024;

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

    /// The stored spellings — see [`ConfigSource::MAIN`] for why these are named.
    pub const LOCAL: &'static str = "local";
    pub const UTC: &'static str = "utc";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => Self::LOCAL,
            Self::Utc => Self::UTC,
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
    TerminalShell,
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
    TerminalShellIntegration,
    TerminalAgentIntegration,
    KeepAwakeSharingOnPower,
    KeepAwakeSharingOnPowerMinutes,
    KeepAwakeSharingOnBattery,
    KeepAwakeSharingOnBatteryMinutes,
    KeepAwakeManualOnBattery,
    SharingPeerTtlMinutes,
    SharingWebTtlMinutes,
    ExtensionsAutoRefresh,
    ExtensionsSource,
    NewsSource,
    ActivityShowWorking,
    ActivityNotifyCommandFinished,
    ActivityNotifyCommandFailed,
    ActivityNotifyAgentWaiting,
    ActivityNotifyNoticed,
    ActivityNotifyAgentFinished,
    WorktreeMarkerStyle,
    WorktreeTrashRetention,
    RunsHistoryDays,
    LogsTimeZone,
    BrowserQuickSwitchResponsive,
    BrowserQuickSwitchColorScheme,
    BrowserExternalOrigins,
    BrowserSearchUrl,
    FilesViewWebPages,
    FilesViewImages,
    FilesViewPdfs,
    FilesViewPlainText,
    FilesViewPatterns,
    FilesWatchByDefault,
    UiHideDisabledActions,
    UiShowProjectNews,
    UiShowProjectColumn,
    GitCreateFrom,
    WorktreeStorageMode,
    WorktreeStorageDir,
    FocusModeEnabled,
    FocusModeSuppressBell,
    FocusModeSuppressToasts,
    FocusModeSuppressOsNotifications,
    BackupEnabled,
    BackupIntervalMinutes,
    BackupKeep,
    BackupKeepDaily,
    BackupDir,
    FeedbackSuppressOverlay,
    DesktopMenuBarIcon,
    Unknown(String),
}

impl SettingKey {
    /// Every known key, **in the order a surface should present them**.
    ///
    /// Exists so tests can enumerate the variants, which is the only thing that
    /// catches the two silent misses when a key is added: [`Self::parse`] ends in an
    /// `other => Unknown` catch-all, so a forgotten arm makes the key *unvalidated*
    /// rather than a compile error — and a forgotten [`defaults`] entry makes the
    /// effective document incomplete, at which point TypeScript's `FALLBACK`
    /// silently becomes the real default, the exact Rust↔TS drift this module's docs
    /// claim is impossible. `as_str` and `validate` are exhaustive matches and need
    /// no help; these two do.
    ///
    /// **The order is load-bearing and is grouped, not alphabetical.** It runs
    /// group by group in [`SettingGroup::ALL`] order and, inside a group, section
    /// by section in the order those headings appear — because
    /// [`catalog`](super::settings_catalog::catalog) walks this list and the
    /// settings dialog renders whatever it is handed. Making this *the* display
    /// order is what keeps a new setting from needing a second list somewhere to
    /// say where it goes; the cost is that inserting one means inserting it in
    /// the right place here rather than at the end.
    pub const ALL: &'static [SettingKey] = &[
        // ── General ──────────────────────────────────────────────────────────
        Self::ExtensionsAutoRefresh,
        Self::ExtensionsSource,
        Self::WorktreeMarkerStyle,
        Self::WorktreeTrashRetention,
        Self::RunsHistoryDays,
        Self::LogsTimeZone,
        Self::UiHideDisabledActions,
        Self::UiShowProjectColumn,
        Self::UiShowProjectNews,
        Self::NewsSource,
        Self::FeedbackSuppressOverlay,
        Self::DesktopMenuBarIcon,
        // ── General › Database backups ───────────────────────────────────────
        Self::BackupEnabled,
        Self::BackupIntervalMinutes,
        Self::BackupKeep,
        Self::BackupKeepDaily,
        Self::BackupDir,
        // ── Git ──────────────────────────────────────────────────────────────
        Self::GitCreateFrom,
        Self::WorktreeStorageMode,
        Self::WorktreeStorageDir,
        // ── Terminal › Appearance ────────────────────────────────────────────
        Self::TerminalFontSize,
        Self::TerminalFontFamily,
        Self::TerminalCursorStyle,
        Self::TerminalCursorBlink,
        // ── Terminal › Behaviour ─────────────────────────────────────────────
        Self::TerminalShell,
        Self::TerminalScrollback,
        Self::TerminalShiftEnterNewline,
        Self::TerminalBellVolume,
        Self::TerminalDetachGrace,
        // ── Terminal › Auto-reconnect ────────────────────────────────────────
        Self::TerminalReconnectTries,
        Self::TerminalReconnectFirstDelaySeconds,
        Self::TerminalReconnectBackoffSeconds,
        // ── Activity › Noticing ──────────────────────────────────────────────
        Self::TerminalShellIntegration,
        Self::TerminalAgentIntegration,
        Self::ActivityShowWorking,
        // ── Activity › Notifying ─────────────────────────────────────────────
        Self::ActivityNotifyCommandFinished,
        Self::ActivityNotifyCommandFailed,
        Self::ActivityNotifyAgentWaiting,
        // `AgentFinished` before `Noticed`, matching the screen this list replaced
        // — and not merely for continuity: `activity.notifyNoticed`'s help says
        // "Its own row rather than the agent one above", which is false the moment
        // these two swap. A help string that refers to a neighbour is a constraint
        // on this order, not just prose.
        Self::ActivityNotifyAgentFinished,
        Self::ActivityNotifyNoticed,
        // ── Activity › Focus mode ────────────────────────────────────────────
        Self::FocusModeEnabled,
        Self::FocusModeSuppressBell,
        Self::FocusModeSuppressToasts,
        Self::FocusModeSuppressOsNotifications,
        // ── Keep awake › While you're sharing ────────────────────────────────
        Self::KeepAwakeSharingOnPower,
        Self::KeepAwakeSharingOnPowerMinutes,
        Self::KeepAwakeSharingOnBattery,
        Self::KeepAwakeSharingOnBatteryMinutes,
        // ── Keep awake › When you ask ────────────────────────────────────────
        Self::KeepAwakeManualOnBattery,
        // ── Sharing ──────────────────────────────────────────────────────────
        // Directly after keep-awake, because the pair above is capped by
        // `min(cap, share expiry)` and these two are the other half of that min.
        Self::SharingPeerTtlMinutes,
        Self::SharingWebTtlMinutes,
        // ── Links ────────────────────────────────────────────────────────────
        Self::TerminalOpenUrlsInApp,
        Self::TerminalInterceptSystemOpen,
        Self::BrowserExternalOrigins,
        // ── Browser panes ────────────────────────────────────────────────────
        Self::BrowserQuickSwitchResponsive,
        Self::BrowserQuickSwitchColorScheme,
        Self::BrowserSearchUrl,
        // ── Browser panes › Local files ──────────────────────────────────────
        // The four groups in the order a reader meets them: the case the feature
        // exists for first, the escape hatch last.
        Self::FilesViewWebPages,
        Self::FilesViewImages,
        Self::FilesViewPdfs,
        Self::FilesViewPlainText,
        Self::FilesViewPatterns,
        Self::FilesWatchByDefault,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            Self::TerminalShell => "terminal.shell",
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
            Self::TerminalShellIntegration => "terminal.shellIntegration",
            Self::TerminalAgentIntegration => "terminal.agentIntegration",
            Self::ExtensionsAutoRefresh => "extensions.autoRefresh",
            Self::ExtensionsSource => "extensions.source",
            Self::NewsSource => "news.source",
            Self::ActivityShowWorking => "activity.showWorking",
            Self::ActivityNotifyCommandFinished => "activity.notifyCommandFinished",
            Self::ActivityNotifyCommandFailed => "activity.notifyCommandFailed",
            Self::ActivityNotifyAgentWaiting => "activity.notifyAgentWaiting",
            Self::ActivityNotifyNoticed => "activity.notifyNoticed",
            Self::ActivityNotifyAgentFinished => "activity.notifyAgentFinished",
            Self::WorktreeMarkerStyle => "worktree.markerStyle",
            Self::WorktreeTrashRetention => "worktree.trashRetentionDays",
            Self::RunsHistoryDays => "runs.historyDays",
            Self::LogsTimeZone => "logs.timeZone",
            Self::BrowserQuickSwitchResponsive => "browser.quickSwitch.responsive",
            Self::BrowserQuickSwitchColorScheme => "browser.quickSwitch.colorScheme",
            Self::BrowserExternalOrigins => "browser.externalOrigins",
            Self::BrowserSearchUrl => "browser.searchUrl",
            Self::FilesViewWebPages => "files.viewWebPages",
            Self::FilesViewImages => "files.viewImages",
            Self::FilesViewPdfs => "files.viewPdfs",
            Self::FilesViewPlainText => "files.viewPlainText",
            Self::FilesViewPatterns => "files.viewPatterns",
            Self::FilesWatchByDefault => "files.watchByDefault",
            Self::UiHideDisabledActions => "ui.hideDisabledActions",
            Self::UiShowProjectNews => "ui.showProjectNews",
            Self::UiShowProjectColumn => "ui.showProjectColumn",
            Self::GitCreateFrom => "git.createFrom",
            Self::WorktreeStorageMode => "worktree.storageMode",
            Self::WorktreeStorageDir => "worktree.storageDir",
            Self::FocusModeEnabled => "focus.enabled",
            Self::FocusModeSuppressBell => "focus.suppressBell",
            Self::FocusModeSuppressToasts => "focus.suppressToasts",
            Self::FocusModeSuppressOsNotifications => "focus.suppressOsNotifications",
            Self::KeepAwakeSharingOnPower => "keepAwake.sharingOnPower",
            Self::KeepAwakeSharingOnPowerMinutes => "keepAwake.sharingOnPowerMinutes",
            Self::KeepAwakeSharingOnBattery => "keepAwake.sharingOnBattery",
            Self::KeepAwakeSharingOnBatteryMinutes => "keepAwake.sharingOnBatteryMinutes",
            Self::KeepAwakeManualOnBattery => "keepAwake.manualOnBattery",
            Self::SharingPeerTtlMinutes => "sharing.peerTtlMinutes",
            Self::SharingWebTtlMinutes => "sharing.webTtlMinutes",
            Self::BackupEnabled => "backup.enabled",
            Self::BackupIntervalMinutes => "backup.intervalMinutes",
            Self::BackupKeep => "backup.keep",
            Self::BackupKeepDaily => "backup.keepDaily",
            Self::BackupDir => "backup.dir",
            Self::FeedbackSuppressOverlay => "feedback.suppressOverlay",
            Self::DesktopMenuBarIcon => "desktop.menuBarIcon",
            Self::Unknown(k) => k,
        }
    }

    pub fn parse(key: &str) -> Self {
        match key {
            "terminal.shell" => Self::TerminalShell,
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
            "terminal.shellIntegration" => Self::TerminalShellIntegration,
            "terminal.agentIntegration" => Self::TerminalAgentIntegration,
            "extensions.autoRefresh" => Self::ExtensionsAutoRefresh,
            "extensions.source" => Self::ExtensionsSource,
            "news.source" => Self::NewsSource,
            "activity.showWorking" => Self::ActivityShowWorking,
            "activity.notifyCommandFinished" => Self::ActivityNotifyCommandFinished,
            "activity.notifyCommandFailed" => Self::ActivityNotifyCommandFailed,
            "activity.notifyAgentWaiting" => Self::ActivityNotifyAgentWaiting,
            "activity.notifyNoticed" => Self::ActivityNotifyNoticed,
            "activity.notifyAgentFinished" => Self::ActivityNotifyAgentFinished,
            "worktree.markerStyle" => Self::WorktreeMarkerStyle,
            "worktree.trashRetentionDays" => Self::WorktreeTrashRetention,
            "runs.historyDays" => Self::RunsHistoryDays,
            "logs.timeZone" => Self::LogsTimeZone,
            "browser.quickSwitch.responsive" => Self::BrowserQuickSwitchResponsive,
            "browser.quickSwitch.colorScheme" => Self::BrowserQuickSwitchColorScheme,
            "browser.externalOrigins" => Self::BrowserExternalOrigins,
            "browser.searchUrl" => Self::BrowserSearchUrl,
            "files.viewWebPages" => Self::FilesViewWebPages,
            "files.viewImages" => Self::FilesViewImages,
            "files.viewPdfs" => Self::FilesViewPdfs,
            "files.viewPlainText" => Self::FilesViewPlainText,
            "files.viewPatterns" => Self::FilesViewPatterns,
            "files.watchByDefault" => Self::FilesWatchByDefault,
            "ui.hideDisabledActions" => Self::UiHideDisabledActions,
            "ui.showProjectNews" => Self::UiShowProjectNews,
            "ui.showProjectColumn" => Self::UiShowProjectColumn,
            "git.createFrom" => Self::GitCreateFrom,
            "worktree.storageMode" => Self::WorktreeStorageMode,
            "worktree.storageDir" => Self::WorktreeStorageDir,
            "focus.enabled" => Self::FocusModeEnabled,
            "focus.suppressBell" => Self::FocusModeSuppressBell,
            "focus.suppressToasts" => Self::FocusModeSuppressToasts,
            "focus.suppressOsNotifications" => Self::FocusModeSuppressOsNotifications,
            "keepAwake.sharingOnPower" => Self::KeepAwakeSharingOnPower,
            "keepAwake.sharingOnPowerMinutes" => Self::KeepAwakeSharingOnPowerMinutes,
            "keepAwake.sharingOnBattery" => Self::KeepAwakeSharingOnBattery,
            "keepAwake.sharingOnBatteryMinutes" => Self::KeepAwakeSharingOnBatteryMinutes,
            "keepAwake.manualOnBattery" => Self::KeepAwakeManualOnBattery,
            "sharing.peerTtlMinutes" => Self::SharingPeerTtlMinutes,
            "sharing.webTtlMinutes" => Self::SharingWebTtlMinutes,
            "backup.enabled" => Self::BackupEnabled,
            "backup.intervalMinutes" => Self::BackupIntervalMinutes,
            "backup.keep" => Self::BackupKeep,
            "backup.keepDaily" => Self::BackupKeepDaily,
            "backup.dir" => Self::BackupDir,
            "feedback.suppressOverlay" => Self::FeedbackSuppressOverlay,
            "desktop.menuBarIcon" => Self::DesktopMenuBarIcon,
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
    pub(super) fn validate(&self, value: &Value) -> Result<Value, DbError> {
        let bad = || DbError::InvalidSetting {
            key: self.as_str().to_string(),
            value: value.to_string(),
            reason: None,
        };
        // For the validators that already produce a sentence. Pass theirs along
        // rather than collapsing it — see `DbError::InvalidSetting`.
        let because = |reason: String| DbError::InvalidSetting {
            key: self.as_str().to_string(),
            value: value.to_string(),
            reason: Some(reason),
        };
        Ok(match self {
            Self::TerminalFontSize => {
                Value::from(clamp_i64(value, MIN_FONT_SIZE, MAX_FONT_SIZE).ok_or_else(bad)?)
            }
            Self::TerminalScrollback => {
                Value::from(clamp_i64(value, MIN_SCROLLBACK, MAX_SCROLLBACK).ok_or_else(bad)?)
            }
            Self::TerminalBellVolume => {
                Value::from(clamp_i64(value, MIN_BELL_VOLUME, MAX_BELL_VOLUME).ok_or_else(bad)?)
            }
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
            Self::RunsHistoryDays => Value::from(
                clamp_i64(value, MIN_RUN_HISTORY_DAYS, MAX_RUN_HISTORY_DAYS).ok_or_else(bad)?,
            ),
            Self::TerminalCursorBlink
            | Self::TerminalShiftEnterNewline
            | Self::TerminalOpenUrlsInApp
            | Self::TerminalInterceptSystemOpen
            | Self::TerminalShellIntegration
            | Self::TerminalAgentIntegration
            | Self::ExtensionsAutoRefresh
            | Self::ActivityShowWorking
            | Self::ActivityNotifyCommandFinished
            | Self::ActivityNotifyCommandFailed
            | Self::ActivityNotifyAgentWaiting
            | Self::ActivityNotifyNoticed
            | Self::ActivityNotifyAgentFinished
            | Self::BrowserQuickSwitchResponsive
            | Self::BrowserQuickSwitchColorScheme
            | Self::UiHideDisabledActions
            | Self::UiShowProjectNews
            | Self::UiShowProjectColumn
            | Self::FocusModeEnabled
            | Self::FocusModeSuppressBell
            | Self::FocusModeSuppressToasts
            | Self::FocusModeSuppressOsNotifications
            | Self::KeepAwakeSharingOnPower
            | Self::KeepAwakeSharingOnBattery
            | Self::KeepAwakeManualOnBattery
            | Self::BackupEnabled
            | Self::FeedbackSuppressOverlay
            | Self::DesktopMenuBarIcon
            | Self::FilesViewWebPages
            | Self::FilesViewImages
            | Self::FilesViewPdfs
            | Self::FilesViewPlainText
            | Self::FilesWatchByDefault => Value::from(value.as_bool().ok_or_else(bad)?),
            // Clamped like every other duration here. The daemon acts on this one
            // — it is the period of its own timer — so it is normalised at the
            // store rather than trusted from the wire.
            Self::BackupIntervalMinutes => Value::from(
                clamp_i64(
                    value,
                    MIN_BACKUP_INTERVAL_MINUTES,
                    MAX_BACKUP_INTERVAL_MINUTES,
                )
                .ok_or_else(bad)?,
            ),
            // No off switch inside the range: `backup.enabled` is the off switch,
            // and a `keep` of zero would mean the daemon writes a backup and then
            // immediately deletes it — work with no artifact at the end of it.
            Self::BackupKeep => {
                Value::from(clamp_i64(value, MIN_BACKUP_KEEP, MAX_BACKUP_KEEP).ok_or_else(bad)?)
            }
            // Zero *is* inside the range here, and means "no daily tail" — the same
            // shape as `worktree.trashRetentionDays`: a user turning the tail off
            // must not have their zero clamped up into turning it on. Unlike that
            // key, zero is also the range's own floor, so no special case is needed.
            Self::BackupKeepDaily => Value::from(
                clamp_i64(value, MIN_BACKUP_KEEP_DAILY, MAX_BACKUP_KEEP_DAILY).ok_or_else(bad)?,
            ),
            // Same rules and same reasoning as `worktree.storageDir`: a filesystem
            // path, so only length, control characters and `..` are refused, and
            // empty is a real value meaning "the derived default".
            //
            // `..` matters more here than it does there. This directory is the one
            // veld *deletes files from* on a timer, so a stored `../..` would have
            // retention pruning walk somewhere the user never pointed at. Pruning
            // refuses to delete anything that does not match veld's own artifact
            // name pattern, which is the real guard — this is the shape being
            // caught at the point somebody chose it.
            Self::BackupDir => {
                let s = value.as_str().ok_or_else(bad)?.trim();
                if s.len() > MAX_BACKUP_DIR_LEN {
                    return Err(because(format!(
                        "is longer than {MAX_BACKUP_DIR_LEN} bytes"
                    )));
                }
                if s.chars().any(char::is_control) {
                    return Err(because("must not contain control characters".into()));
                }
                if !s.is_empty() {
                    let p = std::path::Path::new(s);
                    if !p.is_absolute() {
                        return Err(because(
                            "must be an absolute path (or empty, for the default)".into(),
                        ));
                    }
                    if p.components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    {
                        return Err(because("must not contain ..".into()));
                    }
                }
                Value::from(s)
            }
            // Clamped, not enumerated, even though the dialog offers a fixed list:
            // the cap is a duration like every other number here, and a client that
            // sends 45 should get 45 rather than a rejection it cannot act on.
            // The floor is not zero — a cap of zero would arm a hold and drop it on
            // the same tick, which is what turning the switch *off* already means.
            Self::KeepAwakeSharingOnPowerMinutes | Self::KeepAwakeSharingOnBatteryMinutes => {
                Value::from(
                    clamp_i64(value, MIN_KEEP_AWAKE_MINUTES, MAX_KEEP_AWAKE_MINUTES)
                        .ok_or_else(bad)?,
                )
            }
            // Clamped like the pair above, and against their own bounds — see
            // `MIN_SHARE_TTL_MINUTES`. `veld share --ttl` is a separate path on
            // purpose, with no upper bound (only a 60s floor): this is the number
            // applied to every share with nobody typing it, and that one is a
            // number somebody typed.
            Self::SharingPeerTtlMinutes | Self::SharingWebTtlMinutes => Value::from(
                clamp_i64(value, MIN_SHARE_TTL_MINUTES, MAX_SHARE_TTL_MINUTES).ok_or_else(bad)?,
            ),
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
                    if raw.len() > MAX_ORIGIN_LEN {
                        return Err(because(format!(
                            "{raw:?} is longer than {MAX_ORIGIN_LEN} bytes"
                        )));
                    }
                    // `parse_origin`'s own message names the entry, which matters
                    // here in a way it does not for a scalar key: the value being
                    // refused is a list, so "not valid" without saying *which*
                    // one leaves the author re-reading sixty-four patterns.
                    if let Err(why) = crate::ide::parse_origin(raw) {
                        return Err(because(format!("{raw:?}: {why}")));
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
                Value::from(parse_search_template(s).map_err(because)?)
            }
            // Extra globs naming files a pane may open. Validated by shape only:
            // there is no such thing as an unparseable glob in
            // `veld_core::files::glob_matches` — every byte that is not `*` or `?`
            // matches itself — so the failures worth refusing are a pattern that is
            // *too big to be one* and a pattern that cannot match anything.
            //
            // Empty entries are dropped rather than refused. A text-list control
            // produces one for every stray blank line, and refusing the whole patch
            // over a blank line is a text field that fights its author.
            //
            // `..` is refused as defence in depth, exactly as in
            // `worktree.storageDir` below: a grant already confines every read to
            // one worktree root, so a `..` here cannot escape — but a *stored*
            // pattern has no legitimate need for one, and catching the shape where
            // somebody chose it beats catching it only where it is used.
            Self::FilesViewPatterns => {
                let items = value.as_array().ok_or_else(bad)?;
                if items.len() > MAX_VIEW_PATTERNS {
                    return Err(because(format!("more than {MAX_VIEW_PATTERNS} patterns")));
                }
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let raw = item.as_str().ok_or_else(bad)?.trim();
                    if raw.is_empty() {
                        continue;
                    }
                    if raw.len() > MAX_VIEW_PATTERN_LEN {
                        return Err(because(format!(
                            "{raw:?} is longer than {MAX_VIEW_PATTERN_LEN} bytes"
                        )));
                    }
                    if raw.chars().any(char::is_control) {
                        return Err(because(format!("{raw:?} contains a control character")));
                    }
                    if raw.split('/').any(|seg| seg == "..") {
                        return Err(because(format!("{raw:?} must not contain ..")));
                    }
                    if !out.iter().any(|v| v == &Value::from(raw)) {
                        out.push(Value::from(raw));
                    }
                }
                Value::Array(out)
            }
            Self::TerminalCursorStyle => one_of(value, CURSOR_STYLES).ok_or_else(bad)?,
            // Which shell a terminal opens. Validated by **shape** — `"auto"` or an
            // absolute path — and never by existence, which is
            // `veld_core::shell::resolve`'s job at spawn time: a value must be
            // storable while its shell is mid-install, and a shell that is later
            // uninstalled must not leave a user unable to open the terminal they
            // would fix the setting from. Rejected rather than coerced, like every
            // enum here: the daemon acts on this directly, so a stored value it
            // would silently ignore is worse than a refused write.
            Self::TerminalShell => {
                let s = value.as_str().ok_or_else(bad)?.trim();
                if !crate::shell::is_valid_preference(s) {
                    return Err(because(format!(
                        "must be {:?} or an absolute path to a shell — a bare name would be \
                         looked up on the daemon's own PATH, which is not yours",
                        crate::shell::AUTO
                    )));
                }
                Value::from(s)
            }
            Self::WorktreeMarkerStyle => one_of(value, MARKER_STYLES).ok_or_else(bad)?,
            // Where a *new* worktree's branch is cut from. Rejected rather than
            // coerced (same as the other enums here): the daemon acts on this
            // directly in `create_worktree`, so a stored value neither surface
            // honours would silently change where branches come from.
            Self::GitCreateFrom => one_of(value, GIT_CREATE_SOURCES).ok_or_else(bad)?,
            // Which checkout's `veld.json` `ide.extensions`/`ide.news` declarations
            // are read from. Rejected rather than coerced, same reason as every
            // other enum here: the daemon acts on this directly (`worktree_target`
            // in `veld-daemon/src/extensions.rs`, `repo_view` in `desktop.rs`).
            Self::ExtensionsSource | Self::NewsSource => {
                let s = value.as_str().ok_or_else(bad)?;
                Value::from(ConfigSource::parse(s).ok_or_else(bad)?.as_str())
            }
            // Where a *new* worktree's checkout lands. Rejected rather than
            // coerced, same reason as `GitCreateFrom` just above: the daemon acts
            // on this directly in `create_worktree`.
            Self::WorktreeStorageMode => one_of(value, WORKTREE_STORAGE_MODES).ok_or_else(bad)?,
            // A filesystem path, not a hostname or a CSS value — the two other
            // free-text keys in this file bound their characters because of where
            // the string is interpolated (a search URL, a stylesheet rule). This
            // one has no such trap: it becomes a `PathBuf` and is joined with an
            // alias, never parsed or rendered as markup. Only length and control
            // characters are worth refusing on that basis alone; everything else
            // a filesystem accepts is fine here too.
            //
            // `..` is refused for a different reason: the daemon's own
            // `canonicalize_prefix` (`veld-daemon/src/desktop.rs`) already
            // resolves one lexically before comparing a checkout path against
            // every repo root, so a stored `..` cannot bypass that check —
            // this is defence in depth, catching the shape at the point
            // someone chose it rather than only at the point it is used, and
            // there is no legitimate reason a *stored* directory needs one:
            // every real one normalises to a cleaner absolute path anyway.
            //
            // Empty is a real value — "custom mode is chosen but no folder was
            // picked yet" — and the daemon's `worktree_storage_dir()` reads that
            // as "fall back to the sibling default", so it must not be forced to
            // look absolute like a real choice would.
            Self::WorktreeStorageDir => {
                let s = value.as_str().ok_or_else(bad)?.trim();
                if s.len() > MAX_WORKTREE_STORAGE_DIR_LEN {
                    return Err(because(format!(
                        "is longer than {MAX_WORKTREE_STORAGE_DIR_LEN} bytes"
                    )));
                }
                if s.chars().any(char::is_control) {
                    return Err(because("must not contain control characters".into()));
                }
                if !s.is_empty() {
                    let p = std::path::Path::new(s);
                    if !p.is_absolute() {
                        return Err(because(
                            "must be an absolute path (or empty, for the default)".into(),
                        ));
                    }
                    if p.components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    {
                        return Err(because("must not contain ..".into()));
                    }
                }
                Value::from(s)
            }
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
                if s.is_empty() {
                    return Err(because("must name at least one font family".into()));
                }
                if s.len() > MAX_FONT_FAMILY_LEN {
                    return Err(because(format!(
                        "is longer than {MAX_FONT_FAMILY_LEN} bytes"
                    )));
                }
                if s.contains(['{', '}', ';', '<', '>', '\n', '\r']) {
                    return Err(because(
                        "must not contain { } ; < > or a newline — this ends up inside a CSS \
                         rule, so those characters could escape it"
                            .into(),
                    ));
                }
                Value::from(s)
            }
            // Preserved, but bounded — see MAX_UNKNOWN_* above.
            Self::Unknown(k) => {
                if k.len() > MAX_UNKNOWN_KEY_LEN {
                    return Err(because(format!(
                        "this build does not know this setting, and an unrecognised key may not \
                         be longer than {MAX_UNKNOWN_KEY_LEN} bytes"
                    )));
                }
                if serde_json::to_string(value)?.len() > MAX_UNKNOWN_VALUE_LEN {
                    return Err(because(format!(
                        "this build does not know this setting, and an unrecognised value may \
                         not be longer than {MAX_UNKNOWN_VALUE_LEN} bytes"
                    )));
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
    // `/ ? #` and `:` are already gone (the host span was split on the first three, and
    // on the colon just above); they stay for the reader, since this list is meant to be
    // read as "what a host may not hold".
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

/// Accept a string that is one of a catalog choice list's values.
///
/// Takes the *same* `&'static [Choice]` slice the catalog offers rather than a
/// hand-written `&["block", "underline", "bar"]` beside it. That is the whole
/// point of the slice being shared: the set a client is told it may pick from and
/// the set the daemon will accept are one literal, so they cannot agree today and
/// disagree after somebody adds a fourth cursor style to one of them.
fn one_of(value: &Value, allowed: &[Choice]) -> Option<Value> {
    let s = value.as_str()?;
    allowed
        .iter()
        .any(|choice| choice.value == s)
        .then(|| Value::from(s))
}

/// The one source of truth for every default.
///
/// [`Db::settings`] merges these under whatever is stored, so a client always
/// receives a complete document and never needs a default of its own.
pub fn defaults() -> BTreeMap<String, Value> {
    [
        // `auto` — the user's login shell, which is the right answer for almost
        // everyone and is exactly the previous behaviour. The setting exists for
        // the minority whose login shell is not the shell they work in: macOS has
        // shipped zsh since Catalina, so a bash user's `~/.bashrc` aliases,
        // completions and tool integrations were loading in no veld terminal.
        (SettingKey::TerminalShell, Value::from(crate::shell::AUTO)),
        (SettingKey::TerminalFontSize, Value::from(DEFAULT_FONT_SIZE)),
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
        // point of these two is reach. A phone-sized viewport and the page's colour
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
        // On: the feature it feeds — the rail's unread badge — is worthless if the
        // events that fill it are opt-in, because nobody switches on a signal they
        // have never seen. What it costs is two `precmd`/`PROMPT_COMMAND` hooks that
        // print an escape sequence, in a shell veld is already in for
        // `terminal.interceptSystemOpen`. It is a separate key from that one because
        // the two are independent decisions: "put the shim directory on my PATH" and
        // "tell the window when a command ended" are not the same permission, and the
        // first version made shell integration die whenever the *other* switch was
        // off — a coupling nothing in either setting's documentation implied.
        (SettingKey::TerminalShellIntegration, Value::from(true)),
        // On, for the same reason, and with the same independence. This one is the
        // more invasive of the two — it puts a `claude` shim on `PATH` and hands the
        // agent an ephemeral `--settings` file — so it is worth being able to turn off
        // on its own, without losing the OSC 133 half that has nothing to do with
        // agents. Nothing of the user's is edited either way: no
        // `~/.claude/settings.json` merge, ever.
        (SettingKey::TerminalAgentIntegration, Value::from(true)),
        // On: a project that declares badges declared them to be seen, and the
        // switch exists for the machine that wants none rather than as a gate
        // everybody steps through. See `Db::extensions_auto_refresh`.
        (SettingKey::ExtensionsAutoRefresh, Value::from(true)),
        // `main` — see `ConfigSource`. Reverses the "extensions are worktree-based"
        // decision (`docs/extensions-vision.md`, 2026-08-13): a worktree cloned
        // before a project's `veld.json` gained `ide.extensions` showed nothing
        // until re-cloned. `worktree` restores the old default, for testing a
        // declaration before it merges.
        (SettingKey::ExtensionsSource, Value::from("main")),
        // `main` — the behaviour `ide.news` already had (`repo_view` in
        // `desktop.rs`), now a setting rather than hardcoded, so it can be flipped
        // to `worktree` to preview a card before merging it.
        (SettingKey::NewsSource, Value::from("main")),
        // **On.** This default has now moved twice — on, then off on evidence from
        // real use, and now on again as a maintainer call. The reasoning that took
        // it off still stands and is worth keeping rather than deleting, because it
        // is what this switch's accuracy actually depends on:
        //
        // The signal is only as good as its producers, and they are uneven. A plain
        // shell command is exact — a start mark with no end mark yet genuinely means
        // "running here". A coding agent is not: no *installed* hook reports `Working`
        // (the one that did, `SessionStart`, was blocking and set the state once, so an
        // idle agent spun forever), and an agent veld has no installer for reports
        // nothing at all. So the spinner is authoritative for builds, absent for
        // supported agents, and meaningless for unsupported ones.
        //
        // The judgement that changed is what to do about that: an absent spinner is a
        // missing hint rather than a wrong one, and the build case — "is that still
        // going?" — is common enough to be worth showing everybody. It also loses to
        // every unseen event in the rail, so a worktree waiting for you still reads as
        // waiting rather than as working. `PostToolUse` (see `veld_core::agent`) is
        // still what would make it accurate for agents; this does not wait for it.
        (SettingKey::ActivityShowWorking, Value::from(true)),
        // The notification table. Four rows and not one switch, because "a command
        // finished" and "a coding agent is waiting for you" are not the same event and a
        // single answer for both is wrong in one direction or the other.
        //
        // Every one of these fires **only while Veld is not the focused window** — the
        // rule the OSC 9 notification path already used before this table existed.
        //
        // Off, and the only one that is: a finished command is news, and the rail already
        // carries it. Interrupting a user in another application to say a build they
        // walked away from succeeded is the definition of a notification people turn off
        // wholesale — taking the two rows below with it.
        (
            SettingKey::ActivityNotifyCommandFinished,
            Value::from(false),
        ),
        // On: a failed build is the one "it ended" event that is actionable, and finding
        // out twenty minutes later is the cost this exists to remove.
        (SettingKey::ActivityNotifyCommandFailed, Value::from(true)),
        // On: an agent stopped at a permission prompt or a question is the single most
        // actionable thing this whole feature detects.
        (SettingKey::ActivityNotifyAgentWaiting, Value::from(true)),
        // An OSC 9 notification — a program asking to be noticed. Its own row rather
        // than riding the agent one, because the agent row's label has to be able to
        // say what it covers and OSC 9 is emitted by anything, agent or not.
        //
        // On, and this is the row that keeps a shipped behaviour rather than adding one:
        // an OSC 9 already raised a system banner before this table existed, with no
        // setting to govern it at all. A default of `false` here would have made this
        // change silently switch that off.
        //
        // This is also where "something is reading from stdin" would land if veld ever
        // learns to see it. It cannot today: that is not observable from the browser at
        // all, and not portably observable anywhere — there is no `/proc` on macOS, and
        // `tcgetpgrp` names the foreground process without saying it is blocked on a
        // read. It would be a holder-side, platform-specific producer.
        (SettingKey::ActivityNotifyNoticed, Value::from(true)),
        // On, at the maintainer's call, with the frequency stated: Claude Code's
        // end-of-turn notification fires after **every response**, not once per session,
        // so this is a banner each time an agent hands control back while you are
        // elsewhere. That is the point (you walked away; you want to be called back), and
        // it is also the row most likely to be turned off first.
        (SettingKey::ActivityNotifyAgentFinished, Value::from(true)),
        // Empty: veld ships no opinion about which hosts need the real browser.
        // A default entry would be a guess about someone else's SSO provider.
        (SettingKey::BrowserExternalOrigins, Value::Array(Vec::new())),
        // **Web pages only, and everything else off.** The list is ordered by
        // recency and lives on a screen with a handful of rows, so its value comes
        // entirely from being short: the file you want is the one an agent wrote a
        // moment ago, and every other candidate pushes it down. A repository has
        // vastly more images, PDFs and text files than generated documents — a
        // checked-in logo, a vendored diagram, every `README.md` — and none of them
        // are what somebody opens a pane for.
        //
        // So the default is the two groups a *generated document* arrives as, and
        // the other two are one switch away. This started with images on as well,
        // and a real repository answered it: the list filled with `website/logo.svg`
        // and a vendored diagram while the report that had just been written sat
        // below them. An image is nearly always a committed asset; an HTML file or a
        // PDF at the top of a recency list is nearly always something just made.
        (SettingKey::FilesViewWebPages, Value::from(true)),
        (SettingKey::FilesViewImages, Value::from(false)),
        (SettingKey::FilesViewPdfs, Value::from(true)),
        (SettingKey::FilesViewPlainText, Value::from(false)),
        // Empty for the same reason as the exempt list above: a default pattern is
        // a guess about a file type veld has never seen.
        (SettingKey::FilesViewPatterns, Value::Array(Vec::new())),
        // On: a pane opened on a file is nearly always a pane you are about to
        // watch an agent rewrite, and a stale deck that silently does not reload is
        // worse than a poll nobody notices.
        (SettingKey::FilesWatchByDefault, Value::from(true)),
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
        // Whether a project's own `ide.news` cards are shown at all. Veld's own
        // are not affected and have no such switch.
        //
        // On by default, because a project's first card has to reach somebody or
        // the channel does not exist — an opt-in news channel is a news channel
        // whose launch announcement nobody sees. It is a *user*-level switch
        // rather than a per-project one for the same reason: per-project consent
        // would have to be given before the first card, i.e. before there is any
        // reason to give it.
        //
        // It exists because this is the one surface where somebody other than
        // Veld can put a modal in front of the user. The caps in
        // `veld_core::ide` bound how much they can say; this is the reader's own
        // answer to being told anything at all. Read only by the IDE bundle.
        (SettingKey::UiShowProjectNews, Value::from(true)),
        // Where a new worktree's branch is cut from. `origin` is the point of
        // this setting: a worktree created from a stale local `main` is born
        // behind the remote — missing the latest DB migrations, conflicting with
        // open PRs — and it compounds, because nobody goes back to update `main`.
        // Fetch-then-base-on-origin makes each new worktree current at birth. The
        // daemon acts on this in `create_worktree`, so it is validated above like
        // `TerminalDetachGrace`, not trusted from the wire.
        (SettingKey::GitCreateFrom, Value::from("origin")),
        // Sibling of the repo — today's only behaviour, and the one that needs no
        // setup: a fresh install has never chosen a folder, so the default must
        // be the thing that already works.
        (SettingKey::WorktreeStorageMode, Value::from("sibling")),
        // Empty: meaningless in `sibling` mode, and in `custom` mode it is "chosen
        // custom but no folder yet" — see `WorktreeStorageDir`'s validator.
        (SettingKey::WorktreeStorageDir, Value::from("")),
        // Off. Focus mode is something a user turns on for a stretch of work, not
        // a standing posture — an install that silently suppressed its own bell
        // and banners from the first run would look like a notification bug, not
        // a feature nobody asked for yet.
        // Off. The project column is the multi-project surface, and most installs
        // have one project — for them a 44px column of a single square is cost with
        // no answer in it. It is also the reason the toggle sits in the top bar
        // rather than only in Settings: a control nobody can find is the same thing
        // as a feature nobody has.
        (SettingKey::UiShowProjectColumn, Value::from(false)),
        (SettingKey::FocusModeEnabled, Value::from(false)),
        // All three suppression rows default on: the point of turning focus mode
        // on at all is "stop interrupting me", so a master switch whose sub-rows
        // default off would need a second decision before it did anything. Each
        // is independent so a user who only wants the OS banner gone, say, can
        // turn the other two back on without losing that.
        (SettingKey::FocusModeSuppressBell, Value::from(true)),
        (SettingKey::FocusModeSuppressToasts, Value::from(true)),
        (
            SettingKey::FocusModeSuppressOsNotifications,
            Value::from(true),
        ),
        // Both automatic halves default **on**, and they are two settings rather
        // than one because the answer genuinely differs by power source: on mains
        // nothing is being spent, while on battery the hold costs somebody's
        // charge. Splitting them is what lets the mains half be generous without
        // making the battery half reckless — a single switch would have to be one
        // or the other, and either choice is wrong half the time.
        (SettingKey::KeepAwakeSharingOnPower, Value::from(true)),
        (
            SettingKey::KeepAwakeSharingOnPowerMinutes,
            Value::from(DEFAULT_KEEP_AWAKE_SHARING_ON_POWER_MINUTES),
        ),
        (SettingKey::KeepAwakeSharingOnBattery, Value::from(true)),
        (
            SettingKey::KeepAwakeSharingOnBatteryMinutes,
            Value::from(DEFAULT_KEEP_AWAKE_SHARING_ON_BATTERY_MINUTES),
        ),
        // On, which is what the coffee menu already did before this setting
        // existed — so the default changes nothing and the row is purely an off
        // switch. Turning it off is a real guarantee and not a preference: veld
        // then never writes `pmset disablesleep` on this machine, on any path,
        // even the one where a human pressed the button and asked for it.
        (SettingKey::KeepAwakeManualOnBattery, Value::from(true)),
        // The share lifetimes that were two daemon-private constants until now.
        // Unchanged numbers, so nothing about a default share moves; what changes
        // is that they are answerable and settable. See
        // `DEFAULT_SHARING_PEER_TTL_MINUTES` for why they belong beside the caps
        // above rather than in the share API.
        (
            SettingKey::SharingPeerTtlMinutes,
            Value::from(DEFAULT_SHARING_PEER_TTL_MINUTES),
        ),
        (
            SettingKey::SharingWebTtlMinutes,
            Value::from(DEFAULT_SHARING_WEB_TTL_MINUTES),
        ),
        // **On**, and this is the one default in this file that is a safety
        // posture rather than a preference. Every other key here decides how veld
        // behaves; this one decides whether the user's whole install survives a
        // filesystem event. A backup that ships off is a backup nobody has on the
        // day they need it — and the cost of it being on is a few megabytes an
        // hour, because the artifact omits the tables that make the live file
        // large. The switch exists for someone who has their own snapshotting and
        // wants veld to stay out of it.
        (SettingKey::BackupEnabled, Value::from(true)),
        (
            SettingKey::BackupIntervalMinutes,
            Value::from(DEFAULT_BACKUP_INTERVAL_MINUTES),
        ),
        (SettingKey::BackupKeep, Value::from(DEFAULT_BACKUP_KEEP)),
        (
            SettingKey::BackupKeepDaily,
            Value::from(DEFAULT_BACKUP_KEEP_DAILY),
        ),
        // Empty means the derived default — see `backup::default_dir`. Stored
        // empty rather than resolved once, because the resolved path depends on
        // the machine and a value baked into one user's database would be wrong
        // on the next one they restore it onto.
        (SettingKey::BackupDir, Value::from("")),
        // Off — the default is today's behaviour: the feedback overlay is injected
        // into every routed site. The switch exists for someone who uses Veld
        // purely as an orchestrator and does not collect feedback; see the accessor
        // for what turning it on does.
        (SettingKey::FeedbackSuppressOverlay, Value::from(false)),
        // On — the menu-bar icon is how Veld Desktop says it is running, and it
        // is the only place a run's status is visible with no window open. Off is
        // for a crowded menu bar: the app is still reachable from its Dock icon,
        // which macOS keeps as long as the process lives, so turning it off costs
        // ambient status rather than access. Read by the Electron shell only —
        // a browser tab has no menu bar to put anything in.
        (SettingKey::DesktopMenuBarIcon, Value::from(true)),
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

    /// Delete stored rows, putting those settings back on their defaults.
    ///
    /// **Deleting the row is the only correct way to reset one**, and the reason
    /// is [`defaults`]: writing the default *value* back would store a row that
    /// happens to match today's default and would then silently stop tracking it
    /// the next time the default changes — the user asked for "whatever Veld
    /// thinks is right", not for the number that answered that question once.
    /// It is also what makes an unset distinguishable from a deliberate choice
    /// that agrees with the default, which is what `veld settings` reports in its
    /// `FROM` column.
    ///
    /// Returns how many rows actually existed, so a caller can tell "reset" from
    /// "was already on the default" without a prior read. Unknown keys are
    /// deleted too — an unrecognised key is a preference this build cannot
    /// interpret, not one it may not clear.
    pub fn unset_settings(&self, keys: &[String]) -> Result<usize, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut removed = 0;
        for key in keys {
            removed += tx.execute(
                "DELETE FROM settings WHERE scope = ?1 AND key = ?2",
                params![SCOPE_GLOBAL, key],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Which settings have a stored row — i.e. have been set rather than left on
    /// their default.
    ///
    /// The `FROM` column of `veld settings`, and the thing that makes an
    /// effective value legible: a document of forty-eight values says nothing
    /// about which of them anybody chose.
    pub fn settings_with_stored_value(&self) -> Result<Vec<String>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT key FROM settings WHERE scope = ?1 ORDER BY key")?;
        let rows = stmt
            .query_map(params![SCOPE_GLOBAL], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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

    /// The shell a terminal session should spawn — already resolved, so every
    /// caller gets the same answer and none of them has to know what `"auto"`
    /// means.
    ///
    /// Read by the daemon (it is the daemon that spawns the shell), so it takes
    /// the same "anything that is not a value we accept is the default" path the
    /// other daemon-read keys take rather than trusting the stored bytes: a
    /// preference written by a newer build, or one whose shell has since been
    /// uninstalled, falls back to [`crate::shell::auto_shell`] instead of failing
    /// the spawn.
    pub fn terminal_shell(&self) -> String {
        crate::shell::resolve(
            self.setting(&SettingKey::TerminalShell)
                .ok()
                .flatten()
                .as_ref()
                .and_then(|v| v.as_str()),
        )
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

    /// Whether a terminal session gets OSC 133 shell integration, so the window can
    /// tell that a command started, ended, and with what status.
    ///
    /// Read by the daemon, which builds the session's environment. Independent of
    /// [`SettingKey::TerminalInterceptSystemOpen`] even though both ride the same
    /// startup handoff (`ZDOTDIR` for zsh, posix-mode `$ENV` for bash): the handoff
    /// file is written once and each half of it is gated by its own variable, so one
    /// switch cannot turn the other off. That coupling existed in the first version
    /// and was wrong in a way neither setting's documentation admitted.
    pub fn terminal_shell_integration(&self) -> bool {
        self.setting(&SettingKey::TerminalShellIntegration)
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Whether a terminal session gets a coding-agent shim that injects lifecycle
    /// hooks, so an agent waiting on the user reaches the worktree's inbox.
    ///
    /// Read by the daemon, which builds the session's environment. Off means the
    /// `claude` shim on `PATH` is a bare `exec` passthrough — the file is still there
    /// (the shim directory is written once per daemon start, not per session), and it
    /// is the *absence of `VELD_AGENT_HOOKS`* in the environment that disables it. A
    /// gate that depended on the file being absent would have to rewrite the
    /// directory whenever the setting changed, and would still be wrong for every
    /// shell already open.
    pub fn terminal_agent_integration(&self) -> bool {
        self.setting(&SettingKey::TerminalAgentIntegration)
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Whether the daemon may evaluate a project's `ide.extensions` status badges
    /// automatically.
    ///
    /// The one machine-global off switch for the only thing veld runs from a repo's
    /// config with no user action. Off leaves the declarations visible and the
    /// buttons clickable — a click is the user asking — and stops only the
    /// unattended, repeated half. It exists because that half is the part a
    /// consent prompt would have gated, and a prompt was rejected: bound to the
    /// declared commands it must re-prompt on every `git pull` that touches
    /// `veld.json`, and unbound it is decorative. A switch costs the cautious one
    /// decision and everybody else nothing.
    pub fn extensions_auto_refresh(&self) -> bool {
        self.setting(&SettingKey::ExtensionsAutoRefresh)
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Whether the global feedback-overlay opt-out is on.
    ///
    /// When it is, the orchestrator forces `features.feedback_overlay` off on
    /// every routed site — even one whose `veld.json` asks for the overlay back
    /// on — so the overlay is never injected into a reverse-proxied page. The
    /// per-machine answer for a setup that uses Veld purely as an orchestrator
    /// and does not collect feedback; the client-log collector and the
    /// `/__veld__/*` routes are unaffected.
    ///
    /// Read by both the CLI and the daemon (each can run the orchestrator),
    /// which is why it lives here beside the catalog rather than in the bundle.
    pub fn feedback_overlay_suppressed(&self) -> bool {
        self.setting(&SettingKey::FeedbackSuppressOverlay)
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// The five keep-awake settings, read in one go.
    ///
    /// One call rather than five because the daemon's caffeinate module reads them
    /// as a set on every share event and every power change, and it must do the
    /// read *before* taking its session lock — a per-key round trip through a
    /// blocking rusqlite handle inside that critical section would stall every
    /// status poll behind it.
    pub fn keep_awake(&self) -> KeepAwakePrefs {
        let flag = |key: SettingKey, fallback: bool| {
            self.setting(&key)
                .ok()
                .flatten()
                .and_then(|v| v.as_bool())
                .unwrap_or(fallback)
        };
        let minutes = |key: SettingKey, fallback: i64| {
            self.setting(&key)
                .ok()
                .flatten()
                .and_then(|v| v.as_i64())
                .unwrap_or(fallback)
                .clamp(MIN_KEEP_AWAKE_MINUTES, MAX_KEEP_AWAKE_MINUTES)
        };
        KeepAwakePrefs {
            sharing_on_power: flag(SettingKey::KeepAwakeSharingOnPower, true),
            sharing_on_power_minutes: minutes(
                SettingKey::KeepAwakeSharingOnPowerMinutes,
                DEFAULT_KEEP_AWAKE_SHARING_ON_POWER_MINUTES,
            ),
            sharing_on_battery: flag(SettingKey::KeepAwakeSharingOnBattery, true),
            sharing_on_battery_minutes: minutes(
                SettingKey::KeepAwakeSharingOnBatteryMinutes,
                DEFAULT_KEEP_AWAKE_SHARING_ON_BATTERY_MINUTES,
            ),
            manual_on_battery: flag(SettingKey::KeepAwakeManualOnBattery, true),
        }
    }

    /// How long a share of this `mode` should live, in **seconds**, when neither
    /// `veld share --ttl` nor the project's `veld.json` said.
    ///
    /// Seconds because that is the unit every consumer of a TTL already speaks
    /// (the wire, the manifest's `expires_at`, `--ttl`); minutes are the unit a
    /// *person* sets one in, which is why the stored setting is minutes and the
    /// conversion lives here rather than at the call site.
    ///
    /// Clamped on read like [`Self::keep_awake`]'s minutes, and for the same
    /// reason: a value written by an older or hand-edited database must not widen
    /// the bound the validator enforces on the way in.
    pub fn share_ttl_secs(&self, mode: crate::config::ExposeMode) -> i64 {
        let (key, fallback) = match mode {
            crate::config::ExposeMode::Peer => (
                SettingKey::SharingPeerTtlMinutes,
                DEFAULT_SHARING_PEER_TTL_MINUTES,
            ),
            crate::config::ExposeMode::Web => (
                SettingKey::SharingWebTtlMinutes,
                DEFAULT_SHARING_WEB_TTL_MINUTES,
            ),
        };
        self.setting(&key)
            .ok()
            .flatten()
            .and_then(|v| v.as_i64())
            .unwrap_or(fallback)
            .clamp(MIN_SHARE_TTL_MINUTES, MAX_SHARE_TTL_MINUTES)
            * 60
    }

    /// Which checkout's `veld.json` `ide.extensions` are read from
    /// (`extensions.source`). See [`ConfigSource`].
    ///
    /// Read by the daemon's `worktree_target` (`veld-daemon/src/extensions.rs`),
    /// so — like [`Self::git_create_from`] — it goes through the "anything not a
    /// real value is the default" path rather than trusting the stored bytes. A
    /// value that is not `"worktree"` here is `"main"` (the default).
    pub fn extensions_source(&self) -> ConfigSource {
        self.setting(&SettingKey::ExtensionsSource)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().and_then(ConfigSource::parse))
            .unwrap_or(ConfigSource::Main)
    }

    /// Which checkout's `veld.json` `ide.news` is read from (`news.source`). See
    /// [`ConfigSource`].
    ///
    /// Read by the daemon's `repo_view` (`veld-daemon/src/desktop.rs`), with the
    /// same "anything not a real value is the default" fallback as
    /// [`Self::extensions_source`]. Defaults to `"main"`, unchanged from the
    /// behaviour before this setting existed.
    pub fn news_source(&self) -> ConfigSource {
        self.setting(&SettingKey::NewsSource)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().and_then(ConfigSource::parse))
            .unwrap_or(ConfigSource::Main)
    }

    /// Where a new worktree's branch is cut from (`git.createFrom`).
    ///
    /// Read by the daemon's `create_worktree`, so it goes through the same
    /// "anything not a real value is the default" path the other daemon-read
    /// keys take rather than trusting the stored bytes. The stored value is
    /// validated by [`SettingKey::GitCreateFrom`], so a value that is not
    /// `"local"` here is `"origin"` (the default).
    pub fn git_create_from(&self) -> GitCreateSource {
        self.setting(&SettingKey::GitCreateFrom)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().and_then(GitCreateSource::parse))
            .unwrap_or_default()
    }

    /// The configured base directory for a *new* worktree's checkout
    /// (`worktree.storageMode` / `worktree.storageDir`), or `None` for the
    /// sibling-of-repo `_worktrees` default.
    ///
    /// Read by the daemon's `create_worktree`, so — like [`Self::git_create_from`]
    /// — it goes through the same "anything not a real value is the default" path
    /// rather than trusting the stored bytes. Both conditions have to hold: the
    /// mode has to say `"custom"` **and** the stored directory has to be a
    /// non-empty absolute path. A user who switches to "Custom location" before
    /// picking a folder gets today's behaviour, not new worktrees silently
    /// landing at the repo root's parent.
    ///
    /// Existing checkouts are never moved — this only decides where the *next*
    /// one is created.
    pub fn worktree_storage_dir(&self) -> Option<std::path::PathBuf> {
        let mode = self
            .setting(&SettingKey::WorktreeStorageMode)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_owned));
        if mode.as_deref() != Some("custom") {
            return None;
        }
        let dir = self
            .setting(&SettingKey::WorktreeStorageDir)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let path = std::path::PathBuf::from(dir.trim());
        path.is_absolute().then_some(path)
    }

    /// The five `backup.*` settings as one value, with `backup.dir` already
    /// resolved to a real path.
    ///
    /// One struct rather than five reads for the same reason [`KeepAwakePrefs`] is
    /// one: the daemon's scheduler needs all of them on every tick, and reading
    /// them separately is five chances for one to be read from a different pass
    /// than the rest. Every field degrades to its default rather than failing —
    /// a broken preference must not stop a backup, which is the one subsystem
    /// whose whole job is to still be working when other things are not.
    ///
    /// `dir` is `None` only when the platform has no data directory at all, which
    /// is the same condition [`DbError::NoDataDir`] covers for the database
    /// itself: on such a machine there is nothing to back up either.
    pub fn backup_prefs(&self) -> BackupPrefs {
        let bool_of = |key: SettingKey, fallback: bool| {
            self.setting(&key)
                .ok()
                .flatten()
                .and_then(|v| v.as_bool())
                .unwrap_or(fallback)
        };
        let int_of = |key: SettingKey, fallback: i64| {
            self.setting(&key)
                .ok()
                .flatten()
                .and_then(|v| v.as_i64())
                .unwrap_or(fallback)
        };
        let configured = self
            .setting(&SettingKey::BackupDir)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let configured = std::path::PathBuf::from(configured.trim());
        BackupPrefs {
            enabled: bool_of(SettingKey::BackupEnabled, true),
            interval_minutes: int_of(
                SettingKey::BackupIntervalMinutes,
                DEFAULT_BACKUP_INTERVAL_MINUTES,
            )
            .clamp(MIN_BACKUP_INTERVAL_MINUTES, MAX_BACKUP_INTERVAL_MINUTES),
            keep: int_of(SettingKey::BackupKeep, DEFAULT_BACKUP_KEEP)
                .clamp(MIN_BACKUP_KEEP, MAX_BACKUP_KEEP),
            keep_daily: int_of(SettingKey::BackupKeepDaily, DEFAULT_BACKUP_KEEP_DAILY)
                .clamp(MIN_BACKUP_KEEP_DAILY, MAX_BACKUP_KEEP_DAILY),
            dir: if configured.is_absolute() {
                Some(configured)
            } else {
                super::backup::default_dir()
            },
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

    /// Which local files a browser pane may be pointed at.
    ///
    /// One accessor rather than six reads because this is a *policy*, and every
    /// consumer needs all of it: the `open` shim's interception, the
    /// recently-edited list, and the `open-file` route each answer the same
    /// question. The daemon is the only reader — see `veld_core::files` for why
    /// the pure predicate does not read a database itself.
    ///
    /// Each key falls back to its shipped default rather than to `false`, so a
    /// row a newer build wrote as something unparseable degrades to "behaves as
    /// documented" instead of silently switching the feature off.
    pub fn view_policy(&self) -> crate::files::ViewPolicy {
        let flag = |key: SettingKey, default: bool| {
            self.setting(&key)
                .ok()
                .flatten()
                .and_then(|v| v.as_bool())
                .unwrap_or(default)
        };
        crate::files::ViewPolicy {
            web_pages: flag(SettingKey::FilesViewWebPages, true),
            images: flag(SettingKey::FilesViewImages, false),
            pdfs: flag(SettingKey::FilesViewPdfs, true),
            plain_text: flag(SettingKey::FilesViewPlainText, false),
            patterns: self
                .setting(&SettingKey::FilesViewPatterns)
                .ok()
                .flatten()
                .and_then(|v| match v {
                    Value::Array(items) => Some(items),
                    _ => None,
                })
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect(),
        }
    }

    /// Whether a pane opened on a file watches it for changes unless told not to.
    pub fn files_watch_by_default(&self) -> bool {
        self.setting(&SettingKey::FilesWatchByDefault)
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
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
    fn share_ttl_reads_as_seconds_per_mode() {
        // Minutes are what a person sets; seconds are what every consumer of a
        // TTL already speaks, so the conversion belongs in the reader.
        let (_dir, db) = test_db();
        assert_eq!(
            db.share_ttl_secs(crate::config::ExposeMode::Peer),
            DEFAULT_SHARING_PEER_TTL_MINUTES * 60
        );
        assert_eq!(
            db.share_ttl_secs(crate::config::ExposeMode::Web),
            DEFAULT_SHARING_WEB_TTL_MINUTES * 60
        );
        // The two modes are genuinely different values, which is the whole reason
        // there are two keys — a test that passed with one reader wired to both
        // would not notice.
        assert_ne!(
            db.share_ttl_secs(crate::config::ExposeMode::Peer),
            db.share_ttl_secs(crate::config::ExposeMode::Web)
        );
    }

    #[test]
    fn a_share_ttl_outside_the_bounds_is_clamped_on_the_way_in() {
        // The validator's half: `veld settings set` and the dialog both go through
        // it, so an out-of-range value is stored already narrowed rather than
        // refused (see `validate`).
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("sharing.peerTtlMinutes", Value::from(999_999))]))
            .unwrap();
        assert_eq!(
            db.setting(&SettingKey::SharingPeerTtlMinutes).unwrap(),
            Some(Value::from(MAX_SHARE_TTL_MINUTES))
        );
    }

    #[test]
    fn a_stored_share_ttl_outside_the_bounds_is_clamped_on_read_too() {
        // The *reader's* half, which the test above cannot reach: the validator
        // would have narrowed anything written through it, so the only way to
        // exercise this is the shape a newer build with a wider range — or a
        // hand-edited database — leaves behind. Same pattern as
        // `detach_grace_is_clamped_even_when_stored_out_of_range`.
        let (_dir, db) = test_db();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO settings (scope, key, value, updated_at)
                 VALUES ('global', 'sharing.peerTtlMinutes', '999999', '2026-01-01T00:00:00.000000Z')",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            db.share_ttl_secs(crate::config::ExposeMode::Peer),
            MAX_SHARE_TTL_MINUTES * 60
        );
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
    fn the_terminal_shell_stores_a_path_and_resolves_a_missing_one() {
        let (_dir, db) = test_db();
        // Nothing stored is `auto`, and `auto` resolves to the login shell — the
        // behaviour every existing user had before this key existed.
        assert_eq!(
            db.settings().unwrap()["terminal.shell"],
            Value::from(crate::shell::AUTO)
        );
        assert_eq!(db.terminal_shell(), crate::shell::auto_shell());

        // A path is stored verbatim, because it is what gets spawned.
        db.patch_settings(&patch(&[(
            "terminal.shell",
            Value::from("/nonexistent/sh"),
        )]))
        .unwrap();
        assert_eq!(
            db.settings().unwrap()["terminal.shell"],
            Value::from("/nonexistent/sh")
        );
        // …but a shell that is not there resolves to the login shell rather than
        // being spawned. The preference is kept: the machine may get it back, and
        // deleting a user's choice because a binary is momentarily absent is not
        // ours to do.
        assert_eq!(db.terminal_shell(), crate::shell::auto_shell());

        // A bare name is refused — it would resolve against the daemon's bare
        // service PATH, which is a different binary from the one that name finds
        // in the user's terminal. So is anything that is not a string.
        for bad in [Value::from("bash"), Value::from(""), Value::from(7)] {
            let e = db
                .patch_settings(&patch(&[("terminal.shell", bad.clone())]))
                .expect_err(&format!("{bad} must be refused"));
            assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
        }
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

    /// The two terminal-integration switches default on, take a bool, and are
    /// **independent of each other and of `terminal.interceptSystemOpen`**.
    ///
    /// The independence is the assertion worth having. All three ride the same
    /// startup handoff, and the first version of shell integration was gated on
    /// `interceptSystemOpen` — so turning off "catch `open`/`xdg-open`" silently took
    /// the unread badge with it. Nothing in the type system stops that coming back:
    /// the coupling would live in `session_env`, and from here the only observable is
    /// that each accessor answers for its own key.
    #[test]
    fn the_terminal_integration_switches_are_independent_bools_defaulting_on() {
        let (_dir, db) = test_db();
        assert!(db.terminal_shell_integration());
        assert!(db.terminal_agent_integration());

        // One off leaves the other two alone, in both directions.
        db.patch_settings(&patch(&[("terminal.shellIntegration", Value::from(false))]))
            .unwrap();
        assert!(!db.terminal_shell_integration());
        assert!(db.terminal_agent_integration());
        assert!(db.terminal_intercept_system_open());

        db.patch_settings(&patch(&[
            ("terminal.shellIntegration", Value::from(true)),
            ("terminal.agentIntegration", Value::from(false)),
        ]))
        .unwrap();
        assert!(db.terminal_shell_integration());
        assert!(!db.terminal_agent_integration());
        assert!(db.terminal_intercept_system_open());

        // And `interceptSystemOpen` off does not take either of them down with it —
        // the exact coupling that shipped once.
        db.patch_settings(&patch(&[(
            "terminal.interceptSystemOpen",
            Value::from(false),
        )]))
        .unwrap();
        assert!(db.terminal_shell_integration());
        assert!(!db.terminal_intercept_system_open());

        for key in ["terminal.shellIntegration", "terminal.agentIntegration"] {
            let err = db
                .patch_settings(&patch(&[(key, Value::from("on"))]))
                .unwrap_err();
            assert!(
                matches!(err, DbError::InvalidSetting { .. }),
                "{key} accepted a string"
            );
        }
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

    /// The view-pattern list is normalised the way its cited precedent is.
    ///
    /// Mirrors `browser.externalOrigins`' own test, which this validator names as the
    /// shape it follows — the difference being that a blank line here is *dropped*
    /// rather than refused, because a text-list control produces one for every stray
    /// newline and refusing the whole patch over that is a field that fights its author.
    #[test]
    fn view_patterns_are_normalised_and_bounded() {
        let (_dir, db) = test_db();
        assert_eq!(
            db.settings().unwrap()["files.viewPatterns"],
            Value::Array(Vec::new()),
            "veld ships no guess about somebody's file layout"
        );

        // Trimmed, de-duplicated, and blank entries dropped rather than refused.
        db.patch_settings(&patch(&[(
            "files.viewPatterns",
            Value::Array(vec![
                Value::from("  reports/*.xml  "),
                Value::from(""),
                Value::from("   "),
                Value::from("reports/*.xml"),
                Value::from("*.log"),
            ]),
        )]))
        .unwrap();
        assert_eq!(
            db.settings().unwrap()["files.viewPatterns"],
            Value::Array(vec![Value::from("reports/*.xml"), Value::from("*.log")])
        );

        // Refused: a pattern that is not a string, one too long, one carrying a control
        // character, and one with a `..` segment. `..` cannot escape anything (a grant
        // confines every read), so this is the same defence-in-depth `worktree.storageDir`
        // applies — refuse the shape where somebody chose it.
        for bad in [
            Value::Array(vec![Value::from(7)]),
            Value::Array(vec![Value::from("x".repeat(300))]),
            Value::Array(vec![Value::from("a\nb.html")]),
            Value::Array(vec![Value::from("../outside/*.html")]),
            Value::Array(vec![Value::from("a/../b/*.html")]),
            Value::from("reports/*.xml"),
        ] {
            assert!(
                db.patch_settings(&patch(&[("files.viewPatterns", bad.clone())]))
                    .is_err(),
                "{bad:?} must be refused"
            );
        }
        // …and a refusal leaves the stored value alone.
        assert_eq!(
            db.settings().unwrap()["files.viewPatterns"],
            Value::Array(vec![Value::from("reports/*.xml"), Value::from("*.log")])
        );

        // More patterns than the cap.
        let many: Vec<Value> = (0..MAX_VIEW_PATTERNS + 1)
            .map(|i| Value::from(format!("*.x{i}")))
            .collect();
        assert!(
            db.patch_settings(&patch(&[("files.viewPatterns", Value::Array(many))]))
                .is_err()
        );
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
        for (rel, needle) in [
            ("README.md", &max),
            ("README.md", &default),
            ("website/llms-full.txt", &max),
            ("website/llms-full.txt", &default),
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

        // The dialog's half of this gate has **inverted**, and that is the point of
        // the catalog. It used to assert `SettingsDialog.tsx` *contained*
        // `max={7}` — a literal in another language that silently disagreed once
        // this constant moved. The dialog now reads the bound from
        // `GET /api/settings/catalog`, whose `runs.historyDays` range cites
        // `MAX_RUN_HISTORY_DAYS` directly, so the two cannot disagree and there is
        // nothing left to pin. What is worth pinning is that nobody puts it back:
        // a hardcoded bound beside a catalog-driven control is a control that
        // stops matching the daemon the next time the constant moves, and it would
        // look like the old, tested arrangement while being the untested one.
        let dialog = root.join("crates/veld-daemon/ui/src/components/SettingsDialog.tsx");
        let text = std::fs::read_to_string(&dialog)
            .unwrap_or_else(|e| panic!("read {}: {e}", dialog.display()));
        let hardcoded = format!("max={{{MAX_RUN_HISTORY_DAYS}}}");
        assert!(
            !text.contains(&hardcoded),
            "SettingsDialog.tsx hardcodes {hardcoded:?}. Bounds come from the \
             settings catalog now — render `choices.max` rather than restating \
             MAX_RUN_HISTORY_DAYS in TypeScript."
        );
    }

    #[test]
    fn worktree_storage_dir_is_none_until_custom_mode_has_an_absolute_folder() {
        let (_dir, db) = test_db();
        // Sibling mode (the default): no configured directory at all.
        assert_eq!(db.worktree_storage_dir(), None);

        // Custom mode chosen, but no folder picked yet — still None, not the
        // empty string turned into a path. This is the case that must not
        // silently redirect new worktrees to nowhere useful.
        db.patch_settings(&patch(&[("worktree.storageMode", Value::from("custom"))]))
            .unwrap();
        assert_eq!(db.worktree_storage_dir(), None);

        // A relative path is rejected by the validator before it ever reaches
        // storage, so the effective document keeps the previous (empty) value.
        let e = db
            .patch_settings(&patch(&[(
                "worktree.storageDir",
                Value::from("relative/dir"),
            )]))
            .unwrap_err();
        assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
        assert_eq!(db.worktree_storage_dir(), None);

        // Both conditions hold: mode is custom and the folder is a real absolute path.
        db.patch_settings(&patch(&[(
            "worktree.storageDir",
            Value::from("/tmp/veld-worktrees"),
        )]))
        .unwrap();
        assert_eq!(
            db.worktree_storage_dir(),
            Some(std::path::PathBuf::from("/tmp/veld-worktrees"))
        );

        // Switching back to sibling mode stops using the folder, even though it
        // is still stored — flipping the mode is how a user "clears" this
        // without losing the folder if they flip back.
        db.patch_settings(&patch(&[("worktree.storageMode", Value::from("sibling"))]))
            .unwrap();
        assert_eq!(db.worktree_storage_dir(), None);
    }

    #[test]
    fn worktree_storage_dir_rejects_too_long_and_control_char_values() {
        // The two bounds this key's validator carries and neither had a test —
        // the sibling free-text key (`BrowserSearchUrl`) pins both of these at
        // its own length constant, above.
        let (_dir, db) = test_db();
        for bad in [
            Value::from(format!("/{}", "a".repeat(MAX_WORKTREE_STORAGE_DIR_LEN))),
            Value::from("/tmp/has\ta-tab"),
            Value::from("/tmp/has\na-newline"),
        ] {
            let e = db
                .patch_settings(&patch(&[("worktree.storageDir", bad.clone())]))
                .expect_err(&format!("{bad} must be refused"));
            assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
        }
        // Empty and a real absolute path both still work — the bound is on
        // length and control characters, not on content.
        db.patch_settings(&patch(&[(
            "worktree.storageDir",
            Value::from("/tmp/veld-worktrees"),
        )]))
        .unwrap();
        assert_eq!(
            db.settings().unwrap()["worktree.storageDir"],
            Value::from("/tmp/veld-worktrees")
        );
    }

    #[test]
    fn worktree_storage_dir_rejects_a_parent_dir_component() {
        // Defence in depth: the daemon's own `canonicalize_prefix`
        // (`veld-daemon/src/desktop.rs`) already resolves a `..` lexically
        // before comparing a checkout path against every repo root, so this
        // is not load-bearing for that check — but a *stored* directory has
        // no legitimate reason to carry one, and refusing it here catches
        // the shape at the point someone chose it.
        let (_dir, db) = test_db();
        let e = db
            .patch_settings(&patch(&[(
                "worktree.storageDir",
                Value::from("/base/ghost/../Proj"),
            )]))
            .unwrap_err();
        assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
    }

    #[test]
    fn extensions_source_defaults_to_main_and_reads_the_stored_value() {
        // The gap review found: `extensions_source`/`news_source` hand-compare a
        // string independently of `ConfigSource::parse`, so a typo'd or inverted
        // reader would pin every install to `Main` forever with every other test
        // in the suite still green. This pins the accessor itself, not just the
        // validator or `select_news`.
        let (_dir, db) = test_db();
        assert_eq!(db.extensions_source(), ConfigSource::Main);

        db.patch_settings(&patch(&[("extensions.source", Value::from("worktree"))]))
            .unwrap();
        assert_eq!(db.extensions_source(), ConfigSource::Worktree);

        db.patch_settings(&patch(&[("extensions.source", Value::from("main"))]))
            .unwrap();
        assert_eq!(db.extensions_source(), ConfigSource::Main);
    }

    #[test]
    fn extensions_source_rejects_garbage_rather_than_storing_it() {
        let (_dir, db) = test_db();
        let e = db
            .patch_settings(&patch(&[("extensions.source", Value::from("origin"))]))
            .unwrap_err();
        assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
        // The rejected write must not have clobbered the default.
        assert_eq!(db.extensions_source(), ConfigSource::Main);
    }

    #[test]
    fn news_source_defaults_to_main_and_reads_the_stored_value() {
        let (_dir, db) = test_db();
        assert_eq!(db.news_source(), ConfigSource::Main);

        db.patch_settings(&patch(&[("news.source", Value::from("worktree"))]))
            .unwrap();
        assert_eq!(db.news_source(), ConfigSource::Worktree);

        db.patch_settings(&patch(&[("news.source", Value::from("main"))]))
            .unwrap();
        assert_eq!(db.news_source(), ConfigSource::Main);
    }

    #[test]
    fn news_source_rejects_garbage_rather_than_storing_it() {
        let (_dir, db) = test_db();
        let e = db
            .patch_settings(&patch(&[("news.source", Value::from("branch"))]))
            .unwrap_err();
        assert!(matches!(e, DbError::InvalidSetting { .. }), "{e}");
        assert_eq!(db.news_source(), ConfigSource::Main);
    }

    #[test]
    fn config_source_parse_is_exhaustive_over_all() {
        for source in ConfigSource::ALL {
            assert_eq!(ConfigSource::parse(source.as_str()), Some(*source));
        }
        assert_eq!(ConfigSource::parse("bogus"), None);
    }
}
