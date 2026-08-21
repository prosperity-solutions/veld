//! What a setting *is*, for every surface that has to describe one to a human or
//! an agent: the `/ide` settings dialog, `veld settings`, and anything after them.
//!
//! # Adding a setting
//!
//! **One language, Rust. No TypeScript.** That is the property this module
//! exists to hold. The recipe is three steps:
//!
//! 1. a [`SettingKey`] variant, its entry in [`SettingKey::ALL`] (**in the position
//!    it should appear on screen** — see below), its `as_str`/`parse` arms, and its
//!    [`defaults`](super::settings::defaults) entry;
//! 2. its `validate` arm, which stays a hand-written match arm (see *Why this is a
//!    projection* below);
//! 3. a [`SettingKey::spec`] arm here, naming its title, help, group, section,
//!    shape and [`Choices`].
//!
//! That is six small edits in `settings.rs` plus one here — the variant, its
//! `ALL` entry, `as_str`, `parse`, `defaults`, and `validate`. **Only two of them
//! are compiler-enforced:** `as_str` and `validate` are exhaustive matches with no
//! wildcard arm. The variant itself needs no guard, since omitting it means there
//! is nothing to add in the first place. That leaves **three that compile clean
//! and behave wrong**, which is why each has a test of its own:
//!
//! - a missing [`SettingKey::ALL`] entry leaves the key working but **invisible**
//!   to the catalog, the dialog and the CLI (`every_setting_variant_is_listed`);
//! - a missing [`defaults`](super::settings::defaults) entry leaves the effective
//!   document incomplete (`every_known_key_round_trips_and_has_a_default`);
//! - a missing `parse` arm is the nastiest: that function ends in an
//!   `other => Unknown` catch-all by design, so the key still stores and reads —
//!   **unvalidated**, since `Unknown` accepts anything within its size bounds.
//!
//! The dialog then renders it and `veld settings` lists, gets, sets and describes
//! it, with no further change anywhere. A shape the bundle has never heard of
//! renders as a visible "unsupported control" row rather than vanishing — the one
//! failure mode worth engineering against here is a setting that exists in Rust
//! and is *invisible* in the UI.
//!
//! The only thing that still needs TypeScript is a genuinely **new kind of
//! control** — a colour picker, a key-capture field. That is a new [`Choices`] or
//! [`ValueShape`] variant plus its renderer.
//!
//! **Be precise about what catches you there, because it is easy to overstate.**
//! The bundle's `Choices` union is hand-written, so adding a variant *here* does
//! not stop TypeScript compiling — nothing ties the two declarations together.
//! What the bundle's `never` assertion catches is the second half: once somebody
//! adds the variant to the TS union, the compiler refuses until a renderer
//! exists. Between those two edits the new variant is simply unknown to the
//! client, and it renders as a visible "this version cannot show this setting"
//! row naming the key — degraded, never invisible, which is the property worth
//! having. Do not read the `never` as a Rust→TypeScript gate; it is a
//! TypeScript-internal one.
//!
//! # Why this is a projection, not a table
//!
//! [`SettingKey::spec`] is an **exhaustive `match self`**, not a `&'static [Spec]`
//! to look a key up in. That is deliberate and it is the whole design:
//! `SettingKey::validate` is likewise an exhaustive match with no wildcard arm, so
//! a new variant is a *compile error* until it has been given a validator. A
//! lookup table is a partial function checked by nothing — it would turn the one
//! machine-checked thing in this subsystem into the one thing that needs a test,
//! which is the opposite of the trade this change was made for.
//!
//! For the same reason a spec **never restates a constraint**. Where the validator
//! clamps, the spec cites the same `MIN_`/`MAX_` constant; where it accepts an
//! enum, both cite the same `&'static [Choice]` slice below. One literal, two
//! readers, nothing to assert.
//!
//! Two things a spec deliberately does *not* carry:
//!
//! - **The default.** It has exactly one home — `settings::defaults()` — and
//!   [`catalog`] reads it from there, so the catalog cannot disagree with the
//!   document a client actually receives.
//! - **Anything about how a group is painted.** Tab icons, panel padding and the
//!   modal's shape stay in the bundle. A group is *what a setting is about*, which
//!   is why it belongs beside the title and the help text; a tab is how that gets
//!   drawn, which is not.
//!
//! # Offered is not accepted
//!
//! [`Choices`] describes what a surface should **offer**. For six keys — the two
//! [`Choices::Presets`] and the four [`Choices::Runtime`] — that is deliberately
//! narrower than what `validate` **accepts**, and collapsing the two would make
//! this catalog lie:
//!
//! - `keepAwake.*Minutes` offer six presets and accept anything in
//!   `[MIN_KEEP_AWAKE_MINUTES, MAX_KEEP_AWAKE_MINUTES]` — a client that sends 45
//!   gets 45 (`settings.rs`, `KeepAwakeSharingOnPowerMinutes`);
//! - `terminal.shell` offers what `GET /api/shells` found and accepts `"auto"` or
//!   any absolute path, by shape, never by existence;
//! - `terminal.fontFamily` offers what the browser reports it can render and
//!   accepts any CSS font list that cannot escape a stylesheet rule;
//! - `worktree.storageDir` and `backup.dir` offer a folder picker and accept any
//!   absolute path — or empty, which is each one's "no folder chosen" value.
//!
//! [`Choices::Presets`] and [`Choices::Runtime`] are how that asymmetry is stated
//! rather than hidden, and they are also exactly the keys whose control the bundle
//! supplies itself.

use serde::Serialize;
use serde_json::Value;

use super::settings::{
    ConfigSource, MAX_BACKUP_INTERVAL_MINUTES, MAX_BACKUP_KEEP, MAX_BACKUP_KEEP_DAILY,
    MAX_BELL_VOLUME, MAX_DETACH_GRACE_MINUTES, MAX_FONT_SIZE, MAX_KEEP_AWAKE_MINUTES,
    MAX_RECONNECT_BACKOFF_SECONDS, MAX_RECONNECT_FIRST_DELAY_SECONDS, MAX_RECONNECT_TRIES,
    MAX_RUN_HISTORY_DAYS, MAX_SCROLLBACK, MAX_SHARE_TTL_MINUTES, MAX_TRASH_RETENTION_DAYS,
    MIN_BACKUP_INTERVAL_MINUTES, MIN_BACKUP_KEEP, MIN_BACKUP_KEEP_DAILY, MIN_BELL_VOLUME,
    MIN_DETACH_GRACE_MINUTES, MIN_FONT_SIZE, MIN_KEEP_AWAKE_MINUTES, MIN_RECONNECT_BACKOFF_SECONDS,
    MIN_RECONNECT_FIRST_DELAY_SECONDS, MIN_RECONNECT_TRIES, MIN_RUN_HISTORY_DAYS, MIN_SCROLLBACK,
    MIN_SHARE_TTL_MINUTES, SettingKey, defaults,
};

/// Which part of the product a setting is about.
///
/// **A group is not a key prefix**, and that is load-bearing rather than
/// incidental: `browser.externalOrigins` is about where a link opens, so it is
/// [`Links`](SettingGroup::Links); `worktree.*`, `runs.*` and `logs.*` answer to no
/// larger surface, so they are [`General`](SettingGroup::General). The two
/// shell-integration switches keep their `terminal.*` names because they configure
/// what veld puts in your **shell**, while what they produce — noticing, and
/// deciding whether to interrupt you — is [`Activity`](SettingGroup::Activity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingGroup {
    General,
    Git,
    Terminal,
    Activity,
    KeepAwake,
    Sharing,
    Links,
    Browser,
}

impl SettingGroup {
    /// Every group, in the order a surface should present them.
    pub const ALL: &'static [SettingGroup] = &[
        Self::General,
        Self::Git,
        Self::Terminal,
        Self::Activity,
        Self::KeepAwake,
        // Beside keep-awake, because the question a reader arrives with — "why
        // did my machine stay awake for two hours when I said four" — is answered
        // by one setting from each group.
        Self::Sharing,
        Self::Links,
        Self::Browser,
    ];

    /// The wire id. Stable — the bundle keys its tab icons off these.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Git => "git",
            Self::Terminal => "terminal",
            Self::Activity => "activity",
            Self::KeepAwake => "keepAwake",
            Self::Sharing => "sharing",
            Self::Links => "links",
            Self::Browser => "browser",
        }
    }

    /// What a human is shown.
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Git => "Git",
            Self::Terminal => "Terminal",
            Self::Activity => "Activity",
            Self::KeepAwake => "Keep awake",
            Self::Sharing => "Sharing",
            Self::Links => "Links",
            Self::Browser => "Browser panes",
        }
    }

    /// The inverse of [`Self::as_str`], exhaustive by construction over
    /// [`Self::ALL`].
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|g| g.as_str() == s)
    }
}

/// One option a surface may offer, with the words to offer it in.
///
/// `value` is what is stored and what `veld settings set` takes; `label` is what a
/// dialog puts in front of a person. They are not interchangeable — `news.source`
/// and `extensions.source` share a value vocabulary (`main`/`worktree`) and
/// deliberately do *not* share labels, because "every worktree" and "this
/// worktree" describe different consequences of the same stored string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

const fn choice(value: &'static str, label: &'static str) -> Choice {
    Choice { value, label }
}

/// The JSON type of a setting's value.
///
/// What a CLI needs in order to send `true` rather than `"true"` — the difference
/// between a setting that takes and one that is rejected, and not something an
/// agent should have to discover by trying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueShape {
    Bool,
    Int,
    Text,
    TextList,
}

impl ValueShape {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Text => "text",
            Self::TextList => "textList",
        }
    }
}

/// Where a runtime-offered list of choices comes from.
///
/// An enum rather than a free-form endpoint string so that both ends are
/// exhaustive: a new source is a compile error in Rust and a type error in the
/// bundle's `never` check, instead of a name that silently matches nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeSource {
    /// `GET /api/shells` — the shells this machine actually has.
    Shells,
    /// The fonts the *browser* reports it can render, which no daemon can know.
    Fonts,
    /// A native directory picker.
    Directory,
}

/// What a surface should offer for a setting — never what the validator accepts.
/// See *Offered is not accepted* in the module docs.
/// `rename_all` on an enum renames its **variants**; the fields inside a struct
/// variant need `rename_all_fields`. Both are here on purpose: without the
/// second one this type serialises `empty_means` beside `CatalogEntry`'s
/// `groupLabel`, and a client reading a uniformly-camelCase document gets
/// `undefined` for the one field that is not — silently, since it is `Option`al
/// and a missing placeholder just renders as no placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Choices {
    /// Free text within whatever the validator allows. A plain input.
    Free,
    /// A closed set. The same slice the validator's allow-list is built from.
    Static { options: &'static [Choice] },
    /// A number, bounded by the same constants the validator clamps to.
    Range {
        min: i64,
        max: i64,
        /// The stepper's increment, where a sensible one exists (weeks for a
        /// retention in days). `None` means one.
        step: Option<i64>,
        /// What the number counts, for a suffix and for `veld settings describe`.
        unit: Option<&'static str>,
        /// What an empty box means, when the low end of the range is an off
        /// switch rather than a small value (`0` = "keep until emptied").
        empty_means: Option<&'static str>,
    },
    /// A number offered as a short menu but accepted anywhere in range.
    Presets {
        offered: &'static [Choice],
        min: i64,
        max: i64,
        unit: Option<&'static str>,
    },
    /// A list only the client can produce. The bundle owns this control.
    Runtime { source: RuntimeSource },
}

/// Another setting whose value decides whether this one applies.
///
/// `equals: None` means "that key must be boolean `true`" — the shape of every
/// master switch here. A `Some` compares against a stored string, which is what
/// `worktree.storageDir` needs from `worktree.storageMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Requires {
    pub key: &'static str,
    pub equals: Option<&'static str>,
}

const fn requires_true(key: &'static str) -> Option<Requires> {
    Some(Requires { key, equals: None })
}

const fn requires_eq(key: &'static str, equals: &'static str) -> Option<Requires> {
    Some(Requires {
        key,
        equals: Some(equals),
    })
}

/// Everything a surface needs in order to present one setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// The label. A noun phrase or an imperative that completes on its own —
    /// General is a flat list with no heading above a row to finish its sentence.
    pub title: &'static str,
    /// What it does and what it costs, in prose. Not one line: several of these
    /// are the only place a behaviour is explained to anybody, and shortening
    /// them to fit a table would lose the sentence people came for. A CLI wraps
    /// it; a dialog puts it under the label.
    pub help: &'static str,
    pub group: SettingGroup,
    /// The heading this sits under inside its group, or `None` for a group that
    /// is a flat list.
    pub section: Option<&'static str>,
    pub shape: ValueShape,
    pub choices: Choices,
    /// The setting that gates this one, if any.
    pub requires: Option<Requires>,
}

/// One catalog entry as it goes over the wire and out of `veld settings --json`.
///
/// The default is read from [`defaults`] rather than stored on the [`Spec`], so
/// the catalog can never disagree with the document `GET /api/settings` returns.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub key: String,
    pub title: &'static str,
    pub help: &'static str,
    pub group: &'static str,
    pub group_label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<&'static str>,
    #[serde(rename = "type")]
    pub value_type: &'static str,
    pub default: Value,
    pub choices: Choices,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<Requires>,
}

/// One group as it goes over the wire, in presentation order.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CatalogGroup {
    pub id: &'static str,
    pub label: &'static str,
}

/// The whole catalog: every known setting, in the order a surface presents them.
///
/// Order is [`SettingKey::ALL`]'s, which is maintained as *display* order for
/// exactly this reason — one list rather than a second one to keep in step.
pub fn catalog() -> Vec<CatalogEntry> {
    let defaults = defaults();
    SettingKey::ALL
        .iter()
        .filter_map(|key| {
            let spec = key.spec()?;
            let name = key.as_str().to_string();
            Some(CatalogEntry {
                default: defaults.get(&name).cloned().unwrap_or(Value::Null),
                key: name,
                title: spec.title,
                help: spec.help,
                group: spec.group.as_str(),
                group_label: spec.group.label(),
                section: spec.section,
                value_type: spec.shape.as_str(),
                choices: spec.choices,
                requires: spec.requires,
            })
        })
        .collect()
}

/// Every group, in presentation order.
pub fn catalog_groups() -> Vec<CatalogGroup> {
    SettingGroup::ALL
        .iter()
        .map(|g| CatalogGroup {
            id: g.as_str(),
            label: g.label(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Shared choice lists
//
// Each of these is cited by *both* the spec below and the validator in
// `settings.rs`, so the offered set and the accepted set are one literal rather
// than two that agree today.
// ---------------------------------------------------------------------------

pub(super) const CURSOR_STYLES: &[Choice] = &[
    choice("block", "Block"),
    choice("underline", "Underline"),
    choice("bar", "Bar"),
];

pub(super) const MARKER_STYLES: &[Choice] = &[choice("color", "Colour"), choice("emoji", "Emoji")];

pub(super) const GIT_CREATE_SOURCES: &[Choice] = &[
    choice("origin", "Latest origin"),
    choice("local", "Local main"),
];

pub(super) const WORKTREE_STORAGE_MODES: &[Choice] = &[
    choice("sibling", "Next to repository (default)"),
    choice("custom", "Custom location"),
];

pub(super) const LOG_TIME_ZONES: &[Choice] = &[choice("local", "Local time"), choice("utc", "UTC")];

/// `extensions.source` and `news.source` share a value vocabulary and not their
/// labels — see [`Choice`].
const EXTENSIONS_SOURCES: &[Choice] = &[
    choice(ConfigSource::MAIN, "Main checkout"),
    choice(ConfigSource::WORKTREE, "This worktree"),
];

const NEWS_SOURCES: &[Choice] = &[
    choice(ConfigSource::MAIN, "Main checkout"),
    choice(ConfigSource::WORKTREE, "Every worktree"),
];

/// The keep-awake durations the coffee menu offers. Accepted values are the whole
/// `[MIN_KEEP_AWAKE_MINUTES, MAX_KEEP_AWAKE_MINUTES]` range — see *Offered is not
/// accepted*.
const KEEP_AWAKE_PRESETS: &[Choice] = &[
    choice("15", "15 minutes"),
    choice("30", "30 minutes"),
    choice("60", "1 hour"),
    choice("120", "2 hours"),
    choice("240", "4 hours"),
    choice("480", "8 hours"),
];

/// The share lifetimes the settings dialog offers. Accepted values are the whole
/// `[MIN_SHARE_TTL_MINUTES, MAX_SHARE_TTL_MINUTES]` range — see *Offered is not
/// accepted*.
///
/// Deliberately **not** [`KEEP_AWAKE_PRESETS`], despite today's two lists being
/// identical: one offers how long a machine stays awake, the other how long a
/// link works, and a preset added to one is not automatically right for the
/// other.
const SHARE_TTL_PRESETS: &[Choice] = &[
    choice("15", "15 minutes"),
    choice("30", "30 minutes"),
    choice("60", "1 hour"),
    choice("120", "2 hours"),
    choice("240", "4 hours"),
    choice("480", "8 hours"),
];

// ---------------------------------------------------------------------------
// Section headings
//
// Named rather than written inline so the same heading is one string across the
// settings that share it — a heading is the sentence half of several labels
// ("First attempt after" completes only under *Auto-reconnect*), so a typo in
// one arm would silently split a section in two.
// ---------------------------------------------------------------------------

const DATABASE_BACKUPS: Option<&str> = Some("Database backups");
const APPEARANCE: Option<&str> = Some("Appearance");
const BEHAVIOUR: Option<&str> = Some("Behaviour");
const AUTO_RECONNECT: Option<&str> = Some("Auto-reconnect");
const NOTICING: Option<&str> = Some("Noticing");
const NOTIFYING: Option<&str> = Some("Notifying");
const FOCUS_MODE: Option<&str> = Some("Focus mode");
const WHILE_SHARING: Option<&str> = Some("While you're sharing");
const WHEN_YOU_ASK: Option<&str> = Some("When you ask");
const SHARE_LINKS_EXPIRE: Option<&str> = Some("Share links expire after");
const LOCAL_FILES: Option<&str> = Some("Local files");

/// A plain on/off setting in a group with no headings — the commonest shape, and
/// the one worth not repeating forty times.
const fn toggle(title: &'static str, help: &'static str, group: SettingGroup) -> Spec {
    Spec {
        title,
        help,
        group,
        section: None,
        shape: ValueShape::Bool,
        choices: Choices::Free,
        requires: None,
    }
}

const fn toggle_in(
    title: &'static str,
    help: &'static str,
    group: SettingGroup,
    section: Option<&'static str>,
) -> Spec {
    Spec {
        section,
        ..toggle(title, help, group)
    }
}

const fn range(min: i64, max: i64) -> Choices {
    Choices::Range {
        min,
        max,
        step: None,
        unit: None,
        empty_means: None,
    }
}

impl SettingKey {
    /// How to present this setting to a human or an agent, or `None` for a key
    /// this build does not recognise.
    ///
    /// An **exhaustive match**, deliberately — see this module's *Why this is a
    /// projection, not a table*. A new [`SettingKey`] variant does not compile
    /// until it has been described here, which is the property a lookup table
    /// would have cost.
    ///
    /// [`Unknown`](Self::Unknown) has no spec and never will: it is a preference
    /// this binary is *preserving*, not one it can describe. Every consumer has
    /// to handle a stored value with no catalog entry anyway, because
    /// [`Db::settings`](super::Db::settings) returns unknown keys in the
    /// effective document.
    pub fn spec(&self) -> Option<Spec> {
        use SettingGroup::*;
        Some(match self {
            // ── General ──────────────────────────────────────────────────────
            Self::ExtensionsAutoRefresh => toggle(
                "Let projects refresh their own status badges",
                "A project's veld.json can declare status badges for the top bar — a pull \
                 request's state, a deploy tag — each backed by a command Veld runs in that \
                 worktree and re-runs on the interval the project asked for. This is the only \
                 thing Veld runs from a repo's configuration without you clicking something, so \
                 it has a switch. Turning it off leaves the project's buttons and menus working \
                 (a click is you asking) and stops only the unattended half; badges then render \
                 nothing. Veld bounds these commands either way: no terminal is attached, so a \
                 tool that would ask for credentials fails instead of waiting; there is a hard \
                 timeout and an output limit; a minimum refresh interval and a cap on how many a \
                 project may declare; and every command is written to the daemon log with its \
                 full arguments.",
                General,
            ),
            Self::ExtensionsSource => Spec {
                title: "Read a project's extensions from",
                help: "Main checkout: every worktree of a project sees whatever extensions are \
                       declared in your primary clone, at whatever it has checked out — the \
                       setting for everyone, so a worktree cloned before a project added \
                       ide.extensions still sees them. This worktree: the checked-out worktree's \
                       own veld.json decides, so you can test a new or edited declaration before \
                       merging it. Either way commands still run in the worktree you're looking \
                       at, using its own branch and root.",
                group: General,
                section: None,
                shape: ValueShape::Text,
                choices: Choices::Static {
                    options: EXTENSIONS_SOURCES,
                },
                requires: None,
            },
            Self::WorktreeMarkerStyle => Spec {
                title: "Worktree marker",
                help: "Both a colour and a glyph are stored for every worktree, so switching here \
                       never loses the other one. Pick either from a worktree's context menu → \
                       Change marker…",
                group: General,
                section: None,
                shape: ValueShape::Text,
                choices: Choices::Static {
                    options: MARKER_STYLES,
                },
                requires: None,
            },
            Self::WorktreeTrashRetention => Spec {
                title: "Empty the worktree trash after",
                help: "0 keeps trashed worktrees until you empty the trash yourself — the \
                       default. Set a number of days and a worktree you moved to the trash is \
                       deleted for good after that long. Deleting still goes through git, so a \
                       checkout that picked up uncommitted changes comes back with the reason \
                       instead of being discarded.",
                group: General,
                section: None,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    // Zero is outside the clamped range on purpose (see the
                    // validator) — the offered floor is the off switch itself.
                    min: 0,
                    max: MAX_TRASH_RETENTION_DAYS,
                    step: Some(7),
                    unit: Some("days"),
                    empty_means: Some("keep"),
                },
                requires: None,
            },
            Self::RunsHistoryDays => Spec {
                title: "Show run history from the last",
                help: "Hides ended runs older than this from the History tab and the past-run \
                       pickers. Nothing is deleted — and nothing is kept longer either: veld's \
                       housekeeping already removes ended runs after 7 days, which is why that \
                       is the maximum. Leave it empty to show everything the daemon still has.",
                group: General,
                section: None,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_RUN_HISTORY_DAYS,
                    max: MAX_RUN_HISTORY_DAYS,
                    step: None,
                    unit: Some("days"),
                    empty_means: Some("all"),
                },
                requires: None,
            },
            Self::LogsTimeZone => Spec {
                title: "Show log timestamps in",
                help: "Display only — every line is stored in UTC, which is what veld sorts and \
                       interleaves by. Local is your browser's zone; hover a timestamp for the \
                       date, both zones and the exact stored value. veld logs and veld start \
                       --attach follow this setting too, and veld logs takes --utc / --local to \
                       override it for one command.",
                group: General,
                section: None,
                shape: ValueShape::Text,
                choices: Choices::Static {
                    options: LOG_TIME_ZONES,
                },
                requires: None,
            },
            Self::UiHideDisabledActions => toggle(
                "Hide top-bar actions that can't fire",
                "On, the restart, machine-vars and URLs buttons disappear while they have \
                 nothing to act on — a run stopped, a project that asks for no values, no URLs \
                 to open. Off, every button stays and the ones that can't fire are greyed out, \
                 so the bar never changes shape. This is only about hiding versus disabling; it \
                 never removes a control that could fire.",
                General,
            ),
            Self::UiShowProjectColumn => toggle(
                "Show the project column",
                "A column of projects down the left edge, beside the worktree rail — each one \
                 carrying what its checkouts have to say, so another project's waiting agent is \
                 on screen without a click. ⌘1…⌘9 go straight to a project and squares are \
                 dragged to reorder. Off by default because most installs have one project. ⌘B, \
                 or the stacked-layers button in the top bar, is the same switch.",
                General,
            ),
            Self::UiShowProjectNews => toggle(
                "Show news from your projects",
                "On, a project can tell its own team something changed — a card written into \
                 its veld.json, shown once, labelled with the project's name. Off, only Veld's \
                 own news appears. Turning it off does not mark anything read, so anything you \
                 missed is still in What's new when you turn it back on.",
                General,
            ),
            Self::NewsSource => Spec {
                title: "Read project news from",
                help: "Main checkout: a card only reaches you once it's checked out in your \
                       primary clone — the setting for everyone, so a card being drafted on a \
                       branch never prompts a teammate. This worktree: every checked-out \
                       worktree's own veld.json is read and merged in, so you can preview a card \
                       before merging it. Meant for testing a card you're writing, not for daily \
                       use — reading or dismissing a preview marks its id read for real, so the \
                       merged card won't prompt you again either.",
                group: General,
                section: None,
                shape: ValueShape::Text,
                choices: Choices::Static {
                    options: NEWS_SOURCES,
                },
                requires: None,
            },

            // ── General › Database backups ───────────────────────────────────
            Self::BackupEnabled => toggle_in(
                "Back up Veld's database",
                "Everything Veld knows lives in one file: your repositories, your worktrees and \
                 their lanes and markers, pane layouts, run history, these settings. Nothing else \
                 on the machine has a copy, and losing that file is a fresh install rather than a \
                 bad day. On, Veld writes a compact copy of it on the interval below — logs and \
                 resource samples are left out, so a copy is a few megabytes rather than the \
                 hundreds the live file can reach. Turn it off if you already snapshot your home \
                 directory and would rather Veld stayed out of it.",
                General,
                DATABASE_BACKUPS,
            ),
            Self::BackupIntervalMinutes => Spec {
                title: "Back up every",
                help: "How far back the worst case throws you: a database lost just before the \
                       next copy costs you this much rearranging. The work being protected is \
                       arrangement rather than content — which repositories are registered, which \
                       checkouts exist, how they are laid out — so an hour of it is a few minutes \
                       to redo. Shorten it if you reorganise constantly; the copy is cheap either \
                       way.",
                group: General,
                section: DATABASE_BACKUPS,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_BACKUP_INTERVAL_MINUTES,
                    max: MAX_BACKUP_INTERVAL_MINUTES,
                    step: None,
                    unit: Some("min"),
                    empty_means: None,
                },
                requires: requires_true("backup.enabled"),
            },
            Self::BackupKeep => Spec {
                title: "Keep the most recent",
                help: "Generations to keep, newest first. More than one because nothing \
                       guarantees Veld noticed a database had gone wrong before it copied it — \
                       the generations are what let you step back past a bad one.",
                group: General,
                section: DATABASE_BACKUPS,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_BACKUP_KEEP,
                    max: MAX_BACKUP_KEEP,
                    step: None,
                    unit: Some("copies"),
                    empty_means: None,
                },
                requires: requires_true("backup.enabled"),
            },
            Self::BackupKeepDaily => Spec {
                title: "And one per day for",
                help: "A count on its own bounds disk space and not time: twelve copies taken \
                       every five minutes is an hour of history, so a problem you only notice the \
                       next morning has nothing left to go back to. This keeps the first copy of \
                       each day beyond the recent ones. 0 keeps only the recent ones.",
                group: General,
                section: DATABASE_BACKUPS,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_BACKUP_KEEP_DAILY,
                    max: MAX_BACKUP_KEEP_DAILY,
                    step: Some(7),
                    unit: Some("days"),
                    empty_means: Some("none"),
                },
                requires: requires_true("backup.enabled"),
            },
            Self::BackupDir => Spec {
                title: "Backup folder",
                help: "Empty means the folder Veld picks beside its own data directory, which is \
                       on the same disk as the database it is copying — that survives the file \
                       going bad, and it does not survive the disk going with it. Point this at \
                       an external drive or a synced folder if you want the copies somewhere the \
                       machine's own storage cannot take with it. An absolute path. Veld only \
                       ever deletes files it wrote itself from here, and never changes the \
                       folder's own permissions, so it is safe to share with something else. \
                       Worth knowing before you do: a copy carries everything the database does, \
                       including the tokens for your relays — Veld writes each one readable only \
                       by you, which a drive formatted FAT or a network share cannot honour, and \
                       says so when that happens. `veld backup` shows what is in the folder.",
                group: General,
                section: DATABASE_BACKUPS,
                shape: ValueShape::Text,
                choices: Choices::Runtime {
                    source: RuntimeSource::Directory,
                },
                requires: requires_true("backup.enabled"),
            },

            // ── Git ──────────────────────────────────────────────────────────
            Self::GitCreateFrom => Spec {
                title: "Create worktrees from",
                help: "Origin (recommended): fetching the remote and cutting the new branch from \
                       origin's default branch, so a worktree is never born behind the latest \
                       database migrations and open PRs. Local: the main checkout's current HEAD \
                       — handy when you are offline or deliberately basing on un-pushed local \
                       work.",
                group: Git,
                section: None,
                shape: ValueShape::Text,
                choices: Choices::Static {
                    options: GIT_CREATE_SOURCES,
                },
                requires: None,
            },
            Self::WorktreeStorageMode => Spec {
                title: "Worktree storage location",
                help: "Next to repository (default): a new checkout lands in a _worktrees folder \
                       beside its repo — today's behaviour. Custom location: every new checkout, \
                       for every repository, lands under one folder you choose. Either way each \
                       repo gets its own subfolder there, so two repos can never collide on the \
                       same checkout path. Only affects worktrees created from now on; nothing \
                       already on disk moves.",
                group: Git,
                section: None,
                shape: ValueShape::Text,
                choices: Choices::Static {
                    options: WORKTREE_STORAGE_MODES,
                },
                requires: None,
            },
            Self::WorktreeStorageDir => Spec {
                // Had no help at all while it was dialog-only: the row is hidden
                // unless the mode above is Custom, so the mode's own help carried
                // it. A CLI has no such adjacency — `veld settings describe` is
                // reached by naming this key on its own.
                title: "Custom worktree folder",
                help: "The folder new checkouts land under when the storage location is Custom. \
                       An absolute path, and each repository gets its own subfolder inside it. \
                       Empty means no folder has been chosen yet, and new checkouts still land \
                       beside their repository until one is.",
                group: Git,
                section: None,
                shape: ValueShape::Text,
                choices: Choices::Runtime {
                    source: RuntimeSource::Directory,
                },
                requires: requires_eq("worktree.storageMode", "custom"),
            },

            // ── Terminal › Appearance ────────────────────────────────────────
            Self::TerminalFontSize => Spec {
                title: "Font size",
                help: "How large a terminal renders its text, in pixels. Applies to every open \
                       terminal immediately.",
                group: Terminal,
                section: APPEARANCE,
                shape: ValueShape::Int,
                choices: range(MIN_FONT_SIZE, MAX_FONT_SIZE),
                requires: None,
            },
            Self::TerminalFontFamily => Spec {
                title: "Font",
                help: "A CSS font-family list, so it can name fallbacks. Veld bundles JetBrains \
                       Mono, which renders the same on every machine; anything else has to be \
                       installed wherever you open Veld. Ends up inside a stylesheet rule, so \
                       { } ; < > are refused.",
                group: Terminal,
                section: APPEARANCE,
                shape: ValueShape::Text,
                choices: Choices::Runtime {
                    source: RuntimeSource::Fonts,
                },
                requires: None,
            },
            Self::TerminalCursorStyle => Spec {
                title: "Cursor",
                help: "The shape of a terminal's cursor. A full-screen program that sets its own \
                       cursor shape still wins while it is running.",
                group: Terminal,
                section: APPEARANCE,
                shape: ValueShape::Text,
                choices: Choices::Static {
                    options: CURSOR_STYLES,
                },
                requires: None,
            },
            Self::TerminalCursorBlink => toggle_in(
                "Blinking cursor",
                "Whether a terminal's cursor blinks. Off is steadier to sit beside when a lot \
                 of output is already moving.",
                Terminal,
                APPEARANCE,
            ),

            // ── Terminal › Behaviour ─────────────────────────────────────────
            Self::TerminalShell => Spec {
                title: "Shell",
                help: "Which shell new terminals and config-declared panes open, and the one \
                       Veld asks to learn your PATH. Automatic is your login shell; pick another \
                       if your aliases and integrations live in a different shell's startup \
                       files. Must be auto or an absolute path — a bare name would be looked up \
                       on the daemon's own PATH, which is not your terminal's. A terminal \
                       already open keeps the shell it started with.",
                group: Terminal,
                section: BEHAVIOUR,
                shape: ValueShape::Text,
                choices: Choices::Runtime {
                    source: RuntimeSource::Shells,
                },
                requires: None,
            },
            Self::TerminalScrollback => Spec {
                title: "Scrollback",
                help: "Lines kept per terminal. Lowering this drops the oldest lines from every \
                       live terminal immediately.",
                group: Terminal,
                section: BEHAVIOUR,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_SCROLLBACK,
                    max: MAX_SCROLLBACK,
                    step: None,
                    unit: Some("lines"),
                    empty_means: None,
                },
                requires: None,
            },
            Self::TerminalShiftEnterNewline => toggle_in(
                "Shift+Enter inserts a newline",
                "Sends ESC CR, which is what Claude Code's /terminal-setup configures. Turn off \
                 if a TUI you use binds meta-Enter.",
                Terminal,
                BEHAVIOUR,
            ),
            Self::TerminalBellVolume => Spec {
                title: "Bell volume",
                help: "How loud the terminal bell rings when a process sends a BEL — the \
                       baseline 'something finished' signal. 0 is silent. Takes effect \
                       immediately for new bells.",
                group: Terminal,
                section: BEHAVIOUR,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_BELL_VOLUME,
                    max: MAX_BELL_VOLUME,
                    step: Some(5),
                    // The unit is what makes this a slider rather than a number
                    // box: a percentage is dragged, not typed.
                    unit: Some("%"),
                    empty_means: None,
                },
                requires: None,
            },
            Self::TerminalDetachGrace => Spec {
                title: "Keep detached shells for",
                help: "Minutes a terminal with nobody attached keeps running before it is \
                       collected. Takes effect for new shells and for the next collection pass; \
                       shells already running keep the value they started with.",
                group: Terminal,
                section: BEHAVIOUR,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_DETACH_GRACE_MINUTES,
                    max: MAX_DETACH_GRACE_MINUTES,
                    step: None,
                    unit: Some("min"),
                    empty_means: None,
                },
                requires: None,
            },

            // ── Terminal › Auto-reconnect ────────────────────────────────────
            Self::TerminalReconnectTries => Spec {
                title: "Reconnect attempts on drop",
                help: "How many times a dropped connection reattaches to the running shell \
                       before it gives up and shows the Reconnect button (the machine slept, the \
                       daemon restarted mid-update, a proxy timed out — the shell keeps running, \
                       which is what the holder process is for). 0 turns it off: a dropped \
                       socket always waits for a click. Each attempt reattaches to the same \
                       shell, never starts a new one.",
                group: Terminal,
                section: AUTO_RECONNECT,
                shape: ValueShape::Int,
                choices: range(MIN_RECONNECT_TRIES, MAX_RECONNECT_TRIES),
                requires: None,
            },
            Self::TerminalReconnectFirstDelaySeconds => Spec {
                title: "First attempt after",
                help: "Seconds before the first auto-reconnect fires. The first reconnect is the \
                       near-immediate one that fixes a sleep or a dropped proxy, so it defaults \
                       small — 1s — and this is the setting to raise if a flaky network is \
                       racing every blip.",
                group: Terminal,
                section: AUTO_RECONNECT,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_RECONNECT_FIRST_DELAY_SECONDS,
                    max: MAX_RECONNECT_FIRST_DELAY_SECONDS,
                    step: None,
                    unit: Some("s"),
                    empty_means: None,
                },
                requires: None,
            },
            Self::TerminalReconnectBackoffSeconds => Spec {
                title: "Wait between later attempts",
                help: "Seconds between attempts after the first. This is the backoff: a \
                       connection still failing is not hammering a daemon that is itself coming \
                       back, so later attempts space out to this interval.",
                group: Terminal,
                section: AUTO_RECONNECT,
                shape: ValueShape::Int,
                choices: Choices::Range {
                    min: MIN_RECONNECT_BACKOFF_SECONDS,
                    max: MAX_RECONNECT_BACKOFF_SECONDS,
                    step: None,
                    unit: Some("s"),
                    empty_means: None,
                },
                requires: None,
            },

            // ── Activity › Noticing ──────────────────────────────────────────
            Self::TerminalShellIntegration => toggle_in(
                "Notice when a command finishes",
                "Veld registers two hooks in the shell it opens, which print an invisible \
                 marker when a command starts and when it ends. A command that finishes in a \
                 terminal you are not looking at then marks its worktree in the rail — and its \
                 pane's tab, so you can tell which one. A shell sitting at a prompt never \
                 counts, and neither does a watcher that has not ended, so `pnpm dev` stays \
                 silent. Nothing of yours is edited: the hooks live in a file Veld owns and \
                 rewrites on every start. zsh, and bash 4.4 or newer — macOS's own /bin/bash is \
                 3.2 and cannot carry it. Takes effect for new terminals; a running shell keeps \
                 the environment it started with.",
                Activity,
                NOTICING,
            ),
            Self::TerminalAgentIntegration => toggle_in(
                "Notice when a coding agent is waiting for you",
                "Works with Claude Code, Codex CLI and Pi. Tells Veld when one is working, \
                 waiting for you, or finished, without touching any of your own agent config. \
                 Takes effect for new terminals.",
                Activity,
                NOTICING,
            ),
            Self::ActivityShowWorking => toggle_in(
                "Show what is working",
                "A spinner for any worktree with something running. Accuracy varies by \
                 producer: exact for a shell command, absent for a coding agent Veld has no \
                 hook for. Loses to every unseen event in the rail, so a worktree waiting for \
                 you still reads as waiting rather than as working.",
                Activity,
                NOTICING,
            ),

            // ── Activity › Notifying ─────────────────────────────────────────
            Self::ActivityNotifyCommandFinished => toggle_in(
                "A command finished",
                "Off by default: a build that succeeded is news, and the rail already carries \
                 it. This is the row most likely to make someone turn notifications off \
                 wholesale.",
                Activity,
                NOTIFYING,
            ),
            Self::ActivityNotifyCommandFailed => toggle_in(
                "A command failed",
                "The one 'it ended' event that is actionable — and finding out twenty minutes \
                 later is the cost this exists to remove.",
                Activity,
                NOTIFYING,
            ),
            Self::ActivityNotifyAgentWaiting => toggle_in(
                "A coding agent is waiting for you",
                "It stopped at a permission prompt, a question, or a plan to approve — and it \
                 will sit there until you answer. The single most actionable thing Veld can tell \
                 you about a pane you are not looking at.",
                Activity,
                NOTIFYING,
            ),
            Self::ActivityNotifyAgentFinished => toggle_in(
                "A coding agent finished",
                "Note the frequency: an agent's end-of-turn signal fires after every response, \
                 not once per session — so this is a banner each time one hands control back \
                 while you are elsewhere. That is the point if you walked away, and the first \
                 row to turn off if it is not. Sub-agents do not count: an agent that farms work \
                 out announces each helper finishing, and none of those is yours to act on, so \
                 only the session's own turn is reported here.",
                Activity,
                NOTIFYING,
            ),
            Self::ActivityNotifyNoticed => toggle_in(
                "A program asked to be noticed",
                "Any program can ring the terminal's notification sequence (OSC 9) with a \
                 message — a test runner, a deploy script, a tool Veld knows nothing about. Its \
                 own row rather than the agent one above, because that label has to be able to \
                 say what it covers. Veld cannot yet tell that a plain program is merely \
                 *waiting* for input: that is not observable from the browser at all, so a \
                 program has to say so itself.",
                Activity,
                NOTIFYING,
            ),

            // ── Activity › Focus mode ────────────────────────────────────────
            Self::FocusModeEnabled => toggle_in(
                "Focus mode",
                "Master switch. The three rows below decide what it silences while it's on; \
                 none of them do anything while it's off.",
                Activity,
                FOCUS_MODE,
            ),
            Self::FocusModeSuppressBell => Spec {
                requires: requires_true("focus.enabled"),
                ..toggle_in(
                    "The terminal bell",
                    "The audible BEL a shell or program rings — separate from the notification \
                     table below, which is about a banner or toast, not a sound.",
                    Activity,
                    FOCUS_MODE,
                )
            },
            Self::FocusModeSuppressToasts => Spec {
                requires: requires_true("focus.enabled"),
                ..toggle_in(
                    "In-app toasts",
                    "The 'a pane finished while you weren't looking' toast (notifyTerminal). \
                     Feedback for something you clicked yourself — a failed action, a copy \
                     confirmation — is never gated by focus mode; only the background-activity \
                     channel is.",
                    Activity,
                    FOCUS_MODE,
                )
            },
            Self::FocusModeSuppressOsNotifications => Spec {
                requires: requires_true("focus.enabled"),
                ..toggle_in(
                    "OS-level notifications",
                    "The native banner Veld's own notification path raises (Veld Desktop, or a \
                     browser tab's Web Notification). This is not a system-wide Do Not Disturb — \
                     nothing else on the machine is muted.",
                    Activity,
                    FOCUS_MODE,
                )
            },

            // ── Keep awake › While you're sharing ────────────────────────────
            Self::KeepAwakeSharingOnPower => toggle_in(
                "Keep this machine awake while sharing, on mains power",
                "Covers a shut lid too, which costs nothing here: the macOS flag for it is \
                 valid on AC power only, so no privileged helper is involved.",
                KeepAwake,
                WHILE_SHARING,
            ),
            Self::KeepAwakeSharingOnPowerMinutes => Spec {
                title: "For at most",
                help: "A ceiling, not a countdown — the hold normally ends when the share does, \
                       since shares expire on their own. Measured from when the sharing started, \
                       and it starts again if you unplug or plug in.",
                group: KeepAwake,
                section: WHILE_SHARING,
                shape: ValueShape::Int,
                choices: Choices::Presets {
                    offered: KEEP_AWAKE_PRESETS,
                    min: MIN_KEEP_AWAKE_MINUTES,
                    max: MAX_KEEP_AWAKE_MINUTES,
                    unit: Some("min"),
                },
                requires: requires_true("keepAwake.sharingOnPower"),
            },
            Self::KeepAwakeSharingOnBattery => toggle_in(
                "Keep this machine awake while sharing, on battery",
                "Holds off idle sleep only. A shut lid still sleeps the machine, so a laptop \
                 that goes in a bag mid-share sleeps the way it always did.",
                KeepAwake,
                WHILE_SHARING,
            ),
            Self::KeepAwakeSharingOnBatteryMinutes => Spec {
                title: "For at most",
                help: "Shorter than the mains allowance on purpose: this one is spending your \
                       charge.",
                group: KeepAwake,
                section: WHILE_SHARING,
                shape: ValueShape::Int,
                choices: Choices::Presets {
                    offered: KEEP_AWAKE_PRESETS,
                    min: MIN_KEEP_AWAKE_MINUTES,
                    max: MAX_KEEP_AWAKE_MINUTES,
                    unit: Some("min"),
                },
                requires: requires_true("keepAwake.sharingOnBattery"),
            },

            // ── Keep awake › When you ask ────────────────────────────────────
            Self::KeepAwakeManualOnBattery => toggle_in(
                "Cover a shut lid on battery too",
                "The one case that needs the privileged helper, because macOS offers no \
                 unprivileged way to ask. Turning this off is a guarantee rather than a \
                 preference: Veld then never writes pmset disablesleep on this machine, even \
                 when you pick a length from the coffee menu — and a shut lid on battery sleeps.",
                KeepAwake,
                WHEN_YOU_ASK,
            ),

            // ── Sharing ──────────────────────────────────────────────────────
            Self::SharingPeerTtlMinutes => Spec {
                title: "A share with another Veld user",
                // No "above"/"below" in a spec's help: the catalog feeds `veld
                // settings describe` as well as the dialog, and a positional
                // reference is a lie on a terminal.
                help: "How long a peer share link keeps working. This is what usually ends a \
                       share — and so what ends the automatic keep-awake with it, since the \
                       keepAwake.sharing* caps are a ceiling over this rather than a second \
                       countdown. This machine only: to change it for everyone who checks a \
                       project out, set sharing.peer_ttl_minutes in its veld.json, which wins \
                       over this. veld share --ttl wins over both, for one share. Applies to \
                       the next share, not one already running.",
                group: Sharing,
                section: SHARE_LINKS_EXPIRE,
                shape: ValueShape::Int,
                choices: Choices::Presets {
                    offered: SHARE_TTL_PRESETS,
                    min: MIN_SHARE_TTL_MINUTES,
                    max: MAX_SHARE_TTL_MINUTES,
                    unit: Some("min"),
                },
                requires: None,
            },
            Self::SharingWebTtlMinutes => Spec {
                title: "A share on the public web",
                help: "Shorter than a peer share by default, and for a reason worth keeping: \
                       the audience is the open internet, so an idle share should die sooner. \
                       This machine only, like the peer setting: a project's \
                       sharing.web_ttl_minutes wins over it, and veld share --ttl over both. \
                       Applies to the next share, not one already running.",
                group: Sharing,
                section: SHARE_LINKS_EXPIRE,
                shape: ValueShape::Int,
                choices: Choices::Presets {
                    offered: SHARE_TTL_PRESETS,
                    min: MIN_SHARE_TTL_MINUTES,
                    max: MAX_SHARE_TTL_MINUTES,
                    unit: Some("min"),
                },
                requires: None,
            },

            // ── Links ────────────────────────────────────────────────────────
            Self::TerminalOpenUrlsInApp => toggle(
                "Open links from the terminal in Veld",
                "A URL you click in the terminal output, and a URL a program in it opens (Veld \
                 points $BROWSER at itself, which is what Claude Code, gh, git, vite and next \
                 all use), become a browser pane beside that terminal. Off sends both to your \
                 system browser and puts nothing in the shell at all. Clicking a link responds \
                 immediately; the rest takes effect for new terminals, since a running shell \
                 keeps the environment it started with. Hold ⌘/Ctrl while clicking a link to go \
                 to your browser just once.",
                Links,
            ),
            Self::TerminalInterceptSystemOpen => Spec {
                requires: requires_true("terminal.openUrlsInApp"),
                ..toggle(
                    "Also catch programs that call open / xdg-open",
                    "Most tools read $BROWSER, but some call the system opener directly — \
                     including an agent's shell tool (Bash(open “https://…”)). For those, Veld \
                     puts a small shim directory on the PATH of each terminal. It needs the last \
                     word after your shell's startup files, so Veld points ZDOTDIR at a \
                     directory of its own holding one .zshenv: that file hands ZDOTDIR straight \
                     back, sources your real .zshenv, and registers a hook. Your .zprofile, \
                     .zshrc and .zlogin are read normally, in order, and nothing of yours is \
                     edited. In bash it uses the equivalent seam — posix mode’s $ENV, the only \
                     startup file an interactive --posix bash reads — replaying your own startup \
                     itself; that is probed per binary, because macOS ships bash 3.2 as \
                     /bin/bash and 3.2 ignores $ENV. Other shells keep $BROWSER and can add \
                     $VELD_SHIM_DIR to PATH by hand. The Shell row above reports whether it \
                     actually worked. Takes effect for new terminals.",
                    Links,
                )
            },
            Self::BrowserExternalOrigins => Spec {
                title: "Always open these in the system browser",
                help: "One origin per line — `https://accounts.google.com`, \
                       `https://*.okta.com`, `http://localhost:*` — for the sign-ins that need \
                       the browser you are already logged into. A pane has its own cookie jar, \
                       so an SSO flow in one starts from scratch. A project can add to this list \
                       without touching your settings: `ide.externalOrigins` in its veld.json.",
                group: Links,
                section: None,
                shape: ValueShape::TextList,
                choices: Choices::Free,
                requires: requires_true("terminal.openUrlsInApp"),
            },

            // ── Browser panes ────────────────────────────────────────────────
            Self::BrowserQuickSwitchResponsive => toggle(
                "Responsive switch in the pane toolbar",
                "One click into the resizable viewport, whose edges you drag to find where a \
                 layout breaks. The switch's own off state is no emulation at all — it does not \
                 go back to a device you picked earlier. Unchecking this hides the button; it \
                 changes nothing a pane is currently emulating.",
                Browser,
            ),
            Self::BrowserQuickSwitchColorScheme => toggle(
                "Colour-scheme switch in the pane toolbar",
                "Cycles the page's prefers-color-scheme through System, Dark and Light — the \
                 page in the pane, not Veld itself. System is the absence of an override rather \
                 than a third value. Needs Veld Desktop; in a browser tab the button is shown \
                 inert and its tooltip says why.",
                Browser,
            ),
            Self::BrowserSearchUrl => Spec {
                title: "Search from the address bar",
                help: "Where a browser pane sends words that are not an address, so you can look \
                       something up in the pane you are working in. `%s` is where the words go — \
                       the same spelling a browser's own custom-engine field uses, so a URL \
                       copied from one works here. Leave it empty to turn search off; the \
                       address bar then only accepts addresses.",
                group: Browser,
                section: None,
                shape: ValueShape::Text,
                choices: Choices::Free,
                requires: None,
            },

            // ── Browser panes › Local files ──────────────────────────────────
            // Four switches rather than one, because "which files" is the question
            // a user actually has and an extension list is not an answer to it.
            // Each one names its own extensions in the help, so the row is
            // self-describing without a docs trip.
            Self::FilesViewWebPages => toggle_in(
                "Web pages",
                "`.html` and `.htm` — a slide deck or a report an agent wrote, opened in a pane \
                 instead of in your other browser. Served over http from a Veld-only origin, so \
                 module scripts and `fetch` work, which they do not for a file opened directly.",
                Browser,
                LOCAL_FILES,
            ),
            Self::FilesViewImages => toggle_in(
                "Images",
                "`.png`, `.jpg`, `.gif`, `.webp`, `.avif`, `.svg`, `.ico`, `.bmp` — a diagram or \
                 a screenshot, at full size, without leaving the window. Off by default, unlike \
                 web pages and PDFs: a repository's images are overwhelmingly committed assets, \
                 so switching them on puts a logo and a vendored diagram above the report an \
                 agent wrote a minute ago.",
                Browser,
                LOCAL_FILES,
            ),
            Self::FilesViewPdfs => toggle_in(
                "PDFs",
                "`.pdf`, in the viewer the pane already has.",
                Browser,
                LOCAL_FILES,
            ),
            Self::FilesViewPlainText => toggle_in(
                "Plain text",
                "`.txt`, `.log`, `.md`, `.json`, `.csv`, `.tsv`, `.yaml`, `.toml`, `.xml`, shown \
                 verbatim rather than rendered. Off by default because text files outnumber \
                 everything else in a repository — switching them on means every `README.md` is \
                 a candidate, and the recently-edited list stops being a short one.",
                Browser,
                LOCAL_FILES,
            ),
            Self::FilesViewPatterns => Spec {
                title: "Also treat these as viewable",
                help: "One glob per line, to reach files the switches above leave out — \
                       `reports/*.xml` for one folder, `*.log` without turning on all plain \
                       text. `*` matches within one path segment and `**` across them; a \
                       pattern with no `/` matches a file name at any depth. A pattern \
                       *chooses among* the kinds Veld can display and cannot invent one, so \
                       `*.mmd` matches nothing — and it cannot reach into `node_modules`, \
                       `target`, `dist`, `build` or `vendor`, which are never scanned. \
                       Secrets are never served whatever you write here: `.git`, `.env*`, \
                       `*.pem`, `*.key` and their neighbours are refused first.",
                group: Browser,
                section: LOCAL_FILES,
                shape: ValueShape::TextList,
                choices: Choices::Free,
                requires: None,
            },
            Self::FilesWatchByDefault => toggle_in(
                "Reload a file pane when the file changes",
                "For the loop this feature exists for: an agent rewrites the deck and the pane \
                 shows the new one without being asked. Veld watches the file's timestamp and \
                 reloads the view — nothing is injected into the page, so what you present is \
                 what is on disk. It follows the pane: a deck you reached by a link is watched \
                 like the one you opened. Each pane's toolbar can override this for itself.",
                Browser,
                LOCAL_FILES,
            ),

            // A preference this build is preserving, not describing.
            Self::Unknown(_) => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::settings::SettingKey;

    /// Every key in [`SettingKey::ALL`] has a spec.
    ///
    /// Non-vacuous, but narrower than it looks: [`catalog`] *walks* `ALL`, so the
    /// length comparison below only re-states that no entry was dropped by
    /// `filter_map` — i.e. that every listed key is described. It says nothing
    /// about a key that is not listed. `every_setting_variant_is_listed` is the
    /// test for that, and the two are deliberately separate because they fail for
    /// different reasons.
    #[test]
    fn every_listed_key_is_described() {
        for key in SettingKey::ALL {
            assert!(
                key.spec().is_some(),
                "{} has no catalog entry",
                key.as_str()
            );
        }
        assert_eq!(catalog().len(), SettingKey::ALL.len());
    }

    /// Every `SettingKey` variant actually appears in [`SettingKey::ALL`].
    ///
    /// **This is the gap the exhaustive matches cannot close, and it is silent.**
    /// `as_str`, `parse`, `validate` and `spec` are all exhaustive, so a new
    /// variant cannot compile without being given a name, a validator and a
    /// description — but `ALL` is a hand-written list, and a variant missing from
    /// it validates and stores perfectly while being invisible to `catalog()`,
    /// the settings dialog and `veld settings`. Every other test in this
    /// subsystem iterates `ALL`, so every one of them passes.
    ///
    /// Counted out of the source text rather than derived from the type, because
    /// nothing in Rust enumerates an enum's variants without a macro or a new
    /// dependency, and this file already carries the repo's precedent for a
    /// source-text tripwire (`only_one_site_in_this_module_persists_a_live_node_pid`
    /// counts assignment spellings; `documented_run_history_horizon_matches_the_constants`
    /// greps tracked prose). A tripwire, not a proof: it can be defeated by
    /// writing two variants on one line. It cannot be defeated by forgetting.
    #[test]
    fn every_setting_variant_is_listed() {
        let source = include_str!("settings.rs");
        let body = source
            .split_once("pub enum SettingKey {")
            .expect("SettingKey's declaration moved — update this tripwire")
            .1
            .split_once("\n}")
            .expect("SettingKey's declaration is not brace-terminated")
            .0;

        let declared: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|line| {
                // Variants only: skip doc comments, plain comments, attributes
                // and blank lines. A variant line is `Name,` or `Name(String),`.
                !line.is_empty()
                    && !line.starts_with("//")
                    && !line.starts_with('#')
                    && line.ends_with(',')
            })
            .collect();

        assert_eq!(
            declared.len(),
            SettingKey::ALL.len() + 1,
            "SettingKey declares {} variants and ALL lists {} (+1 for Unknown). \
             Declared: {declared:?}. A variant missing from ALL still validates and \
             stores, but is invisible to the catalog, the dialog and the CLI.",
            declared.len(),
            SettingKey::ALL.len(),
        );
    }

    /// An unrecognised key is preserved, not described — the forward-compat rule
    /// in `settings.rs`'s module docs. A consumer must handle a value with no
    /// entry, because `Db::settings` returns unknown keys in the document.
    #[test]
    fn an_unknown_key_has_no_spec() {
        assert!(
            SettingKey::Unknown("veld.fromTheFuture".into())
                .spec()
                .is_none()
        );
    }

    /// Every default is a value its own key would accept, and — for the enums —
    /// one of the choices the catalog offers.
    ///
    /// Cheap, but it is the assertion that would have caught a default and an
    /// allow-list drifting apart back when they were written in two places, and
    /// it stays honest now that they are written in one.
    #[test]
    fn every_default_is_offered_and_accepted() {
        let defaults = defaults();
        for entry in catalog() {
            let default = defaults
                .get(&entry.key)
                .unwrap_or_else(|| panic!("{} has no default", entry.key));
            match entry.choices {
                Choices::Static { options } => {
                    let got = default
                        .as_str()
                        .unwrap_or_else(|| panic!("{}'s default is not a string", entry.key));
                    assert!(
                        options.iter().any(|c| c.value == got),
                        "{}'s default {got:?} is not one of its offered choices",
                        entry.key
                    );
                }
                Choices::Range { min, max, .. } | Choices::Presets { min, max, .. } => {
                    let got = default
                        .as_i64()
                        .unwrap_or_else(|| panic!("{}'s default is not a number", entry.key));
                    // Deliberately `<=`/`>=` rather than "inside the offered
                    // presets": `keepAwake.*Minutes` accept the whole range and
                    // only *offer* six values, and `worktree.trashRetentionDays`
                    // defaults to its off switch. Asserting membership of the
                    // offered set would fail on exactly the keys this catalog
                    // exists to describe honestly.
                    assert!(
                        (min..=max).contains(&got),
                        "{}'s default {got} is outside [{min}, {max}]",
                        entry.key
                    );
                }
                Choices::Free | Choices::Runtime { .. } => {}
            }
        }
    }

    /// A `requires` names a real setting, and the value it expects is one that
    /// setting can actually hold.
    ///
    /// A typo here disables a row forever with no error anywhere — the gate just
    /// never opens, and the setting is unreachable in the UI while still being
    /// settable from the CLI.
    #[test]
    fn every_dependency_points_at_a_real_setting() {
        let entries = catalog();
        for entry in &entries {
            let Some(dep) = entry.requires else { continue };
            let target = entries
                .iter()
                .find(|e| e.key == dep.key)
                .unwrap_or_else(|| panic!("{} requires unknown setting {}", entry.key, dep.key));
            match dep.equals {
                None => assert_eq!(
                    target.value_type,
                    ValueShape::Bool.as_str(),
                    "{} gates on {} being true, but {} is not a boolean",
                    entry.key,
                    dep.key,
                    dep.key
                ),
                Some(expected) => {
                    let Choices::Static { options } = target.choices else {
                        panic!(
                            "{} gates on {} == {expected:?}, but {} is not a closed set",
                            entry.key, dep.key, dep.key
                        )
                    };
                    assert!(
                        options.iter().any(|c| c.value == expected),
                        "{} gates on {} == {expected:?}, which {} cannot hold",
                        entry.key,
                        dep.key,
                        dep.key
                    );
                }
            }
        }
    }

    /// Every setting lands in a group the catalog also publishes, and the
    /// sections inside a group are **contiguous**.
    ///
    /// The second half is what makes `SettingKey::ALL`'s order load-bearing: a
    /// renderer emits a heading when the section changes between consecutive
    /// entries, so a key inserted in the wrong place does not merely sort oddly
    /// — it prints the same heading twice with a foreign row between them.
    #[test]
    fn groups_and_sections_are_contiguous() {
        let groups: Vec<&str> = catalog_groups().iter().map(|g| g.id).collect();
        let mut seen_groups: Vec<&str> = Vec::new();
        let mut seen_sections: Vec<(&str, Option<&str>)> = Vec::new();

        for entry in catalog() {
            assert!(
                groups.contains(&entry.group),
                "{} is in unpublished group {}",
                entry.key,
                entry.group
            );
            if seen_groups.last() != Some(&entry.group) {
                assert!(
                    !seen_groups.contains(&entry.group),
                    "group {} is split — {} reopens it",
                    entry.group,
                    entry.key
                );
                seen_groups.push(entry.group);
            }
            let here = (entry.group, entry.section);
            if seen_sections.last() != Some(&here) {
                assert!(
                    !seen_sections.contains(&here),
                    "section {:?} of group {} is split — {} reopens it",
                    entry.section,
                    entry.group,
                    entry.key
                );
                seen_sections.push(here);
            }
        }

        assert_eq!(
            seen_groups, groups,
            "the order settings appear in must match the published group order"
        );
    }

    /// The choice lists the catalog offers are the ones the validator accepts.
    ///
    /// Not an agreement test between two lists — they are one list, and this
    /// asserts that sharing it actually took, from the outside, through
    /// `validate`. A regression here means somebody reintroduced a hand-written
    /// allow-list beside the shared slice.
    #[test]
    fn every_offered_choice_validates() {
        for entry in catalog() {
            let Choices::Static { options } = entry.choices else {
                continue;
            };
            let key = SettingKey::parse(&entry.key);
            for option in options {
                let value = Value::from(option.value);
                assert!(
                    key.validate(&value).is_ok(),
                    "{} offers {:?}, which its own validator refuses",
                    entry.key,
                    option.value
                );
            }
        }
    }
}

#[cfg(test)]
mod bundle_tests {
    use super::*;

    /// Every setting key and section heading the settings dialog names by hand
    /// exists in the catalog.
    ///
    /// **The one drift this change did not remove, gated.** The dialog now takes
    /// every title, help string, bound and allowed value off the wire — but four
    /// of its maps are still keyed by Rust-owned *strings*: `OVERRIDES` and
    /// `TRAILING_ROWS` by setting key, `HARDWARE_GATES` by setting key, and
    /// `SECTION_BLURBS` by section heading **prose**. Nothing in TypeScript can
    /// check them, and every failure is silent in the worst direction:
    ///
    /// - a renamed key drops a bespoke control back to the generic one, so
    ///   `terminal.shell` would render as a plain text box;
    /// - a renamed key drops a hardware gate, so a battery-only row appears on a
    ///   desktop;
    /// - a reworded heading drops its explanatory blurb, and the section still
    ///   renders, so nothing looks broken.
    ///
    /// Checked from Rust rather than from a TypeScript fixture because Rust is
    /// the side that owns the strings — a fixture would be a third copy. Same
    /// idiom as `documented_run_history_horizon_matches_the_constants`, which
    /// pins tracked prose against a constant.
    #[test]
    fn every_setting_the_dialog_names_by_hand_exists() {
        let dialog = include_str!("../../../veld-daemon/ui/src/components/SettingsDialog.tsx");
        let entries = catalog();
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        let sections: Vec<&str> = entries.iter().filter_map(|e| e.section).collect();

        for (map, valid, what) in [
            ("OVERRIDES", &keys, "setting"),
            ("TRAILING_ROWS", &keys, "setting"),
            ("HARDWARE_GATES", &keys, "setting"),
            ("SECTION_BLURBS", &sections, "section heading"),
        ] {
            let body = dialog
                .split_once(&format!("const {map}"))
                .unwrap_or_else(|| panic!("{map} is gone from SettingsDialog.tsx — if it was renamed, rename it here too; if it was deleted, delete this arm"))
                .1
                .split_once("\n};")
                .expect("the map is not brace-terminated")
                .0;

            let mut checked = 0usize;
            // `split_once("const {map}")` leaves the rest of the declaration line
            // (`: Record<…> = {`) at the front of the body; the entries start on
            // the next line.
            for line in body.lines().skip(1) {
                let line = line.trim();
                // An entry is `"<key>": <value>,` **or** `<key>: <value>,` — biome
                // drops the quotes when the key is a valid JS identifier, which is
                // why `SECTION_BLURBS`' `Notifying` has none while `"Focus mode"`
                // does. Reading only the quoted form silently skipped a third of
                // that map, so the guard passed while covering two keys of three.
                //
                // The trailing `:` is what distinguishes a key from a continuation
                // line: these values are wrapped prose, and a wrapped line is a
                // bare quoted string with no `:` after its closing quote.
                let named = if let Some(rest) = line.strip_prefix('"') {
                    match rest.split_once('"') {
                        Some((key, after)) if after.trim_start().starts_with(':') => Some(key),
                        _ => None,
                    }
                } else {
                    let ident: String = line
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                        .collect();
                    let rest = &line[ident.len()..];
                    (!ident.is_empty() && rest.trim_start().starts_with(':'))
                        .then(|| &line[..ident.len()])
                };

                let Some(named) = named else {
                    // Fail loudly on a shape this parser does not know, rather
                    // than skipping it — a silently-unparsed entry is exactly how
                    // this tripwire came to cover two keys of three. A computed
                    // key (`[FOO]: …`) or a spread would land here.
                    //
                    // A line that *starts* with a quote and was not parsed above
                    // is a wrapped value, not an entry: these values are prose and
                    // several contain a colon mid-sentence. Excluding them is what
                    // keeps this check about syntax rather than about punctuation.
                    assert!(
                        line.starts_with('"')
                            || line.starts_with("//")
                            || line.starts_with('*')
                            || !line.contains(':'),
                        "{map} in SettingsDialog.tsx has an entry this tripwire cannot parse: \
                         {line:?}. Teach it that shape — do not let it skip one."
                    );
                    continue;
                };
                checked += 1;
                assert!(
                    valid.contains(&named),
                    "SettingsDialog.tsx's {map} names the {what} {named:?}, which \
                     the catalog does not have. Known: {valid:?}"
                );
            }
            // A parser that matches nothing passes every assertion above it.
            assert!(
                checked > 0,
                "{map} was found in SettingsDialog.tsx but no entry was parsed out \
                 of it — the tripwire is checking nothing"
            );
        }
    }
}
