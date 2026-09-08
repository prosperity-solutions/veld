//! The interpreted part of the reserved `ide` config namespace.
//!
//! The key was reserved in schemaVersion 3 — parsed, stored and deliberately not
//! interpreted ([`crate::config::VeldConfig::ide`]). This module gives it its
//! first real meaning — `ide.quicklinks`, `ide.permissions` and
//! `ide.externalOrigins` — while everything
//! else under `ide` stays opaque, so a JSON-defined IDE extension is still free
//! to use whatever key shape it likes. Lint finding F8 narrows accordingly: it
//! now reports the *rest* of `ide` as not-yet-rendered instead of all of it.
//!
//! It was spelled `ui` while it was reserved, and was renamed before anything
//! read it: `/ide` is the route, a per-project IDE surface is what the reserved
//! shape was for, and "UI" could equally have meant the dashboard, the feedback
//! overlay or the CLI's own output. Renaming a top-level key is a breaking
//! change, so it had to happen in the release that gave the key a meaning or not
//! at all.
//!
//! Two rules run through the whole module:
//!
//! - **Lenient, never fatal.** A malformed quicklink or permission rule is a
//!   [`IdeProblem`] that `veld lint` reports and the loader ignores. It must never
//!   become a load error: F0.1 says a config that will not load takes `veld stop`
//!   and `veld logs` down with it, and a typo in a desktop-only convenience field
//!   has no business doing that.
//! - **Fail closed.** An entry that cannot be understood grants nothing. A
//!   permission rule with an unparseable origin, an unknown permission id or a
//!   wrong-typed field is dropped rather than partially applied — the failure mode
//!   of guessing is a capability handed to web content nobody meant to give it.
//!
//! Matching a *permission* request lives in the desktop app
//! (`desktop/src/permissions.js`), because that is where the request arrives. What
//! crosses the wire is therefore *already normalised*: the origin is split into
//! scheme/host/port here, once, so the matcher cannot disagree with what
//! `veld lint` accepted. Matching a *URL being opened* is the other consumer of the
//! same normalised shape and it runs in the daemon ([`route_url`]) — two consumers
//! of one pattern rather than one implementation that could be shared, which is why
//! [`origin_matches`] restates that matcher's rules and is tested on the same
//! cases.

use serde::{Deserialize, Serialize};

/// Every permission id a project config may name.
///
/// These are veld's ids, not Electron's, and the difference is deliberate in one
/// place: Electron's `media` covers camera and microphone together and
/// distinguishes them only inside the request details, while a per-site UI has to
/// show them as the two separate switches every browser shows. The mapping back to
/// Electron's names lives in `desktop/src/permissions.js`, and a test there asserts
/// its table covers exactly this list — read out of the JSON schema, so the two
/// cannot drift apart silently.
pub const PERMISSION_IDS: &[&str] = &[
    "camera",
    "clipboard-read",
    "clipboard-write",
    "display-capture",
    "file-system",
    "fullscreen",
    "geolocation",
    "hid",
    "idle-detection",
    "keyboard-lock",
    "microphone",
    "midi",
    "notifications",
    "open-external",
    "pointer-lock",
    "protected-media",
    "serial",
    "speaker-selection",
    "storage-access",
    "usb",
    "window-management",
];

/// Every icon name a pane or an `ide.extensions` entry may name, in sorted order.
///
/// These are [Tabler](https://tabler.io/icons) names, the icon set every other
/// pane tab already uses, so a config-declared pane sits beside the built-in
/// ones without looking pasted in. An allowlist rather than "any Tabler name"
/// because the UI must import each component statically — a name resolved at
/// runtime would either pull the whole set into the bundle or render nothing.
///
/// A pane may use an emoji instead ([`PaneIcon::Emoji`]), which is unbounded and
/// needs no entry here. The two forms are told apart by ASCII-ness, so this list
/// can only ever grow with ASCII names.
pub const PANE_ICON_NAMES: &[&str] = &[
    "alert-triangle",
    "app-window",
    "atom",
    "ban",
    "bolt",
    "book",
    "brain",
    "brand-docker",
    "brand-github",
    "brand-gitlab",
    "brand-slack",
    "brand-vscode",
    "browser",
    "bug",
    "bulb",
    "chart-line",
    "check",
    "circle-check",
    "circle-dashed",
    "circle-x",
    "clock",
    "cloud",
    "cloud-upload",
    "code",
    "coin",
    "compass",
    "cpu",
    "database",
    "device-desktop",
    "download",
    "external-link",
    "eye",
    "file-code",
    "flag",
    "flask",
    "folder",
    "gauge",
    "git-branch",
    "git-commit",
    "git-merge",
    "git-pull-request",
    "help-circle",
    "history",
    "hourglass",
    "info-circle",
    "key",
    "link",
    "list-check",
    "lock",
    "lock-open",
    "mail",
    "map",
    "message-chatbot",
    "notebook",
    "package",
    "palette",
    "player-pause",
    "player-play",
    "plug",
    "puzzle",
    "refresh",
    "robot",
    "rocket",
    "search",
    "server",
    "shield",
    "shield-check",
    "sparkles",
    "star",
    "tag",
    "terminal",
    "terminal-2",
    "tool",
    "trending-down",
    "trending-up",
    "upload",
    "user",
    "users",
    "wand",
    "x",
];

/// Every illustration a news item may name, in sorted order.
///
/// The same closed set the IDE's own promotion cards draw from (`GlyphName` in
/// `crates/veld-daemon/ui/src/promotions/model.ts`) — line art in `currentColor`,
/// never a raster. Four names against `ide.panes`'s thirty is deliberate: a news
/// card is one sentence with a mark beside it, and the set staying small is what
/// keeps a project's card looking like it belongs beside Veld's rather than
/// pasted in. It is not the pane icon vocabulary and must not grow into it.
///
/// Two gates keep this list, the schema's `enum`, and the bundle's `GLYPH_NAMES`
/// from drifting: [`tests::the_glyph_set_matches_the_published_schema`] here, and
/// `it("matches the published schema's glyph set and every cap")` in `model.test.ts`.
pub const NEWS_GLYPHS: &[&str] = &["device", "inbox", "panes", "terminal"];

/// Every key one `ide.news` entry may declare, in sorted order.
///
/// Mirrors the schema's `$defs.newsItem`, which sets `additionalProperties:
/// false` — so without this an editor red-squiggles `"headlines": "…"` while
/// `veld lint` accepts it and the entry is dropped for a missing `headline` that
/// the author can see right there in the file.
pub const NEWS_ITEM_KEYS: &[&str] = &["body", "eyebrow", "glyph", "headline", "id", "since"];

/// Caps on one news item's copy, in characters.
///
/// The same numbers the IDE bundle enforces on Veld's own cards, and they are
/// **the mechanism, not style advice**: a card is a headline and one sentence, so
/// a project cannot turn the interrupting surface into a wall of prose. An entry
/// that breaches one is dropped with a problem rather than truncated — a
/// half-sentence is worse than an author being told to shorten it.
pub const MAX_NEWS_EYEBROW: usize = 24;
/// See [`MAX_NEWS_EYEBROW`].
pub const MAX_NEWS_HEADLINE: usize = 44;
/// See [`MAX_NEWS_EYEBROW`].
pub const MAX_NEWS_BODY: usize = 160;

/// How many news items one config may have **live at once**.
///
/// The scarcity discipline in `docs/promotions.md`, made a gate. A news channel
/// in a shared config file is a channel every teammate can push a modal through
/// to every other teammate, and the technical mitigations that work are the ones
/// that also improve authoring: the copy caps above stop a wall, and this stops a
/// stack. Retiring a news item is *deleting* it, exactly as for Veld's own cards,
/// so a project that has more than this many things to say at once has a
/// changelog to say them in.
///
/// Over the cap, the entries with the **oldest `since`** are dropped — see the tail
/// of [`parse_news`] for why by date and not by array position.
pub const MAX_NEWS_ITEMS: usize = 5;

/// Every key a `terminal` pane may declare, in sorted order.
///
/// Mirrors the schema's `$defs.pane` terminal branch, which sets
/// `additionalProperties: false` — so without this an editor red-squiggles
/// `"autoresume": true` while `veld lint` accepts it and the pane silently takes
/// the default. Both of the defaults a typo can reach change behaviour
/// (`auto_resume` false, `close_on_exit` true), which is the worst shape for a
/// silent one.
pub const TERMINAL_PANE_KEYS: &[&str] = &[
    "allow_terminal_renaming",
    "argv",
    "auto_resume",
    "close_on_exit",
    "description",
    "icon",
    "id",
    "label",
    "requires_bin",
    "resume",
    "shell",
    "type",
];

/// The `${veld.*}` names a pane command may reference, in sorted order.
///
/// Deliberately **not** part of [`crate::config::BUILTIN_VARS`]: `pane.*` exists
/// only while a pane is being launched, and a node command that referenced it
/// would resolve to nothing. Keeping the two sets separate is what lets
/// `check_builtin_names` keep rejecting `${veld.pane.token}` in a node while this
/// module accepts it — the closed-set rule holds per scope, not globally.
pub const PANE_BUILTINS: &[&str] = &[
    "branch",
    "branch_raw",
    "pane.id",
    "pane.label",
    "pane.token",
    "project",
    "root",
    "username",
    "worktree",
];

/// The `${veld.*}` names an extension command may reference, in sorted order.
///
/// [`PANE_BUILTINS`] minus the `pane.*` family, which exists only while a pane is
/// being launched: an extension command runs against a *worktree*, so
/// `${veld.pane.token}` there would resolve to nothing. Same closed-set rule, one
/// scope narrower — and the reason the check is worth having is that an
/// unresolvable reference is not a soft failure, it is a badge that never renders
/// with an error the author has to read backwards from.
pub const EXTENSION_BUILTINS: &[&str] = &[
    "branch",
    "branch_raw",
    "project",
    "root",
    "username",
    "worktree",
];

/// `${veld.*}` names permitted only in `argv`, refused in `shell`.
///
/// `branch_raw` is the checkout name **unslugified** — the value `branch`
/// exists to avoid interpolating raw, because an outsider chooses it (you check
/// out someone else's pull-request branch). `git check-ref-format --branch`
/// accepts `foo$(id)`, `foo'bar` and `feat/foo` alike: the first two make a
/// `shell` string built from it remote command execution on checkout, and
/// quoting cannot fix it, since `foo'bar` breaks out of a single-quoted
/// substitution too. `argv` has no such hole for those characters, because
/// interpolation runs after the element count is fixed, so `$(...)`, `|` and
/// `&&` are inert text in a single argument.
///
/// A leading `-` is a *different* hole — `argv` has no shell to protect
/// against, but a value starting with `-` is still ordinary text the
/// receiving program's own flag parser can read as an option, and (unlike a
/// `git branch`-created name) a checked-out branch really can start with one:
/// `git switch -- -foo` succeeds and `git worktree list --porcelain` reports
/// it verbatim. That one is closed at the source instead — see
/// `worktree_builtins` in `crates/veld-daemon/src/pty.rs`, which omits
/// `branch_raw` entirely for such a branch, so a command referencing it fails
/// closed with an unresolved-variable error rather than handing a flag to
/// whatever it runs. `branch` never has this problem: `url::slugify` never
/// starts or ends with `-`.
const SHELL_REFUSED_BUILTINS: &[&str] = &["branch_raw"];

/// Every named place an extension may contribute to, in sorted order.
///
/// The slot set is a *contract*: it is what `ide.extensions` entries are
/// validated against, and what the UI implements. One entry today — adding the
/// next one is a string here plus a render site, which is the whole reason
/// `slot` is a field on the item instead of a level of config structure.
pub const EXTENSION_SLOTS: &[&str] = &["topBar"];

/// Which side of a slot an extension sits on.
pub const EXTENSION_ALIGNS: &[&str] = &["end", "start"];

/// What an extension looks like when its `requires_bin` is not installed.
pub const EXTENSION_WHEN_MISSING: &[&str] = &["disable", "hide", "hint"];

/// Where a status extension's `href` opens.
pub const EXTENSION_OPEN_IN: &[&str] = &["pane", "system"];

/// How a status extension's badge renders: a label, or its glyph alone.
pub const EXTENSION_DISPLAY: &[&str] = &["icon", "text"];

/// Keys every extension may declare, whatever its type, in sorted order.
pub const EXTENSION_COMMON_KEYS: &[&str] = &[
    "align",
    "description",
    "hint",
    "icon",
    "id",
    "label",
    "requires_bin",
    "slot",
    "type",
    "when_missing",
];

/// Extra keys a `status` extension may declare, in sorted order.
pub const STATUS_EXTENSION_KEYS: &[&str] =
    &["argv", "display", "open_in", "refresh_seconds", "shell"];

/// Extra keys an `action` extension may declare, in sorted order.
pub const ACTION_EXTENSION_KEYS: &[&str] = &["argv", "shell"];

/// Extra keys a `menu` extension may declare, in sorted order.
pub const MENU_EXTENSION_KEYS: &[&str] = &["items"];

/// How many extensions one project may declare.
///
/// A cost bound owned by veld rather than by a file in somebody's repo — the
/// same reasoning as `PRESETS_EXPANDED_PER_LISTING`. Every `status` extension
/// is a child process the daemon runs on a timer, so without a cap the load a
/// worktree puts on the machine is set by whoever last edited its config.
pub const MAX_EXTENSIONS_PER_PROJECT: usize = 24;

/// The floor on `refresh_seconds`, for the same reason as the count cap.
pub const MIN_EXTENSION_REFRESH_SECONDS: u64 = 15;

/// What a `status` extension refreshes at when it does not say.
pub const DEFAULT_EXTENSION_REFRESH_SECONDS: u64 = 60;

/// The interpreted `ide` section, plus whatever could not be interpreted.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdeSection {
    /// Project links that are *not* veld's — staging, a dashboard, a wiki — shown
    /// beside the run's own URLs on a browser pane's start page.
    #[serde(default)]
    pub quicklinks: Vec<Quicklink>,
    /// Permissions the repo pre-answers for web content in a browser pane.
    #[serde(default)]
    pub permissions: Vec<PermissionRule>,
    /// Extra pane types this project offers in the desktop app's pane menu.
    #[serde(default)]
    pub panes: Vec<PaneDef>,
    /// News this project tells its own team, in author order, capped at
    /// [`MAX_NEWS_ITEMS`].
    ///
    /// The repo's half of the IDE's promotion channel: a maintainer merges a card
    /// with the change it describes, a teammate pulls, and the teammate is told.
    /// Everything about *delivery* — whether a card has been read, whether its
    /// date predates this user, what the unread count is — lives in the bundle
    /// against opaque ids, exactly as for Veld's own cards. What lives here is
    /// only the parse and the caps, because this is the process that reads
    /// `veld.json` and therefore the only one that can tell an author their
    /// headline ran long.
    #[serde(default)]
    pub news: Vec<NewsItem>,
    /// Badges, buttons and menus this project contributes to named places in the
    /// IDE chrome. See [`Extension`].
    #[serde(default)]
    pub extensions: Vec<Extension>,
    /// Origins that must open in the **system** browser rather than in a Veld
    /// browser pane — the project's half of the exempt list (the other half is the
    /// `browser.externalOrigins` setting, and the two are unioned).
    ///
    /// It exists because a pane is not the browser the user is logged into. An SSO
    /// or bank flow in a fresh partition is a second login at best and a dead end
    /// at worst, and the project is the only place that knows which hosts those
    /// are for the app being worked on.
    #[serde(default)]
    pub external_origins: Vec<OriginPattern>,
    /// How sensitively the IDE's worktree-staleness indicator is coloured
    /// (`ide.git.stalenessSensitivity`).
    ///
    /// A multiplier on the "update main" count pill's severity curve. `1` is the
    /// baseline — a single commit a week old, or fifty commits in a day, are both
    /// at the top of the scale (red); `2` halves both thresholds (a 3.5-day-old
    /// commit or 25 commits read red); `0.5` halves the sensitivity. Clamped to
    /// `[0.1, 10]`. A project that lives on a fast-moving trunk tunes it up; a
    /// project whose worktrees naturally drift tunes it down. Lives under the
    /// `ide.git` subscope so other per-project git knobs (create-from-origin, …)
    /// have a home rather than each squatting at the top of `ide`.
    #[serde(default = "default_staleness_sensitivity")]
    pub staleness_sensitivity: f64,
    /// Top-level keys under `ide` that this version still does not interpret, in
    /// sorted order. F8 names them so an author can tell "reserved" from "typo".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uninterpreted: Vec<String>,
    /// Everything wrong with the section. Reported by `veld lint` as warnings;
    /// each one corresponds to an entry that was dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<IdeProblem>,
}

impl IdeSection {
    /// True when nothing here is worth sending to a UI.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.quicklinks.is_empty()
            && self.permissions.is_empty()
            && self.external_origins.is_empty()
            && self.panes.is_empty()
            && self.news.is_empty()
            && self.extensions.is_empty()
    }

    /// The staleness-sensitivity multiplier, floored at `0.1`. Always at least
    /// that much so a zero or negative value from a hand-written struct can never
    /// divide by zero or invert the curve downstream.
    #[must_use]
    pub fn staleness_sensitivity_safe(&self) -> f64 {
        self.staleness_sensitivity.max(0.1)
    }

    /// The pane this id names, if the project declares one.
    #[must_use]
    pub fn pane(&self, id: &str) -> Option<&PaneDef> {
        self.panes.iter().find(|p| p.id == id)
    }

    /// The extension this id names, if the project declares one.
    ///
    /// The one lookup every execution path goes through: the client names an
    /// extension and this is what turns that name into the command the *config*
    /// declares. Nothing may execute a command that arrived from anywhere else —
    /// see [`Extension`].
    #[must_use]
    pub fn extension(&self, id: &str) -> Option<&Extension> {
        self.extensions.iter().find(|e| e.id == id)
    }

    /// The extensions rendered in `slot`, in declaration order.
    ///
    /// Skips the ones with no slot: those are declared to be *referenced* (from a
    /// menu, or from a status extension's output) and rendering them as well would
    /// put an "Open in WebStorm" button beside the "Open in" menu that contains it.
    #[must_use]
    pub fn extensions_in_slot(&self, slot: &str) -> Vec<&Extension> {
        self.extensions
            .iter()
            .filter(|e| e.slot.as_deref() == Some(slot))
            .collect()
    }
}

/// One badge, button or menu a project contributes to the IDE chrome.
///
/// The fields here are the ones every extension type needs whatever it renders —
/// an identity, where it goes, how to label it, and whether the machine can run it
/// at all. What the extension *does* lives in [`Extension::body`], keyed by the
/// `type` discriminator, exactly as [`PaneDef`] splits from [`PaneBody`].
///
/// **The command is only ever read from here.** A client — and, for a status
/// extension, the *output of a previous run* — names an extension by id; the
/// daemon looks that id up in the project's on-disk config and runs what the
/// config declares. This is the boundary `run_action` and `resolve_pane` already
/// hold, extended one step: a runtime value may choose *which* declared extension
/// is offered, and may never contribute one. Relax that and a badge's stdout
/// becomes a command-injection surface with nothing to validate it against,
/// because there is no declaration to compare it to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Extension {
    /// Stable id, unique within the project's extensions. Names the extension on
    /// the wire, in a menu's `items`, and in a status extension's `actions`.
    pub id: String,
    /// The text or tooltip the user sees. Defaults to `id`.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PaneIcon>,
    /// The named place this renders in, one of [`EXTENSION_SLOTS`].
    ///
    /// `None` means **declared but not rendered**: the extension exists only to be
    /// referenced by a `menu`'s `items` or by a status extension's `actions`. That
    /// is what lets a project declare five editor actions without putting five
    /// buttons in a 42px bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// Which side of the slot it sits on.
    #[serde(default)]
    pub align: ExtensionAlign,
    /// Executables that must be on `PATH`. Empty means "always available".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_bin: Vec<String>,
    /// What an unavailable extension looks like.
    #[serde(default)]
    pub when_missing: WhenMissing,
    /// What to tell the user when [`Self::when_missing`] is [`WhenMissing::Hint`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<ExtensionHint>,
    pub body: ExtensionBody,
}

impl Extension {
    /// The command this extension runs, or `None` for a type that runs nothing.
    #[must_use]
    pub fn command(&self) -> Option<&crate::config::CommandSpec> {
        match &self.body {
            ExtensionBody::Status(s) => Some(&s.command),
            ExtensionBody::Action(a) => Some(&a.command),
            ExtensionBody::Menu(_) => None,
        }
    }

    /// The `type` string this extension was declared with.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match &self.body {
            ExtensionBody::Status(_) => "status",
            ExtensionBody::Action(_) => "action",
            ExtensionBody::Menu(_) => "menu",
        }
    }
}

/// What an extension is. The discriminator carries the whole shape difference, so
/// a future `link` or `script` type is additive rather than a reshape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionBody {
    Status(StatusExtension),
    Action(ActionExtension),
    Menu(MenuExtension),
}

/// A badge: a command whose stdout is rendered, re-run on a timer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusExtension {
    pub command: crate::config::CommandSpec,
    /// How often the badge is re-evaluated, floored at
    /// [`MIN_EXTENSION_REFRESH_SECONDS`].
    #[serde(default = "default_refresh_seconds")]
    pub refresh_seconds: u64,
    /// Where the badge's `href` opens.
    #[serde(default)]
    pub open_in: OpenIn,
    /// Whether the badge renders as a label (the default) or as its glyph
    /// alone. The badge's own output may override this per value, the same as
    /// `open_in`. Falls back to a label when the badge would otherwise have no
    /// glyph to render — see [`BadgeDisplay`].
    #[serde(default)]
    pub display: BadgeDisplay,
}

/// A button: a command run on a click, with no output contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionExtension {
    pub command: crate::config::CommandSpec,
}

/// A group: one control in the slot whose members appear in a popover.
///
/// `items` are ids of declared `action` extensions, not nested objects. Nesting is
/// one level deep on purpose — two levels of popover in a 42px bar is worse than a
/// second menu — and referencing rather than nesting means a menu member is a
/// first-class declaration that `veld lint` checks for a duplicate id like any
/// other.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MenuExtension {
    pub items: Vec<String>,
}

/// Which side of a slot an extension sits on.
///
/// `Start` by default because the top bar's own convention is *left is what this
/// project does, right is what the app does*, and an extension is the project's.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionAlign {
    #[default]
    Start,
    End,
}

/// What an extension whose `requires_bin` is missing looks like.
///
/// `Hint` is the default because an extension is the project telling a newcomer
/// what it expects them to have installed, and silence teaches nothing. An
/// explicit value here also **beats the global `ui.hideDisabledActions`
/// setting**: that preference is about hiding inapplicable *core* actions, and it
/// must not delete a project's setup instructions.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WhenMissing {
    Hide,
    Disable,
    #[default]
    Hint,
}

/// Where a status extension's `href` opens.
///
/// `System` by default, which is the opposite of how [`Quicklink`] behaves, and
/// deliberately: a quicklink points at localhost or staging, while an extension's
/// href points at a *provider's* authenticated surface — a pull request, a CI run,
/// a dashboard — where the user is already signed in and a browser pane's separate
/// partition is a second login at best.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OpenIn {
    #[default]
    System,
    Pane,
}

/// How a status extension's badge renders: a label, or its glyph alone.
///
/// `Text` (the default) keeps today's rendering — the label, with a glyph
/// beside it if one is set. `Icon` renders the glyph as the whole badge, with
/// the label kept as the button's accessible name (a real `<button>` with no
/// visible text needs one) and as the tooltip's fallback, rather than shown.
/// Named `BadgeDisplay` rather than `Display` so it does not collide with
/// `std::fmt::Display` at a call site that imports both.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BadgeDisplay {
    #[default]
    Text,
    Icon,
}

/// What to show for an extension whose tool is not installed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionHint {
    pub text: String,
    /// Where to send someone who wants to fix it — an install page. `http(s)` only,
    /// for the reason [`Quicklink`] is restricted the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

fn default_refresh_seconds() -> u64 {
    DEFAULT_EXTENSION_REFRESH_SECONDS
}

/// A pane type a project adds to the desktop app's pane menu.
///
/// The fields here are the ones every pane type needs whatever it renders — an
/// identity, how to label it, and whether the machine can run it at all. What
/// the pane *is* lives in [`PaneDef::body`], keyed by the `type` discriminator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaneDef {
    /// Stable id, unique within the project. Names the pane on the wire and in
    /// `${veld.pane.id}`; never shown to the user, which is what `label` is for.
    pub id: String,
    /// Menu text. Defaults to `id` when the author omits it.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PaneIcon>,
    /// Executables that must be on `PATH` for this pane to be offered. Empty
    /// means "always offered".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_bin: Vec<String>,
    pub body: PaneBody,
}

/// What a pane actually is. One variant today; the discriminator exists so the
/// next one is additive rather than a reshape.
///
/// The names match the runtime pane kinds the UI already has
/// (`ui/src/panes/model.ts`), so a future `type: "browser"` means there exactly
/// what it means here. The runtime set is the larger one — `nodes` and `new` are
/// veld's own panes and will never be config-declarable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneBody {
    Terminal(TerminalPane),
}

/// A pane that runs a command in a terminal instead of a login shell.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalPane {
    /// What a fresh pane runs.
    pub launch: crate::config::CommandSpec,
    /// What a pane whose shell has died runs instead, to pick up where the tool
    /// left off. Absent means the pane can only ever start fresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<crate::config::CommandSpec>,
    /// Whether veld may run `resume` without being asked. Only ever consulted
    /// when a pane is restored with its shell already gone, never while the user
    /// is watching it — see `crates/veld-daemon/ui/src/panes/terminalHost.ts`.
    ///
    /// Defaults to false, and stays false when `resume` is absent: these
    /// commands launch coding agents, so an unattended one spends money and runs
    /// tools with nobody watching.
    #[serde(default)]
    pub auto_resume: bool,
    /// Whether the pane closes itself when its command exits **cleanly**.
    ///
    /// Defaults to true, which is what a terminal emulator does and what
    /// quitting the tool means: you are done with the pane. It can only fire on
    /// an exit somebody was there to see — a reboot, a crashed daemon, a quit
    /// app and a reaped session all leave the pane closed or restored from the
    /// layout instead, so this never competes with [`Self::auto_resume`].
    ///
    /// **A non-zero exit never closes the pane**, whatever this says. The reason
    /// a tool died is printed on the screen it dies on, and a pane that
    /// disappears with it is the oldest complaint about terminal emulators.
    #[serde(default = "default_true")]
    pub close_on_exit: bool,
    /// Whether the process in the pane may rename its own tab with the terminal
    /// title it sets (OSC 0/2), instead of the pane's configured `label`.
    ///
    /// A config-declared pane's `label` is intentional — it is how the user
    /// navigates a rail full of agent panes — so a tool like Claude Code that
    /// sets a dynamic title is kept off the label by default. Plain terminals
    /// (a login shell, not a pane) always adopt their OSC title. This flag is
    /// the opt-in for a pane whose own title is more useful than its fixed one.
    ///
    /// The stored `title` is only ever a *display* override: the pane's
    /// identity on the wire and in `${veld.pane.id}` stays its `id`, and the
    /// config `label` is what a fresh pane is born with.
    #[serde(default)]
    pub allow_terminal_renaming: bool,
}

fn default_true() -> bool {
    true
}

/// How a pane's tab is illustrated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PaneIcon {
    /// One of [`PANE_ICON_NAMES`].
    Name(String),
    /// Any non-ASCII string — in practice an emoji, rendered as text.
    Emoji(String),
}

/// One card a project shows its own team.
///
/// Deliberately the *same* closed field set as a Veld promotion, not a superset:
/// no `layout`, no `variant`, no CTA, no link, no Markdown. The cap is the
/// mechanism — four short strings and a mark cannot become a wall of headings,
/// which is the only thing that keeps an interrupting surface worth interrupting
/// for. A `details` pointer to a repo-relative Markdown file is the designed
/// extension point if somebody genuinely hits the limit; it is not this version,
/// and if it is ever built it wants a strict subset renderer rather than a
/// Markdown library, no HTML passthrough, no images, and `https:` links shown as
/// their literal URL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewsItem {
    /// The author's own slug, kebab-case, stable forever.
    ///
    /// **Not** what is persisted: the bundle namespaces this per project before
    /// it reaches storage, so two unrelated repos both shipping `new-build` stay
    /// separate. It still must never be renamed or reused within one project, for
    /// the same two silent failures Veld's own ids have — a rename re-shows the
    /// card to the whole team, and reusing a retired slug suppresses the new card
    /// for everyone who saw the old one.
    pub id: String,
    /// The day this item was written, `YYYY-MM-DD`.
    ///
    /// **Required, with no default, and it gates.** A teammate who imported the
    /// project after this day never sees the card — which is what stops cloning a
    /// repo with a year of history from being a stack of modals about changes that
    /// predate you, and what makes an item that nobody deleted stop reaching
    /// people anyway. There is deliberately no fallback: a defaulted date would
    /// gate wrongly and silently, and "today" is wrong the moment the config is
    /// read on a different day than it was written.
    pub since: String,
    pub eyebrow: String,
    pub headline: String,
    /// One sentence. Not two.
    pub body: String,
    /// One of [`NEWS_GLYPHS`]. Defaults to `inbox`.
    pub glyph: String,
}

/// A link on the browser pane's start page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Quicklink {
    pub label: String,
    pub url: String,
}

/// One pre-answer: an origin, and what it may and may not do.
///
/// `deny` exists so a project can *withdraw* something veld would otherwise allow
/// by default (screen capture of the pane's own contents is the one such default),
/// and it wins over `allow` in the same rule for the usual fail-closed reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRule {
    pub origin: OriginPattern,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// A normalised origin, split once here so the matcher never re-parses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OriginPattern {
    /// What the author wrote, for display in the permission UI and in findings.
    pub raw: String,
    /// `http` or `https`, lowercased.
    pub scheme: String,
    /// The host, lowercased — or, when [`Self::wildcard`] is set, the suffix it
    /// must end under, with the leading `*.` removed.
    pub host: String,
    /// Whether the author wrote a leading `*.`, meaning "any subdomain of this".
    ///
    /// It exists because veld's own URLs put the **run name in the hostname**
    /// (`{service}.{run}.{project}.localhost` by default) and run names come from
    /// the worktree folder, the branch, or `--name` — so a project granting a
    /// permission to its own dev server has no fixed host to write. `*` matches
    /// one *or more* labels for the same reason: `website.<run>.veld.localhost`
    /// is two labels deep, so single-label semantics would not reach it.
    ///
    /// Matching is label-wise (`host.ends_with(".{suffix}")`), never a bare string
    /// suffix, so `evilveld.localhost` does not match `*.veld.localhost`. And the
    /// suffix must be more than one label — `*.com` is refused — because a
    /// wildcard over a whole TLD is never what anyone meant. `*.localhost` is the
    /// deliberate exception: RFC 6761 pins it to loopback, so it cannot name
    /// anything on the network.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wildcard: bool,
    /// The port, or `None` for "any port" — which is what a literal `*` in the
    /// port position means. An *omitted* port is not "any": it resolves to the
    /// scheme's default here, so `http://example.com` means port 80 exactly, the
    /// way an origin means it everywhere else. Dev servers move between ports, so
    /// `http://localhost:*` is the form worth knowing.
    pub port: Option<u16>,
}

/// Whether an origin pattern names only this machine.
///
/// Loopback and `.localhost` (RFC 6761 — resolvers must not send it to the
/// network) are the origins a project can grant to without reaching past the
/// developer's own machine. Everything else is a *remote* server, and a grant to
/// one is a standing capability for third-party JavaScript on the machine of
/// anyone who opens that repo in a pane.
#[must_use]
pub fn is_local_origin(origin: &OriginPattern) -> bool {
    let host = origin.host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Something in `ide` that was dropped, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdeProblem {
    /// A dotted path such as `ide.permissions[1].origin`.
    pub location: String,
    pub message: String,
}

/// Interpret the stored `ide` value.
///
/// Takes the raw [`serde_json::Value`] rather than a typed field because the raw
/// value stays the single source of truth: `ide` is round-tripped verbatim by the
/// loader, and the config's opaque-`ide` exemption in the v3 legacy-`command` check
/// depends on nothing here reshaping it.
/// The baseline sensitivity for the staleness indicator: `1`.
fn default_staleness_sensitivity() -> f64 {
    1.0
}

/// Parse `ide.git` — a per-project subscope for git-related IDE knobs. Only
/// `stalenessSensitivity` is read today; other keys under `git` are reserved
/// and parked in [`IdeSection::uninterpreted`] (as `git.<key>`) so `veld lint`
/// can still tell a reserved key from a typo.
fn parse_git(value: &serde_json::Value, out: &mut IdeSection) {
    let Some(map) = value.as_object() else {
        out.problems.push(IdeProblem {
            location: "ide.git".to_owned(),
            message: "must be an object; it was ignored".to_owned(),
        });
        return;
    };
    for (key, child) in map {
        match key.as_str() {
            "stalenessSensitivity" => parse_staleness_sensitivity(child, out),
            other => out.uninterpreted.push(format!("git.{other}")),
        }
    }
}

/// Parse `ide.git.stalenessSensitivity` — a non-negative multiplier, clamped to
/// `[0.1, 10]`. Lenient like every other `ide` field: an unparseable value
/// reports a problem and keeps the default, never a load error.
fn parse_staleness_sensitivity(value: &serde_json::Value, out: &mut IdeSection) {
    let n = value.as_f64().or_else(|| value.as_i64().map(|i| i as f64));
    match n {
        // Any finite number from 0 up is clamped into [0.1, 10] — so a `0`
        // means "minimum", matching the schema's `minimum: 0.1`, rather than a
        // divide-by-zero. A negative value or a non-number is a problem and
        // keeps the default.
        Some(n) if n.is_finite() && n >= 0.0 => {
            out.staleness_sensitivity = n.clamp(0.1, 10.0);
        }
        _ => out.problems.push(IdeProblem {
            location: "ide.git.stalenessSensitivity".to_owned(),
            message: "must be a non-negative number; the default (1) is used".to_owned(),
        }),
    }
}

pub fn parse(value: Option<&serde_json::Value>) -> IdeSection {
    // Rust's `Default` for `f64` is `0.0`, but the *effective* default for the
    // sensitivity knob is `1.0` (the baseline). Set it here so an absent key —
    // the common case — lands on the baseline rather than on the floor that
    // `staleness_sensitivity_safe` would then clamp to.
    let mut section = IdeSection {
        staleness_sensitivity: 1.0,
        ..IdeSection::default()
    };
    let Some(value) = value else {
        return section;
    };
    let Some(map) = value.as_object() else {
        section.problems.push(IdeProblem {
            location: "ide".to_owned(),
            message: "`ide` must be an object; the whole section was ignored".to_owned(),
        });
        return section;
    };

    for (key, child) in map {
        match key.as_str() {
            "news" => parse_news(child, &mut section),
            "quicklinks" => parse_quicklinks(child, &mut section),
            "permissions" => parse_permissions(child, &mut section),
            "panes" => parse_panes(child, &mut section),
            "extensions" => parse_extensions(child, &mut section),
            "externalOrigins" => parse_external_origins(child, &mut section),
            "git" => parse_git(child, &mut section),
            other => section.uninterpreted.push(other.to_owned()),
        }
    }
    section.uninterpreted.sort();
    section
}

fn parse_panes(value: &serde_json::Value, out: &mut IdeSection) {
    let Some(items) = value.as_array() else {
        out.problems.push(IdeProblem {
            location: "ide.panes".to_owned(),
            message: "must be an array of pane objects; it was ignored".to_owned(),
        });
        return;
    };
    for (index, item) in items.iter().enumerate() {
        let at = format!("ide.panes[{index}]");
        if let Some(pane) = parse_pane(item, &at, out) {
            if out.panes.iter().any(|p| p.id == pane.id) {
                out.problems.push(IdeProblem {
                    location: format!("{at}.id"),
                    message: format!(
                        "duplicate pane id {:?} — the first one wins and this entry was dropped",
                        pane.id
                    ),
                });
                continue;
            }
            out.panes.push(pane);
        }
    }
}

fn parse_extensions(value: &serde_json::Value, out: &mut IdeSection) {
    let Some(items) = value.as_array() else {
        out.problems.push(IdeProblem {
            location: "ide.extensions".to_owned(),
            message: "must be an array of extension objects; it was ignored".to_owned(),
        });
        return;
    };
    for (index, item) in items.iter().enumerate() {
        let at = format!("ide.extensions[{index}]");
        // The cap is applied to *accepted* entries, and before parsing the next
        // one, so a config with 30 declarations still gets its first 24 rather
        // than none. Reported once, not 6 times.
        if out.extensions.len() >= MAX_EXTENSIONS_PER_PROJECT {
            out.problems.push(IdeProblem {
                location: "ide.extensions".to_owned(),
                message: format!(
                    "declares more than {MAX_EXTENSIONS_PER_PROJECT} extensions; the ones after \
                     the first {MAX_EXTENSIONS_PER_PROJECT} were dropped"
                ),
            });
            return;
        }
        if let Some(ext) = parse_extension(item, &at, out) {
            if out.extensions.iter().any(|e| e.id == ext.id) {
                out.problems.push(IdeProblem {
                    location: format!("{at}.id"),
                    message: format!(
                        "duplicate extension id {:?} — the first one wins and this entry was \
                         dropped",
                        ext.id
                    ),
                });
                continue;
            }
            out.extensions.push(ext);
        }
    }
    check_extension_references(out);
}

/// Resolve every `menu` member against the declarations, after all of them exist.
///
/// A cross-item check, so it cannot run inside [`parse_extension`] — a menu is
/// allowed to reference a member declared below it. Fail closed: an unresolvable
/// member is dropped from the menu rather than rendered as a control that does
/// nothing when clicked. A menu left with no members at all is dropped entirely,
/// because an empty popover is worse than an absent one.
fn check_extension_references(out: &mut IdeSection) {
    let declared: Vec<(String, &'static str)> = out
        .extensions
        .iter()
        .map(|e| (e.id.clone(), e.kind()))
        .collect();

    let mut problems = Vec::new();
    let mut drop_ids = Vec::new();
    for ext in &mut out.extensions {
        let ExtensionBody::Menu(menu) = &mut ext.body else {
            continue;
        };
        let at = format!("ide.extensions[{}]", ext.id);
        menu.items.retain(|item| {
            match declared.iter().find(|(id, _)| id == item) {
                None => {
                    problems.push(IdeProblem {
                        location: format!("{at}.items"),
                        message: format!(
                            "references {item:?}, which no extension in this project declares; \
                             the entry was dropped"
                        ),
                    });
                    false
                }
                // A menu of menus is the two-levels-of-popover shape this type
                // refuses, and a menu member that is a badge has no click to run.
                Some((_, kind)) if *kind != "action" => {
                    problems.push(IdeProblem {
                        location: format!("{at}.items"),
                        message: format!(
                            "references {item:?}, which is a {kind:?} extension — a menu's items \
                             must be `action` extensions; the entry was dropped"
                        ),
                    });
                    false
                }
                Some(_) => true,
            }
        });
        if menu.items.is_empty() {
            problems.push(IdeProblem {
                location: at,
                message: "is a menu with no usable items, so it was dropped".to_owned(),
            });
            drop_ids.push(ext.id.clone());
        }
    }
    out.extensions.retain(|e| !drop_ids.contains(&e.id));
    out.problems.extend(problems);
}

fn parse_extension(item: &serde_json::Value, at: &str, out: &mut IdeSection) -> Option<Extension> {
    let entry = item.as_object().or_else(|| {
        out.problems.push(IdeProblem {
            location: at.to_owned(),
            message: "must be an object with an `id` and a `type`".to_owned(),
        });
        None
    })?;

    // `id` and `type` first, for the reason `parse_pane` reads them first: a typo
    // in one is reported against the entry rather than as a pile of downstream
    // problems about fields whose meaning depends on the type.
    let Some(id) = entry
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
    else {
        out.problems.push(IdeProblem {
            location: format!("{at}.id"),
            message: "is required and must be a string".to_owned(),
        });
        return None;
    };
    if !valid_pane_id(id) {
        out.problems.push(IdeProblem {
            location: format!("{at}.id"),
            message: format!("must be 1-64 characters of letters, digits, `-` or `_` (got {id:?})"),
        });
        return None;
    }
    let Some(kind) = entry.get("type").and_then(serde_json::Value::as_str) else {
        out.problems.push(IdeProblem {
            location: format!("{at}.type"),
            message: "is required — this version renders: status, action, menu".to_owned(),
        });
        return None;
    };

    let label = match entry.get("label") {
        None => id.to_owned(),
        Some(v) => match v.as_str().map(str::trim) {
            Some(text) if !text.is_empty() => text.to_owned(),
            _ => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.label"),
                    message: "must be a non-empty string".to_owned(),
                });
                return None;
            }
        },
    };
    let description = match entry.get("description") {
        None => None,
        Some(v) => match v.as_str().map(str::trim) {
            Some(text) if !text.is_empty() => Some(text.to_owned()),
            _ => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.description"),
                    message: "must be a non-empty string".to_owned(),
                });
                return None;
            }
        },
    };
    let icon = match entry.get("icon") {
        None => None,
        Some(v) => Some(parse_pane_icon(v, at, out)?),
    };
    let requires_bin = parse_requires_bin(entry.get("requires_bin"), at, out)?;

    let slot = match entry.get("slot") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let Some(text) = v.as_str().map(str::trim).filter(|t| !t.is_empty()) else {
                out.problems.push(IdeProblem {
                    location: format!("{at}.slot"),
                    message: format!(
                        "must be one of: {}, or absent for an extension that is only referenced \
                         from a menu or a status output",
                        EXTENSION_SLOTS.join(", ")
                    ),
                });
                return None;
            };
            if !EXTENSION_SLOTS.contains(&text) {
                // Not a typo report: a project may legitimately be written for a
                // newer veld with more slots. Naming the ones this version has is
                // the useful half.
                out.problems.push(IdeProblem {
                    location: format!("{at}.slot"),
                    message: format!(
                        "slot {text:?} is not one this version of veld renders (it knows: {}). \
                         The extension was skipped; the rest of `ide.extensions` still applies",
                        EXTENSION_SLOTS.join(", ")
                    ),
                });
                return None;
            }
            Some(text.to_owned())
        }
    };

    let align = match entry.get("align") {
        None => ExtensionAlign::Start,
        Some(v) => match v.as_str().map(str::trim) {
            Some("start") => ExtensionAlign::Start,
            Some("end") => ExtensionAlign::End,
            _ => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.align"),
                    message: format!("must be one of: {}", EXTENSION_ALIGNS.join(", ")),
                });
                return None;
            }
        },
    };

    let when_missing = match entry.get("when_missing") {
        None => WhenMissing::default(),
        Some(v) => match v.as_str().map(str::trim) {
            Some("hide") => WhenMissing::Hide,
            Some("disable") => WhenMissing::Disable,
            Some("hint") => WhenMissing::Hint,
            _ => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.when_missing"),
                    message: format!("must be one of: {}", EXTENSION_WHEN_MISSING.join(", ")),
                });
                return None;
            }
        },
    };
    let hint = parse_extension_hint(entry.get("hint"), at, out)?;

    let body = match kind {
        "status" => ExtensionBody::Status(parse_status_extension(entry, at, out)?),
        "action" => ExtensionBody::Action(ActionExtension {
            command: parse_extension_command(entry, at, out)?,
        }),
        "menu" => ExtensionBody::Menu(parse_menu_extension(entry, at, out)?),
        other => {
            out.problems.push(IdeProblem {
                location: format!("{at}.type"),
                message: format!(
                    "extension type {other:?} is not one this version of veld renders (it knows: \
                     status, action, menu). The extension was skipped; the rest of \
                     `ide.extensions` still applies"
                ),
            });
            return None;
        }
    };

    // Only an `action` can be referenced, so anything else without a slot would be
    // declared and unreachable — silently, which is the shape worth reporting.
    if slot.is_none() && !matches!(body, ExtensionBody::Action(_)) {
        out.problems.push(IdeProblem {
            location: format!("{at}.slot"),
            message: format!(
                "is required for a {kind:?} extension — only an `action` may omit it, to be \
                 referenced from a menu or a status output"
            ),
        });
        return None;
    }

    // After the body, so an extension being dropped for a real reason does not
    // also collect a pile of key complaints.
    let allowed = extension_keys(kind);
    let mut unknown: Vec<&str> = entry
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        out.problems.push(IdeProblem {
            location: at.to_owned(),
            message: format!(
                "unknown key(s) {} for a {kind:?} extension. It may declare: {}",
                unknown
                    .iter()
                    .map(|k| format!("{k:?}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                allowed.join(", ")
            ),
        });
    }

    Some(Extension {
        id: id.to_owned(),
        label,
        description,
        icon,
        slot,
        align,
        requires_bin,
        when_missing,
        hint,
        body,
    })
}

fn parse_extension_command(
    entry: &serde_json::Map<String, serde_json::Value>,
    location: &str,
    out: &mut IdeSection,
) -> Option<crate::config::CommandSpec> {
    parse_command_in_scope(entry, location, "extension", EXTENSION_BUILTINS, out)
}

/// Every key this extension type may declare, sorted — the union of the common set
/// and the type's own. Built rather than listed per type so a new common key cannot
/// be added to one type's list and forgotten in the others.
fn extension_keys(kind: &str) -> Vec<&'static str> {
    let extra = match kind {
        "status" => STATUS_EXTENSION_KEYS,
        "action" => ACTION_EXTENSION_KEYS,
        "menu" => MENU_EXTENSION_KEYS,
        _ => &[],
    };
    let mut keys: Vec<&'static str> = EXTENSION_COMMON_KEYS
        .iter()
        .chain(extra.iter())
        .copied()
        .collect();
    keys.sort_unstable();
    keys
}

fn parse_status_extension(
    entry: &serde_json::Map<String, serde_json::Value>,
    at: &str,
    out: &mut IdeSection,
) -> Option<StatusExtension> {
    let command = parse_extension_command(entry, at, out)?;

    let refresh_seconds = match entry.get("refresh_seconds") {
        None => DEFAULT_EXTENSION_REFRESH_SECONDS,
        Some(v) => {
            let Some(n) = v.as_u64() else {
                out.problems.push(IdeProblem {
                    location: format!("{at}.refresh_seconds"),
                    message: format!(
                        "must be a whole number of seconds, at least \
                         {MIN_EXTENSION_REFRESH_SECONDS}"
                    ),
                });
                return None;
            };
            // Clamped rather than refused: the author's intent ("refresh often") is
            // clear and honouring it as far as veld allows is more useful than
            // dropping the badge. Reported so the effective value is never a
            // surprise.
            if n < MIN_EXTENSION_REFRESH_SECONDS {
                out.problems.push(IdeProblem {
                    location: format!("{at}.refresh_seconds"),
                    message: format!(
                        "{n} is below the minimum of {MIN_EXTENSION_REFRESH_SECONDS}s, which is \
                         what was used"
                    ),
                });
                MIN_EXTENSION_REFRESH_SECONDS
            } else {
                n
            }
        }
    };

    let open_in = match entry.get("open_in") {
        None => OpenIn::default(),
        Some(v) => match v.as_str().map(str::trim) {
            Some("system") => OpenIn::System,
            Some("pane") => OpenIn::Pane,
            _ => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.open_in"),
                    message: format!("must be one of: {}", EXTENSION_OPEN_IN.join(", ")),
                });
                return None;
            }
        },
    };

    let display = match entry.get("display") {
        None => BadgeDisplay::default(),
        Some(v) => match v.as_str().map(str::trim) {
            Some("text") => BadgeDisplay::Text,
            Some("icon") => BadgeDisplay::Icon,
            _ => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.display"),
                    message: format!("must be one of: {}", EXTENSION_DISPLAY.join(", ")),
                });
                return None;
            }
        },
    };

    Some(StatusExtension {
        command,
        refresh_seconds,
        open_in,
        display,
    })
}

fn parse_menu_extension(
    entry: &serde_json::Map<String, serde_json::Value>,
    at: &str,
    out: &mut IdeSection,
) -> Option<MenuExtension> {
    let Some(items) = entry.get("items").and_then(serde_json::Value::as_array) else {
        out.problems.push(IdeProblem {
            location: format!("{at}.items"),
            message: "is required and must be an array of extension ids".to_owned(),
        });
        return None;
    };
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str().map(str::trim).filter(|t| !t.is_empty()) else {
            out.problems.push(IdeProblem {
                location: format!("{at}.items"),
                message: "every entry must be the id of a declared `action` extension".to_owned(),
            });
            return None;
        };
        if !ids.iter().any(|existing: &String| existing == text) {
            ids.push(text.to_owned());
        }
    }
    if ids.is_empty() {
        out.problems.push(IdeProblem {
            location: format!("{at}.items"),
            message: "must name at least one `action` extension".to_owned(),
        });
        return None;
    }
    Some(MenuExtension { items: ids })
}

fn parse_extension_hint(
    value: Option<&serde_json::Value>,
    at: &str,
    out: &mut IdeSection,
) -> Option<Option<ExtensionHint>> {
    let Some(value) = value else {
        return Some(None);
    };
    let Some(map) = value.as_object() else {
        out.problems.push(IdeProblem {
            location: format!("{at}.hint"),
            message: "must be an object with `text` and an optional `href`".to_owned(),
        });
        return None;
    };
    let Some(text) = map
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        out.problems.push(IdeProblem {
            location: format!("{at}.hint.text"),
            message: "is required and must be a non-empty string".to_owned(),
        });
        return None;
    };
    let href = match map.get("href") {
        None => None,
        Some(v) => {
            let Some(url) = v.as_str().map(str::trim).filter(|u| !u.is_empty()) else {
                out.problems.push(IdeProblem {
                    location: format!("{at}.hint.href"),
                    message: "must be an http:// or https:// URL".to_owned(),
                });
                return None;
            };
            if !is_web_url(url) {
                out.problems.push(IdeProblem {
                    location: format!("{at}.hint.href"),
                    message: format!("must be an http:// or https:// URL (got {url:?})"),
                });
                return None;
            }
            Some(url.to_owned())
        }
    };
    let mut unknown: Vec<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "text" && *k != "href")
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        out.problems.push(IdeProblem {
            location: format!("{at}.hint"),
            message: format!(
                "unknown key(s) {}. A hint may declare: href, text",
                unknown
                    .iter()
                    .map(|k| format!("{k:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    Some(Some(ExtensionHint {
        text: text.to_owned(),
        href,
    }))
}

/// `http(s)` only, the one restriction every repo-controlled URL in this module
/// carries — a click hands it to the OS, so `vscode://` or `file://` would turn a
/// config file into a launcher for whatever the machine has registered.
#[must_use]
pub fn is_web_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// How many characters an emoji icon may be.
///
/// A glyph, not a string. Bounded because this parses a *runtime* value — a status
/// extension's stdout — and it is rendered as text into a 42px bar, so an unbounded
/// one destroys the bar for that worktree. Two rather than one because a single
/// user-perceived emoji is routinely several `char`s (a variation selector, a skin
/// tone, a ZWJ sequence), and the alternative — counting graphemes — would mean a
/// segmentation dependency in `veld-core` for one field. Anything longer is not a
/// glyph and is refused rather than truncated, since half a ZWJ sequence renders as
/// something the author did not write.
const MAX_EMOJI_CHARS: usize = 8;

/// Read an icon out of a string, or `None` if it names nothing.
///
/// The same rule [`parse_pane_icon`] applies, without a problem to report: a
/// *status extension's output* may name an icon too, and there the answer arrives
/// at runtime rather than at lint time, so an unknown name has nobody to tell and
/// simply renders no glyph. Sharing the allowlist is the point — a name that works
/// in `veld.json` works in a badge's output and vice versa.
#[must_use]
pub fn parse_icon_name(text: &str) -> Option<PaneIcon> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if !text.is_ascii() {
        return (text.chars().count() <= MAX_EMOJI_CHARS).then(|| PaneIcon::Emoji(text.to_owned()));
    }
    PANE_ICON_NAMES
        .contains(&text)
        .then(|| PaneIcon::Name(text.to_owned()))
}

fn parse_pane(item: &serde_json::Value, at: &str, out: &mut IdeSection) -> Option<PaneDef> {
    let entry = item.as_object().or_else(|| {
        out.problems.push(IdeProblem {
            location: at.to_owned(),
            message: "must be an object with an `id` and a `type`".to_owned(),
        });
        None
    })?;

    // The two required keys are read before anything else so a typo in one is
    // reported against the entry rather than as a pile of downstream problems.
    let Some(id) = entry
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
    else {
        out.problems.push(IdeProblem {
            location: format!("{at}.id"),
            message: "is required and must be a string".to_owned(),
        });
        return None;
    };
    if !valid_pane_id(id) {
        out.problems.push(IdeProblem {
            location: format!("{at}.id"),
            message: format!("must be 1-64 characters of letters, digits, `-` or `_` (got {id:?})"),
        });
        return None;
    }
    let Some(pane_type) = entry.get("type").and_then(serde_json::Value::as_str) else {
        out.problems.push(IdeProblem {
            location: format!("{at}.type"),
            message: "is required — the only type this version renders is \"terminal\"".to_owned(),
        });
        return None;
    };

    let label = match entry.get("label") {
        None => id.to_owned(),
        Some(v) => match v.as_str().map(str::trim) {
            Some(text) if !text.is_empty() => text.to_owned(),
            _ => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.label"),
                    message: "must be a non-empty string".to_owned(),
                });
                return None;
            }
        },
    };
    let description = match entry.get("description") {
        None => None,
        Some(v) => match v.as_str().map(str::trim) {
            Some(text) if !text.is_empty() => Some(text.to_owned()),
            _ => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.description"),
                    message: "must be a non-empty string".to_owned(),
                });
                return None;
            }
        },
    };
    let icon = match entry.get("icon") {
        None => None,
        Some(v) => Some(parse_pane_icon(v, at, out)?),
    };
    let requires_bin = parse_requires_bin(entry.get("requires_bin"), at, out)?;

    let body = match pane_type {
        "terminal" => PaneBody::Terminal(parse_terminal_pane(entry, at, out)?),
        other => {
            // Not fatal and not a typo report: a project may legitimately be
            // written for a newer veld. Naming the version is the useful half —
            // "unknown key" would send the author looking for a spelling mistake
            // that isn't there.
            out.problems.push(IdeProblem {
                location: format!("{at}.type"),
                message: format!(
                    "pane type {other:?} is not one this version of veld renders (it knows: \
                     terminal). The pane was skipped; the rest of `ide.panes` still applies"
                ),
            });
            return None;
        }
    };

    // After the body, so a pane that is being dropped for a real reason does not
    // also collect a pile of key complaints.
    let mut unknown: Vec<&str> = entry
        .keys()
        .map(String::as_str)
        .filter(|k| !TERMINAL_PANE_KEYS.contains(k))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        out.problems.push(IdeProblem {
            location: at.to_owned(),
            message: format!(
                "unknown pane key(s) {}. A terminal pane may declare: {}",
                unknown
                    .iter()
                    .map(|k| format!("{k:?}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                TERMINAL_PANE_KEYS.join(", ")
            ),
        });
    }

    Some(PaneDef {
        id: id.to_owned(),
        label,
        description,
        icon,
        requires_bin,
        body,
    })
}

fn parse_terminal_pane(
    entry: &serde_json::Map<String, serde_json::Value>,
    at: &str,
    out: &mut IdeSection,
) -> Option<TerminalPane> {
    let launch = parse_command(entry, at, out)?;
    let resume = match entry.get("resume") {
        None => None,
        Some(value) => {
            let resume_at = format!("{at}.resume");
            let Some(map) = value.as_object() else {
                out.problems.push(IdeProblem {
                    location: resume_at,
                    message: "must be an object with `argv` or `shell`".to_owned(),
                });
                return None;
            };
            Some(parse_command(map, &resume_at, out)?)
        }
    };

    let auto_resume = match entry.get("auto_resume") {
        None => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(_) => {
            out.problems.push(IdeProblem {
                location: format!("{at}.auto_resume"),
                message: "must be true or false".to_owned(),
            });
            return None;
        }
    };
    // Fail closed, and downgrade rather than drop: a pane with nothing to resume
    // is still a perfectly good pane, but "resume this automatically" cannot mean
    // anything, and silently leaving the flag set would read as though it did.
    let close_on_exit = match entry.get("close_on_exit") {
        None => true,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(_) => {
            out.problems.push(IdeProblem {
                location: format!("{at}.close_on_exit"),
                message: "must be true or false".to_owned(),
            });
            return None;
        }
    };

    let allow_terminal_renaming = match entry.get("allow_terminal_renaming") {
        None => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(_) => {
            out.problems.push(IdeProblem {
                location: format!("{at}.allow_terminal_renaming"),
                message: "must be true or false".to_owned(),
            });
            return None;
        }
    };

    let auto_resume = if auto_resume && resume.is_none() {
        out.problems.push(IdeProblem {
            location: format!("{at}.auto_resume"),
            message: "has no effect without a `resume` command, so it was treated as false"
                .to_owned(),
        });
        false
    } else {
        auto_resume
    };

    Some(TerminalPane {
        launch,
        resume,
        auto_resume,
        close_on_exit,
        allow_terminal_renaming,
    })
}

/// Read the `argv` / `shell` pair out of a carrier object.
///
/// Hand-rolled rather than `serde`-derived for this module's reason — a bad
/// command has to become an [`IdeProblem`], never a load error — but the accepted
/// shape is exactly the one every other command position in the config takes, so
/// a pane command reads the same as a node's.
fn parse_command(
    entry: &serde_json::Map<String, serde_json::Value>,
    location: &str,
    out: &mut IdeSection,
) -> Option<crate::config::CommandSpec> {
    parse_command_in_scope(entry, location, "pane", PANE_BUILTINS, out)
}

fn parse_command_in_scope(
    entry: &serde_json::Map<String, serde_json::Value>,
    location: &str,
    scope: &str,
    allowed: &[&str],
    out: &mut IdeSection,
) -> Option<crate::config::CommandSpec> {
    let has_argv = entry.contains_key("argv");
    let has_shell = entry.contains_key("shell");
    if has_argv && has_shell {
        out.problems.push(IdeProblem {
            location: location.to_owned(),
            message: "declares both `argv` and `shell` — name exactly one".to_owned(),
        });
        return None;
    }
    let spec = if has_argv {
        let Some(items) = entry["argv"].as_array() else {
            out.problems.push(IdeProblem {
                location: format!("{location}.argv"),
                message: "must be an array of strings".to_owned(),
            });
            return None;
        };
        let mut argv = Vec::with_capacity(items.len());
        for item in items {
            let Some(text) = item.as_str() else {
                out.problems.push(IdeProblem {
                    location: format!("{location}.argv"),
                    message: "every argument must be a string".to_owned(),
                });
                return None;
            };
            argv.push(text.to_owned());
        }
        crate::config::CommandSpec::Argv(argv)
    } else if has_shell {
        let Some(text) = entry["shell"].as_str() else {
            out.problems.push(IdeProblem {
                location: format!("{location}.shell"),
                message: "must be a string".to_owned(),
            });
            return None;
        };
        crate::config::CommandSpec::Shell(text.to_owned())
    } else {
        out.problems.push(IdeProblem {
            location: location.to_owned(),
            message: "must declare `argv` or `shell`".to_owned(),
        });
        return None;
    };

    if spec.is_empty() {
        out.problems.push(IdeProblem {
            location: location.to_owned(),
            message: "runs nothing".to_owned(),
        });
        return None;
    }
    check_command_variables(&spec, location, scope, allowed, out)?;
    Some(spec)
}

/// Refuse a pane command that references a variable a pane will not have.
///
/// A pane command is interpolated against a much smaller context than a node's
/// ([`PANE_BUILTINS`]), and an unresolvable reference is not a soft failure at
/// spawn time — the pane simply never starts, with an error the author has to
/// read backwards from. Catching it in `veld lint` is the whole point of the
/// scope being closed.
///
/// A second, narrower check rides along: [`SHELL_REFUSED_BUILTINS`] names are in
/// `allowed` (so an `argv` command may use them) but refused when `spec` is
/// `Shell` — see that constant for why the asymmetry exists.
fn check_command_variables(
    spec: &crate::config::CommandSpec,
    location: &str,
    scope: &str,
    allowed: &[&str],
    out: &mut IdeSection,
) -> Option<()> {
    let is_shell = matches!(spec, crate::config::CommandSpec::Shell(_));
    let parts: Vec<String> = match spec {
        crate::config::CommandSpec::Argv(argv) => argv.clone(),
        crate::config::CommandSpec::Shell(s) => vec![s.clone()],
    };
    let available: Vec<&str> = allowed
        .iter()
        .copied()
        .filter(|n| !is_shell || !SHELL_REFUSED_BUILTINS.contains(n))
        .collect();
    let not_available = |shown: &str, out: &mut IdeSection| {
        out.problems.push(IdeProblem {
            location: location.to_owned(),
            message: format!(
                "`{shown}` is not available in a {scope} command. A {scope} may use: {}",
                variable_list(&available)
            ),
        });
    };
    for part in &parts {
        for reference in all_references(part) {
            let Some(name) = reference.strip_prefix("veld.") else {
                not_available(&format!("${{{reference}}}"), out);
                return None;
            };
            if is_shell && SHELL_REFUSED_BUILTINS.contains(&name) {
                out.problems.push(IdeProblem {
                    location: location.to_owned(),
                    message: format!(
                        "`${{veld.{name}}}` is refused in `shell` — it is not slugified, and \
                         git allows branch names like `foo$(id)` or `foo'bar` that make a shell \
                         string built from it into command execution on checkout. Use `argv`, \
                         which spawns it as a single fixed argument, or run \
                         `git rev-parse --abbrev-ref HEAD` inside the command instead."
                    ),
                });
                return None;
            }
            if !available.contains(&name) {
                not_available(&format!("${{veld.{name}}}"), out);
                return None;
            }
        }
    }
    Some(())
}

fn variable_list(allowed: &[&str]) -> String {
    allowed
        .iter()
        .map(|n| format!("${{veld.{n}}}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every `${…}` reference in `s`, whatever namespace it names.
///
/// Wider than `config::builtin_refs`, which only sees `${veld.*}`: a pane author
/// reaching for `${output.PORT}` has made a scope mistake worth naming, and a
/// scanner that only knew the `veld.` prefix would let it through to fail at
/// spawn time instead.
fn all_references(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        match after.find('}') {
            None => break,
            Some(end) => {
                out.push(after[..end].to_owned());
                rest = &after[end + 1..];
            }
        }
    }
    out
}

fn parse_pane_icon(value: &serde_json::Value, at: &str, out: &mut IdeSection) -> Option<PaneIcon> {
    let Some(text) = value.as_str().map(str::trim).filter(|t| !t.is_empty()) else {
        out.problems.push(IdeProblem {
            location: format!("{at}.icon"),
            message: "must be a non-empty string — an icon name or an emoji".to_owned(),
        });
        return None;
    };
    // ASCII means "this is meant to be a name", so a misspelled one is reported
    // rather than rendered as the literal text `sparkle`. Anything non-ASCII is
    // an emoji and needs no allowlist.
    if !text.is_ascii() {
        return Some(PaneIcon::Emoji(text.to_owned()));
    }
    if PANE_ICON_NAMES.contains(&text) {
        return Some(PaneIcon::Name(text.to_owned()));
    }
    out.problems.push(IdeProblem {
        location: format!("{at}.icon"),
        message: format!(
            "unknown icon {text:?}. Use an emoji, or one of: {}",
            PANE_ICON_NAMES.join(", ")
        ),
    });
    None
}

fn parse_requires_bin(
    value: Option<&serde_json::Value>,
    at: &str,
    out: &mut IdeSection,
) -> Option<Vec<String>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    let Some(items) = value.as_array() else {
        out.problems.push(IdeProblem {
            location: format!("{at}.requires_bin"),
            message: "must be an array of executable names".to_owned(),
        });
        return None;
    };
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str().map(str::trim).filter(|n| !n.is_empty()) else {
            out.problems.push(IdeProblem {
                location: format!("{at}.requires_bin"),
                message: "every entry must be a non-empty executable name".to_owned(),
            });
            return None;
        };
        // A name, looked up on the user's `PATH` — not a path. A check that
        // accepted `/opt/homebrew/bin/claude` would pass or fail on one machine's
        // layout, which is the opposite of what this field is for.
        if name.contains('/') || name.contains('\\') {
            out.problems.push(IdeProblem {
                location: format!("{at}.requires_bin"),
                message: format!(
                    "must be an executable name looked up on PATH, not a path (got {name:?})"
                ),
            });
            return None;
        }
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    }
    Some(names)
}

fn valid_pane_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether a slug can be a news id.
///
/// Kebab-case, the same grammar Veld's own promotion ids use, and the length
/// bound is not cosmetic: the bundle namespaces this slug per project before it
/// reaches storage, and the whole namespaced string has to fit the promotions
/// endpoint's 128-character ceiling. 64 leaves room for the prefix and the
/// project hash with plenty to spare.
///
/// The rule that matters most is what kebab-case *excludes*: a `:` cannot appear
/// here, which is what makes `proj:<hash>:<slug>` unambiguous and keeps a project
/// from writing an id that collides with one of Veld's. Do not loosen this to
/// admit `:` — see `NAMESPACE_SEPARATOR` in the bundle's `model.ts`.
fn valid_news_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Whether a string is a plausible `YYYY-MM-DD` day.
///
/// Shape and range only — no calendar. The gate this date feeds compares
/// day-granularity strings lexicographically, so `2026-02-31` would work
/// perfectly well and mean nothing; the check exists to catch the typo
/// (`2026-13-04`, `26-08-12`, `2026/08/12`) before it silently changes who sees a
/// card, not to validate a calendar veld has no reason to know about.
fn plausible_day(day: &str) -> bool {
    let bytes = day.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return false;
    }
    let num = |from: usize, to: usize| day[from..to].parse::<u32>().unwrap_or(0);
    (1..=12).contains(&num(5, 7)) && (1..=31).contains(&num(8, 10))
}

/// The latest day it currently is anywhere this machine can name — the **later** of
/// its local day and the UTC day, as the `YYYY-MM-DD` string [`plausible_day`]
/// accepts.
///
/// One of the two is always "tomorrow" relative to the other, and picking either one
/// alone is strict in one hemisphere. Local alone is strict in the **west**: an
/// author in Berlin dates a card today, and on a Los Angeles machine after 17:00 the
/// UTC day has already rolled over while the local day has not — so the card would
/// be dropped as "in the future" on the very day it was written. That is
/// fail-*closed* on the one thing this feature exists to do, which is deliver the
/// card. UTC alone is strict in the **east**, where an author writing their own local
/// today is a day ahead of it.
///
/// Taking the later of the two gives a day of slack in whichever direction the
/// machine needs, with no date arithmetic anywhere — both are `YYYY-MM-DD` strings,
/// so "later" is `max`. A transposed year (`2062` for `2026`) is still refused
/// everywhere, which is the typo this check exists for.
fn today_anywhere() -> String {
    let local = chrono::Local::now().format("%Y-%m-%d").to_string();
    let utc = chrono::Utc::now().format("%Y-%m-%d").to_string();
    local.max(utc)
}

/// Characters a card's copy may not contain at all.
///
/// Two groups, both invisible and both able to make a card claim something the text
/// does not say:
///
/// - **Zero-width and word-joiners** (`200B`–`200D`, `2060`, `FEFF`, `00AD`, `180E`).
///   `str::trim` removes `White_Space`, which does *not* include these — so
///   `"\u{200b}".repeat(24)` passes a 24-character eyebrow cap and renders as
///   nothing, spending a reader's one interrupt on a blank card.
/// - **Bidi controls** (`200E`/`200F`, `202A`–`202E`, `2066`–`2069`). These reorder
///   the line they sit in, and the line a project's card sits in is the byline the
///   reader is meant to be checking provenance against. Flex makes each string its
///   own bidi paragraph, which bounds the damage to one field — refusing them
///   removes it.
///
/// C0/C1 controls (`char::is_control`) are refused by the same check, one range up.
const INVISIBLE_COPY_CHARS: &[char] = &[
    '\u{00ad}', '\u{180e}', '\u{200b}', '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{202a}',
    '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2060}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}', '\u{feff}',
];

/// Whether copy is text a reader can actually see.
///
/// The length caps bound how *much* text a card carries; this bounds whether it
/// carries any. See [`INVISIBLE_COPY_CHARS`] for what is refused and why.
fn has_visible_text(text: &str) -> bool {
    if text
        .chars()
        .any(|c| c.is_control() || INVISIBLE_COPY_CHARS.contains(&c))
    {
        return false;
    }
    text.chars().any(|c| !c.is_whitespace())
}

/// Parse `ide.news` — the cards this project shows its own team.
///
/// Lenient like every other `ide` key: a malformed entry is dropped with a
/// problem that `veld lint` reports, never a load error. Note where this parser
/// deliberately does **not** follow the module's fail-closed rule: an unknown key
/// on an otherwise-valid entry reports a problem and *keeps* the card, exactly as
/// `ide.panes` does. Fail-closed exists because guessing at a permission hands
/// web content a capability nobody granted; a news card grants nothing, and
/// dropping one over a stray key is a change the team never hears about.
fn parse_news(value: &serde_json::Value, out: &mut IdeSection) {
    let Some(items) = value.as_array() else {
        out.problems.push(IdeProblem {
            location: "ide.news".to_owned(),
            message: "must be an array of news items; it was ignored".to_owned(),
        });
        return;
    };
    let today = today_anywhere();
    for (index, item) in items.iter().enumerate() {
        let at = format!("ide.news[{index}]");
        let Some(entry) = item.as_object() else {
            out.problems.push(IdeProblem {
                location: at,
                message: "must be an object with `id`, `since`, `eyebrow`, `headline` and `body`"
                    .to_owned(),
            });
            continue;
        };

        let id = entry.get("id").and_then(serde_json::Value::as_str);
        let Some(id) = id.filter(|id| valid_news_id(id)) else {
            out.problems.push(IdeProblem {
                location: format!("{at}.id"),
                message: format!(
                    "required, and must be a kebab-case slug of at most 64 characters \
                     such as \"build-moved-to-just\" (got {:?})",
                    id.unwrap_or_default()
                ),
            });
            continue;
        };
        // One id is one slot in the user's stored state, so two entries sharing one
        // would mean reading either marks both. First wins, matching `ide.panes`.
        if out.news.iter().any(|existing| existing.id == id) {
            out.problems.push(IdeProblem {
                location: format!("{at}.id"),
                message: format!("duplicate news id {id:?}; the first entry was kept"),
            });
            continue;
        }

        let since = entry.get("since").and_then(serde_json::Value::as_str);
        let Some(since) = since.filter(|s| plausible_day(s)) else {
            out.problems.push(IdeProblem {
                location: format!("{at}.since"),
                message: format!(
                    "required for every news item, as a YYYY-MM-DD day such as \
                     \"2026-08-12\" (got {:?}). It is shown on the card and it decides who \
                     is new enough not to need it — so there is no default",
                    since.unwrap_or_default()
                ),
            });
            continue;
        };

        // **A future day is refused, and this is the gate's load-bearing half.**
        // `since` is the only thing that retires a card: a reader who arrived after
        // it never sees it. A day that has not happened yet is therefore after
        // *every* arrival, forever — which re-creates exactly the never-expiring
        // card this channel deleted the `onboarding` kind to be rid of, in the one
        // half of it veld does not author. It is also the single likeliest typo
        // (`2062` for `2026`), which is why it is refused rather than reported: a
        // card that prompts the whole team until somebody notices is worse than a
        // card the author is told to date correctly.
        if since > today.as_str() {
            out.problems.push(IdeProblem {
                location: format!("{at}.since"),
                message: format!(
                    "is in the future ({since:?}, and today is {today:?}), so it was dropped. \
                     A card is retired by its date — one that has not happened yet would \
                     reach every teammate forever, including everybody who joins later"
                ),
            });
            continue;
        }

        // The three copy fields, each dropped rather than truncated on a breach:
        // half a sentence reads as a bug in veld, where a lint finding reads as
        // what it is.
        let mut copy: Vec<&str> = Vec::with_capacity(3);
        let mut bad = false;
        for (key, max) in [
            ("eyebrow", MAX_NEWS_EYEBROW),
            ("headline", MAX_NEWS_HEADLINE),
            ("body", MAX_NEWS_BODY),
        ] {
            let text = entry.get(key).and_then(serde_json::Value::as_str);
            match text {
                // `has_visible_text` rather than `!trim().is_empty()`: trim only
                // removes `White_Space`, so zero-width padding is text by the cap's
                // reckoning and nothing at all by the reader's.
                Some(text) if has_visible_text(text) && text.chars().count() <= max => {
                    copy.push(text);
                }
                _ => {
                    bad = true;
                    out.problems.push(IdeProblem {
                        location: format!("{at}.{key}"),
                        message: format!(
                            "required, and must be 1-{max} characters of visible plain text \
                             (got {}). The cap is the point: a card is a headline and one \
                             sentence, so it is worth being interrupted by",
                            text.map_or_else(
                                || "nothing".to_owned(),
                                |t| format!("{}", t.chars().count())
                            )
                        ),
                    });
                }
            }
        }
        if bad {
            continue;
        }

        let glyph = match entry.get("glyph").map(serde_json::Value::as_str) {
            None => "inbox",
            Some(Some(name)) if NEWS_GLYPHS.contains(&name) => name,
            other => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.glyph"),
                    message: format!(
                        "must be one of {} (got {:?})",
                        NEWS_GLYPHS.join(", "),
                        other.flatten().unwrap_or_default()
                    ),
                });
                continue;
            }
        };

        // After the item is known to be good, so a card being dropped for a real
        // reason does not also collect a pile of key complaints.
        let mut unknown: Vec<&str> = entry
            .keys()
            .map(String::as_str)
            .filter(|k| !NEWS_ITEM_KEYS.contains(k))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            out.problems.push(IdeProblem {
                location: at,
                message: format!(
                    "unknown news key(s) {}. A news item may declare: {}",
                    unknown
                        .iter()
                        .map(|k| format!("{k:?}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    NEWS_ITEM_KEYS.join(", ")
                ),
            });
        }

        out.news.push(NewsItem {
            id: id.to_owned(),
            since: since.to_owned(),
            eyebrow: copy[0].to_owned(),
            headline: copy[1].to_owned(),
            body: copy[2].to_owned(),
            glyph: glyph.to_owned(),
        });
    }

    // **Over the cap, the oldest entries by `since` go — not the ones at the front
    // of the array.**
    //
    // Two decisions here, and the second corrects the first. Applying the cap after
    // the whole array is parsed, rather than refusing items once the cap is reached,
    // is what stops the card that *just landed* being the one dropped: authors are
    // told to append, and the history view breaks a shared day by reverse array
    // order, so the last entry is the newest thing.
    //
    // But array position is **not** chronology, and assuming it was would have made
    // this a worse version of the bug it fixed. `ide` arrays *concatenate across
    // `include` files* in sorted-filename order (see `merge_reserved` in
    // `include.rs`), and `docs/configuration.md` actively suggests moving news into
    // `veld.d/news.jsonc` — so a second file sorting earlier puts a newer card at
    // the front. An author who prepends, which is how changelogs are written, does
    // the same thing. `since` is the field that means "when", so `since` is what
    // decides; the array index only breaks a tie, ascending, matching the history
    // view's own within-a-day order.
    //
    // Survivors keep their relative order, because that order is what `historyOf`
    // ties-breaks on.
    if out.news.len() > MAX_NEWS_ITEMS {
        let mut by_age: Vec<usize> = (0..out.news.len()).collect();
        by_age.sort_by(|&a, &b| out.news[a].since.cmp(&out.news[b].since).then(a.cmp(&b)));
        let mut doomed: Vec<usize> = by_age
            .into_iter()
            .take(out.news.len() - MAX_NEWS_ITEMS)
            .collect();
        doomed.sort_unstable();
        let ids: Vec<String> = doomed
            .iter()
            .rev()
            .map(|&i| format!("{:?}", out.news.remove(i).id))
            .collect();
        out.problems.push(IdeProblem {
            location: "ide.news".to_owned(),
            message: format!(
                "a project may have at most {MAX_NEWS_ITEMS} news items live at once, so the \
                 {} with the oldest `since` were dropped: {}. Retiring a news item is deleting \
                 it — remove the ones that have stopped being news",
                doomed.len(),
                ids.iter().rev().cloned().collect::<Vec<_>>().join(", ")
            ),
        });
    }
}

fn parse_quicklinks(value: &serde_json::Value, out: &mut IdeSection) {
    let Some(items) = value.as_array() else {
        out.problems.push(IdeProblem {
            location: "ide.quicklinks".to_owned(),
            message: "must be an array of { label, url } objects; it was ignored".to_owned(),
        });
        return;
    };
    for (index, item) in items.iter().enumerate() {
        let at = format!("ide.quicklinks[{index}]");
        let Some(entry) = item.as_object() else {
            out.problems.push(IdeProblem {
                location: at,
                message: "must be an object with `label` and `url`".to_owned(),
            });
            continue;
        };
        let label = entry.get("label").and_then(serde_json::Value::as_str);
        let url = entry.get("url").and_then(serde_json::Value::as_str);
        let (Some(label), Some(url)) = (label, url) else {
            out.problems.push(IdeProblem {
                location: at,
                message: "`label` and `url` are both required and must be strings".to_owned(),
            });
            continue;
        };
        if label.trim().is_empty() {
            out.problems.push(IdeProblem {
                location: format!("{at}.label"),
                message: "must not be empty".to_owned(),
            });
            continue;
        }
        // http(s) only. A quicklink is a repo-controlled string that a click hands
        // to the OS, so `vscode://`, `file://` and friends would turn a config file
        // into a launcher for whatever the machine happens to have registered.
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            out.problems.push(IdeProblem {
                location: format!("{at}.url"),
                message: format!("must be an http:// or https:// URL (got {url:?})"),
            });
            continue;
        }
        out.quicklinks.push(Quicklink {
            label: label.to_owned(),
            url: url.to_owned(),
        });
    }
}

fn parse_external_origins(value: &serde_json::Value, out: &mut IdeSection) {
    let Some(items) = value.as_array() else {
        out.problems.push(IdeProblem {
            location: "ide.externalOrigins".to_owned(),
            message: "must be an array of origin strings such as \
                      [\"https://accounts.google.com\"]; it was ignored"
                .to_owned(),
        });
        return;
    };
    for (index, item) in items.iter().enumerate() {
        let at = format!("ide.externalOrigins[{index}]");
        let Some(raw) = item.as_str() else {
            out.problems.push(IdeProblem {
                location: at,
                message: "must be a string such as \"https://*.okta.com\"".to_owned(),
            });
            continue;
        };
        match parse_origin(raw) {
            Ok(origin) => out.external_origins.push(origin),
            Err(message) => out.problems.push(IdeProblem {
                location: at,
                message,
            }),
        }
    }
}

fn parse_permissions(value: &serde_json::Value, out: &mut IdeSection) {
    let Some(items) = value.as_array() else {
        out.problems.push(IdeProblem {
            location: "ide.permissions".to_owned(),
            message: "must be an array of { origin, allow, deny } objects; it was ignored"
                .to_owned(),
        });
        return;
    };
    for (index, item) in items.iter().enumerate() {
        let at = format!("ide.permissions[{index}]");
        let Some(entry) = item.as_object() else {
            out.problems.push(IdeProblem {
                location: at,
                message: "must be an object with an `origin`".to_owned(),
            });
            continue;
        };
        let Some(raw_origin) = entry.get("origin").and_then(serde_json::Value::as_str) else {
            out.problems.push(IdeProblem {
                location: format!("{at}.origin"),
                message: "is required and must be a string such as \"http://localhost:*\""
                    .to_owned(),
            });
            continue;
        };
        let origin = match parse_origin(raw_origin) {
            Ok(origin) => origin,
            Err(message) => {
                out.problems.push(IdeProblem {
                    location: format!("{at}.origin"),
                    message,
                });
                continue;
            }
        };
        let allow = parse_permission_list(entry.get("allow"), &at, "allow", out);
        let deny = parse_permission_list(entry.get("deny"), &at, "deny", out);
        if allow.is_empty() && deny.is_empty() {
            out.problems.push(IdeProblem {
                location: at,
                message: format!(
                    "names neither an `allow` nor a `deny` permission, so it does nothing for \
                     {raw_origin}"
                ),
            });
            continue;
        }
        out.permissions.push(PermissionRule {
            origin,
            allow,
            deny,
        });
    }
}

fn parse_permission_list(
    value: Option<&serde_json::Value>,
    at: &str,
    field: &str,
    out: &mut IdeSection,
) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        out.problems.push(IdeProblem {
            location: format!("{at}.{field}"),
            message: "must be an array of permission ids".to_owned(),
        });
        return Vec::new();
    };
    let mut ids = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(id) = item.as_str() else {
            out.problems.push(IdeProblem {
                location: format!("{at}.{field}[{index}]"),
                message: "must be a string".to_owned(),
            });
            continue;
        };
        // `open-external` may be refused by a project but never granted by one.
        // `parse_quicklinks` rejects a non-http(s) URL because a config file must
        // not become "a launcher for whatever the machine happens to have
        // registered" — and a silent `open-external` grant restores exactly that,
        // with the page choosing the protocol instead of the config. The two
        // rules would otherwise contradict each other in the same file. A person
        // can still allow it at a prompt, which is the human-in-the-loop the
        // quicklink rule is really protecting.
        if (field == "allow") && id == "open-external" {
            out.problems.push(IdeProblem {
                location: format!("{at}.{field}[{index}]"),
                message: "`open-external` cannot be granted by a project — it hands a \
                          page-chosen protocol URL to the OS, the same thing `quicklinks` \
                          refuses. Use `deny` to withdraw it, or answer the prompt by hand"
                    .to_owned(),
            });
            continue;
        }
        if !PERMISSION_IDS.contains(&id) {
            // Naming the whole set is worth the line width here: the ids are veld's
            // own, so an author who guesses Electron's or Chrome's spelling gets no
            // other hint that `mic` is not `microphone`.
            out.problems.push(IdeProblem {
                location: format!("{at}.{field}[{index}]"),
                message: format!(
                    "unknown permission {id:?}. Expected one of: {}",
                    PERMISSION_IDS.join(", ")
                ),
            });
            continue;
        }
        if ids.iter().any(|existing| existing == id) {
            continue;
        }
        ids.push(id.to_owned());
    }
    ids
}

/// Split `scheme://host[:port]` into its parts, or say what is wrong with it.
///
/// Public because the same grammar has a second producer: the
/// `browser.externalOrigins` **setting** holds the same pattern strings as
/// [`IdeSection::external_origins`], and a settings list validated by a second,
/// looser parser would accept a pattern that then matched nothing.
///
/// Hand-rolled rather than routed through a URL crate because the accepted grammar
/// is deliberately *narrower* than a URL: no path, no credentials, no query, and a
/// `*` in the port position that no parser would accept. A permissive parser here
/// would quietly accept `http://evil.com@localhost` and match it against the wrong
/// host, which is the classic version of this bug.
pub fn parse_origin(raw: &str) -> Result<OriginPattern, String> {
    let trimmed = raw.trim();
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
        format!("must be a full origin such as \"http://localhost:*\" (got {raw:?})")
    })?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "only http:// and https:// origins are supported (got {scheme:?})"
        ));
    }
    if rest.contains('/') {
        return Err(format!(
            "must be an origin, with no path or trailing slash (got {raw:?})"
        ));
    }
    if rest.contains('@') || rest.contains('?') || rest.contains('#') {
        return Err(format!(
            "must be a bare scheme://host[:port] with no credentials, query or fragment (got \
             {raw:?})"
        ));
    }

    // IPv6 literals are bracketed, and the brackets are what separates the address'
    // own colons from the port separator.
    let (host, port_part) = if let Some(after) = rest.strip_prefix('[') {
        let (inside, tail) = after
            .split_once(']')
            .ok_or_else(|| format!("unterminated IPv6 host in {raw:?}"))?;
        let port = match tail {
            "" => None,
            other => Some(
                other
                    .strip_prefix(':')
                    .ok_or_else(|| format!("expected \":port\" after the IPv6 host in {raw:?}"))?,
            ),
        };
        // Parsed and re-serialised, because the *matcher* is JavaScript and
        // `new URL(...).hostname` compresses an IPv6 literal: `[0:0:0:0:0:0:0:1]`
        // becomes `[::1]` there, and a rule that stayed expanded here could never
        // match. `Ipv6Addr`'s Display agrees with it — **except** for an
        // IPv4-mapped address, where Rust prints `::ffff:127.0.0.1` and Chromium
        // prints `::ffff:7f00:1`. Refused rather than written out wrong: the
        // alternative is a rule that silently matches nothing, which is exactly
        // what this parser exists to turn into a lint message.
        let addr = inside
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| format!("not a valid IPv6 address ({raw:?})"))?;
        if addr.to_ipv4_mapped().is_some() {
            return Err(format!(
                "an IPv4-mapped IPv6 address is spelled differently by the browser that has to \
                 match it, so this rule could never fire — write the plain IPv4 form instead \
                 (got {raw:?})"
            ));
        }
        (format!("[{addr}]"), port)
    } else {
        match rest.split_once(':') {
            Some((host, port)) => (host.to_ascii_lowercase(), Some(port)),
            None => (rest.to_ascii_lowercase(), None),
        }
    };

    // A single trailing dot is the fully-qualified spelling of the same name, and
    // the matcher strips it — so a rule that kept it could never match, and
    // `is_local_origin` would call `localhost.` remote and warn about loopback.
    let host = host.strip_suffix('.').map(str::to_owned).unwrap_or(host);
    if host.is_empty() || host == "[]" {
        return Err(format!("names no host ({raw:?})"));
    }
    // A host whose last label is numeric is an *IP address* to every URL parser,
    // and they accept forms this one does not: `new URL()` folds `127.1`,
    // `2130706433`, `0177.0.0.1` and `127.0.0.01` all to `127.0.0.1`. Stored
    // verbatim, such a rule could never fire — and `is_local_origin` would call it
    // remote, so `veld lint` would warn the author that their own loopback grant
    // "is not this machine". Refused with the canonical spelling named, which is
    // the same treatment the IPv4-mapped and Unicode forms get above.
    let last_label = host.rsplit('.').next().unwrap_or("");
    let numeric_last_label = !last_label.is_empty()
        && (last_label.chars().all(|c| c.is_ascii_digit())
            // `0x` alone does not make a label a number — the URL spec requires
            // the remainder to be hex digits. `0xy` and `foo.0xy` are ordinary
            // hostnames that browsers keep verbatim, and refusing them told the
            // author to "write four decimal octets" about a name that is not an
            // address at all.
            || matches!(
                last_label.get(..2).map(str::to_ascii_lowercase).as_deref(),
                Some("0x")
            ) && last_label.len() > 2
                && last_label[2..].chars().all(|c| c.is_ascii_hexdigit()));
    if numeric_last_label
        && host.parse::<std::net::Ipv4Addr>().map(|ip| ip.to_string()) != Ok(host.clone())
    {
        return Err(format!(
            "a host ending in a number is read as an IPv4 address, and the browser will \
             normalise this one to a different string than veld stores — write it as four \
             plain decimal octets (got {raw:?})"
        ));
    }
    // The matcher runs hosts through `new URL()`, which punycodes a Unicode name;
    // this parser does not, so such a rule could only ever fail to match. Say so
    // rather than storing something that quietly does nothing.
    if !host.is_ascii() {
        return Err(format!(
            "a non-ASCII host must be written in its punycode form (`xn--…`), which is what the \
             browser compares against (got {raw:?})"
        ));
    }

    // A leading `*.` is the only wildcard position. Everything else — `a*.b`,
    // `*.a.*`, a bare `*` — is refused rather than interpreted, because a
    // half-understood host pattern grants a capability to a host nobody named.
    let (host, wildcard) = match host.strip_prefix("*.") {
        Some(suffix) => (suffix.to_owned(), true),
        None => (host, false),
    };
    if host.contains('*') {
        return Err(format!(
            "`*` is only allowed as the leading label of the host (`*.example.com`) or as the \
             whole port (got {raw:?})"
        ));
    }
    if wildcard {
        if host.is_empty() {
            return Err(format!(
                "a wildcard needs something to match under ({raw:?})"
            ));
        }
        if host.starts_with('[') {
            return Err(format!(
                "an IP address has no subdomains, so `*.` means nothing here ({raw:?})"
            ));
        }
        // One label under the wildcard is a whole TLD — `*.com`, `*.dev`, `*.io` —
        // which is never what anyone meant and is the accident worth refusing
        // outright. `localhost` is the deliberate exception: RFC 6761 pins it to
        // loopback, so `*.localhost` cannot name anything on the network. This is
        // not a public-suffix list, and does not pretend to be one: `*.co.uk`
        // passes. It catches the mistake people actually make.
        if !host.contains('.') && host != "localhost" {
            return Err(format!(
                "`*.{host}` would match every host under a top-level domain. Name at least one \
                 more label (got {raw:?})"
            ));
        }
    }

    let port = match port_part {
        // An omitted port is the scheme's default port, not "any port". `*` is how
        // "any" is spelled, and it has to be explicit or `http://example.com` would
        // silently cover every service on that host.
        None => Some(if scheme == "https" { 443 } else { 80 }),
        Some("*") => None,
        Some(text) => Some(
            text.parse::<u16>()
                .map_err(|_| format!("port must be a number or \"*\" (got {text:?} in {raw:?})"))?,
        ),
    };

    Ok(OriginPattern {
        raw: trimmed.to_owned(),
        scheme,
        host,
        wildcard,
        port,
    })
}

// ---------------------------------------------------------------------------
// Where a URL opens
// ---------------------------------------------------------------------------

/// Where a URL a terminal produced should be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlTarget {
    /// A Veld browser pane, in the window showing that terminal.
    Pane,
    /// The user's real browser, via the OS.
    System,
}

/// A URL reduced to what an [`OriginPattern`] compares against.
///
/// Deliberately the same three fields, derived the same way, as `parseOrigin` in
/// `desktop/src/permissions.js`: lowercased host with a single trailing dot
/// stripped, and an absent port resolved to the scheme's default so
/// `https://example.com` and `https://example.com:443` compare equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlOrigin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

/// A URL that has been through [`parse_web_url`], and therefore the only kind that
/// may be handed onward to something that will *load* it.
///
/// A newtype with no public constructor, deliberately: "route the parsed URL but
/// forward the string the caller sent" is a one-word edit that compiles, passes every
/// test that checks the parser in isolation, and silently reopens the exempt-list
/// bypass described on [`parse_web_url`]. Making the frame's field this type means
/// that edit does not compile. A comment there would have been the fifth guard in this
/// module defended only by prose.
/// `Serialize` only, and that is part of the guard rather than an omission: a derived
/// `Deserialize` is expanded with access to the private field, so
/// `serde_json::from_str::<CanonicalUrl>("anything")` — or any future struct that
/// derives `Deserialize` with a field of this type — would construct one from
/// arbitrary text without ever passing through [`parse_web_url`]. That is the same
/// bypass reached through serde instead of through a literal constructor, and it would
/// have made the claim above ("that edit does not compile") false. Nothing needs to
/// deserialize this: the frame travels daemon → renderer, and the renderer parses JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CanonicalUrl(String);

impl CanonicalUrl {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CanonicalUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An `http(s)` URL, parsed the way the thing that will *load* it parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebUrl {
    /// What the exempt list is matched against.
    pub origin: UrlOrigin,
    /// The **canonical serialisation**, and the only string that should travel
    /// onward. See [`parse_web_url`] for why routing on one string and opening a
    /// different one was a real defect rather than a tidiness point.
    pub canonical: CanonicalUrl,
}

/// Parse an `http(s)` URL, or `None` if it is not one.
///
/// # Why this uses a real URL parser and returns a canonical string
///
/// This was hand-rolled once, scanning for the authority up to `/`, `?` or `#` and
/// taking what followed the last `@`. That is not what a browser does, and the gap
/// was **exploitable against the one control this feature offers**: a WHATWG parser
/// also ends the authority at `\` and strips ASCII tab/CR/LF anywhere in the URL, so
///
/// - `https://accounts.google.com\@evil.com` scanned as host `evil.com` — not on the
///   exempt list, so routed to a *pane* — while the pane loaded
///   `accounts.google.com`; and
/// - a tab inside the host (`accounts.goo<TAB>gle.com`) scanned as a different host
///   for the same result.
///
/// Either one silently sidesteps an `externalOrigins` entry, which is exactly how an
/// SSO or banking host the user pinned to their real browser ends up rendering in a
/// pane on the shared cookie jar.
///
/// Two rules follow, and both are load-bearing:
///
/// 1. **Parse with the same standard the loader implements.** `url::Url` (via
///    `reqwest`, already a dependency) implements the WHATWG URL Standard, which is
///    what Chromium and `new URL()` in the renderer implement. Do not reintroduce
///    hand scanning here — the narrow custom grammar in [`parse_origin`] is for
///    *patterns*, which are veld's own syntax and deliberately not URLs.
/// 2. **Route and open the same string.** [`WebUrl::canonical`] is what goes into the
///    `open_url` frame, so "what was checked" and "what will be loaded" cannot
///    disagree no matter how the input was spelled.
///
/// Parsing here also removes two limitations the hand-rolled version documented: an
/// internationalised host is punycoded (so it can match an `xn--…` pattern) and
/// `http://127.1` normalises to `127.0.0.1` the way every URL parser resolves it.
#[must_use]
pub fn parse_web_url(url: &str) -> Option<WebUrl> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    // A pane accepts nothing else, and neither does `normalizeBrowserUrl` in the
    // renderer or `safeUrl` in the desktop shell.
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // Already lowercased, punycoded and IP-normalised by the parser. IPv6 keeps its
    // brackets, which is the form `parse_origin` stores.
    let host = parsed.host_str()?;
    if host.is_empty() {
        return None;
    }
    // A single trailing dot is the fully-qualified spelling of the same name, and
    // `parse_origin` strips it from patterns — so it has to come off here too or a
    // link to the dotted form would slip past every exempt entry. (The URL Standard
    // keeps it, which is why the parser leaves it in.)
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    Some(WebUrl {
        origin: UrlOrigin {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            port: parsed.port_or_known_default()?,
        },
        canonical: CanonicalUrl(parsed.as_str().to_owned()),
    })
}

/// Reduce an `http(s)` URL to its origin, or `None` if it has none.
///
/// A thin wrapper over [`parse_web_url`] for callers that only need the origin.
#[must_use]
pub fn parse_url_origin(url: &str) -> Option<UrlOrigin> {
    parse_web_url(url).map(|u| u.origin)
}

/// Whether a URL's origin matches one pattern.
///
/// The comparison rules are `matchesPattern`'s in `desktop/src/permissions.js`,
/// restated here because that matcher answers permission requests in the Electron
/// main process and this one answers a routing question in the daemon — two
/// consumers of one normalised pattern, not one implementation that could be
/// shared. Both are pinned by tests using the same cases, in particular the one
/// that matters: a wildcard is **label-wise**, so `*.veld.localhost` does not
/// match `evilveld.localhost`, and does not match the bare suffix either.
#[must_use]
pub fn origin_matches(origin: &UrlOrigin, pattern: &OriginPattern) -> bool {
    if origin.scheme != pattern.scheme {
        return false;
    }
    let host_matches = if pattern.wildcard {
        origin.host.ends_with(&format!(".{}", pattern.host))
    } else {
        origin.host == pattern.host
    };
    if !host_matches {
        return false;
    }
    // `None` is the pattern's `*`.
    pattern.port.is_none_or(|port| port == origin.port)
}

/// Decide where a URL produced by a terminal session opens.
///
/// The **one** owner of that decision, and the reason both entry points (a click
/// on a link in the terminal, and a process in the shell invoking `$BROWSER`) go
/// through the daemon rather than deciding locally: the project's exempt list
/// lives in its `veld.json`, which the renderer does not read, and two
/// implementations of one policy drift.
///
/// `external` is the union of the `browser.externalOrigins` setting and the
/// project's `ide.externalOrigins` — unioned rather than overridden, because both
/// answer the same question ("this host needs my real browser") from different
/// distances and neither is a correction of the other.
///
/// Anything that is not an `http(s)` URL is [`UrlTarget::System`]: a pane refuses
/// it anyway (`normalizeBrowserUrl`, and again in the shell), so the honest answer
/// is that Veld is not where it opens.
#[must_use]
pub fn route_url(url: &str, open_in_app: bool, external: &[OriginPattern]) -> UrlTarget {
    if !open_in_app {
        return UrlTarget::System;
    }
    let Some(origin) = parse_url_origin(url) else {
        return UrlTarget::System;
    };
    if external.iter().any(|p| origin_matches(&origin, p)) {
        return UrlTarget::System;
    }
    UrlTarget::Pane
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn section(value: serde_json::Value) -> IdeSection {
        parse(Some(&value))
    }

    fn pattern(raw: &str) -> OriginPattern {
        parse_origin(raw).expect(raw)
    }

    #[test]
    fn external_origins_parse_and_report_bad_entries() {
        let parsed = section(json!({
            "externalOrigins": ["https://accounts.google.com", "https://*.okta.com"],
        }));
        assert_eq!(parsed.external_origins.len(), 2);
        assert_eq!(parsed.external_origins[0].host, "accounts.google.com");
        assert_eq!(parsed.external_origins[0].port, Some(443));
        assert!(parsed.external_origins[1].wildcard);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert!(!parsed.is_empty(), "a list of origins is worth sending");

        // The same parser `ide.permissions` uses, so the same refusals reach the
        // author as lint findings rather than as a rule that matches nothing.
        let bad = section(json!({
            "externalOrigins": ["accounts.google.com", "https://*.com", 7, "https://a.com/path"],
        }));
        assert!(bad.external_origins.is_empty());
        assert_eq!(bad.problems.len(), 4);
        assert_eq!(bad.problems[0].location, "ide.externalOrigins[0]");
        assert!(bad.problems[1].message.contains("top-level domain"));

        // A non-array is one finding, not a panic, and drops only this key.
        let wrong = section(json!({ "externalOrigins": "https://a.com" }));
        assert_eq!(wrong.problems.len(), 1);
        assert_eq!(wrong.problems[0].location, "ide.externalOrigins");
    }

    #[test]
    fn a_url_reduces_to_the_origin_a_browser_would_navigate_to() {
        let o = parse_url_origin("https://Example.COM/a/b?c=1#d").expect("origin");
        assert_eq!(o.scheme, "https");
        assert_eq!(o.host, "example.com");
        // An absent port is the scheme's default, so a pattern written either way
        // compares equal.
        assert_eq!(o.port, 443);
        assert_eq!(parse_url_origin("http://example.com").unwrap().port, 80);
        assert_eq!(
            parse_url_origin("http://example.com:3000/x").unwrap().port,
            3000
        );

        // The authority ends at the first `/`, `?` **or** `#`.
        for url in [
            "https://example.com?next=/login",
            "https://example.com#/route",
            "https://example.com/",
        ] {
            assert_eq!(parse_url_origin(url).unwrap().host, "example.com", "{url}");
        }

        // The host is what follows the LAST `@`. Reading the userinfo as the host
        // would answer the exempt list's question about a name the browser never
        // visits.
        assert_eq!(
            parse_url_origin("http://accounts.google.com@evil.com/x")
                .unwrap()
                .host,
            "evil.com"
        );
        assert_eq!(
            parse_url_origin("http://user:pw@example.com:8080/x").unwrap(),
            UrlOrigin {
                scheme: "http".to_owned(),
                host: "example.com".to_owned(),
                port: 8080,
            }
        );

        // A trailing dot is the same name, and patterns never carry one.
        assert_eq!(
            parse_url_origin("https://example.com./x").unwrap().host,
            "example.com"
        );

        // IPv6 keeps its brackets, the way `parse_origin` stores them.
        assert_eq!(
            parse_url_origin("http://[::1]:8080/").unwrap().host,
            "[::1]"
        );

        // Nothing without an http(s) scheme and a host has an origin. Each of these
        // is also rejected (or has no host) in a browser — checked with `new URL()`,
        // because agreeing with the loader is the whole contract of this function.
        for url in [
            "",
            "example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "vscode://open",
            "https://",
            "http://:8080/",
            "https://example.com:notaport/",
        ] {
            assert_eq!(parse_url_origin(url), None, "{url:?} must have no origin");
        }
        // …and where a browser *does* find a host in something odd, so does this:
        // `new URL("https:///path").hostname` is `path`. The hand-rolled version
        // this replaced answered `None` here, which is the shape of the bug below.
        assert_eq!(parse_url_origin("https:///path").unwrap().host, "path");
    }

    /// The exempt list must not be sidesteppable by spelling the URL differently.
    ///
    /// Both of these routed on the wrong host before `parse_web_url` used a real URL
    /// parser: a WHATWG parser ends the authority at `\` as well as `/?#`, and strips
    /// ASCII tab/CR/LF anywhere — so the daemon checked one host while the pane
    /// loaded another, and an `externalOrigins` entry for an SSO host silently did
    /// not apply. The canonical string is what travels onward for the same reason.
    #[test]
    fn a_url_cannot_be_spelled_to_route_on_one_host_and_load_another() {
        let external = vec![pattern("https://accounts.google.com")];

        for spelling in [
            // Verified against `new URL()`: every one of these loads
            // accounts.google.com.
            "https://accounts.google.com\\@evil.com",
            "https://accounts.goo\tgle.com/x",
            "https://accounts.google.com\r\n/x",
            "https://ACCOUNTS.Google.COM/x",
            "https://accounts.google.com./x",
            "  https://accounts.google.com/x  ",
        ] {
            let parsed = parse_web_url(spelling).unwrap_or_else(|| panic!("{spelling:?}"));
            assert_eq!(
                parsed.origin.host, "accounts.google.com",
                "{spelling:?} routed on the wrong host"
            );
            assert_eq!(
                route_url(spelling, true, &external),
                UrlTarget::System,
                "{spelling:?} escaped the exempt list"
            );
            // The invariant that makes "route one string, open another" impossible:
            // what will be opened is the canonical form, and re-routing *that*
            // reaches the same decision. (A trailing dot survives canonicalisation,
            // as it does in a browser, which is why this is idempotence rather than
            // a literal prefix check.)
            let again = parse_web_url(parsed.canonical.as_str()).expect("canonical re-parses");
            assert_eq!(
                again.origin, parsed.origin,
                "{spelling:?} is not idempotent"
            );
            assert_eq!(
                route_url(parsed.canonical.as_str(), true, &external),
                UrlTarget::System,
                "{spelling:?} canonicalised into something that escapes the list"
            );
        }
    }

    #[test]
    fn matching_is_label_wise_and_scheme_and_port_exact() {
        let url = |u: &str| parse_url_origin(u).expect(u);

        let exact = pattern("https://accounts.google.com");
        assert!(origin_matches(
            &url("https://accounts.google.com/o/oauth2"),
            &exact
        ));
        // Scheme is part of the origin.
        assert!(!origin_matches(&url("http://accounts.google.com/"), &exact));
        // An omitted port means the default port, not any port.
        assert!(!origin_matches(
            &url("https://accounts.google.com:8443/"),
            &exact
        ));
        // A subdomain is a different host unless a wildcard says otherwise.
        assert!(!origin_matches(
            &url("https://mail.accounts.google.com/"),
            &exact
        ));

        let wild = pattern("https://*.okta.com");
        assert!(origin_matches(
            &url("https://dev-123.okta.com/login"),
            &wild
        ));
        // Label-wise: this is the check that a bare `ends_with` gets wrong.
        assert!(!origin_matches(&url("https://evilokta.com/"), &wild));
        // …and a wildcard is subdomains, not the suffix itself.
        assert!(!origin_matches(&url("https://okta.com/"), &wild));

        // `*` in the port position is the only "any port".
        let any_port = pattern("http://localhost:*");
        for u in ["http://localhost/", "http://localhost:3000/x"] {
            assert!(origin_matches(&url(u), &any_port), "{u}");
        }
        assert!(!origin_matches(&url("https://localhost:3000/"), &any_port));
    }

    #[test]
    fn routing_is_pane_unless_something_says_otherwise() {
        let external = vec![pattern("https://*.okta.com")];

        assert_eq!(
            route_url("https://web.dev.app.localhost/", true, &external),
            UrlTarget::Pane
        );
        assert_eq!(
            route_url("https://dev-1.okta.com/login", true, &external),
            UrlTarget::System
        );
        // The master switch wins over an empty list…
        assert_eq!(
            route_url("https://web.dev.app.localhost/", false, &[]),
            UrlTarget::System
        );
        // …and an empty list with the switch on is the whole point of the feature.
        assert_eq!(
            route_url("https://anything.example.com/", true, &[]),
            UrlTarget::Pane
        );
        // A pane cannot show these at all, so Veld is not where they open.
        for url in ["file:///etc/passwd", "vscode://open", "not a url"] {
            assert_eq!(route_url(url, true, &external), UrlTarget::System, "{url}");
        }
    }

    #[test]
    fn an_absent_ide_section_is_empty_and_silent() {
        let parsed = parse(None);
        assert!(parsed.is_empty());
        assert!(parsed.problems.is_empty());
        assert!(parsed.uninterpreted.is_empty());
        // The sensitivity defaults to the baseline even when nothing is set —
        // Rust's `f64::default()` is 0.0, and a 0 would floor to 0.1 (ten times
        // too sensitive) in `staleness_sensitivity_safe`.
        assert_eq!(parsed.staleness_sensitivity, 1.0);
    }

    #[test]
    fn staleness_sensitivity_is_parsed_clamped_and_lenient() {
        let parsed = section(json!({ "git": { "stalenessSensitivity": 2 } }));
        assert_eq!(parsed.staleness_sensitivity, 2.0);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert!(
            parsed.uninterpreted.is_empty(),
            "{:?}",
            parsed.uninterpreted
        );

        // Clamped to [0.1, 10].
        let parsed = section(json!({ "git": { "stalenessSensitivity": 500 } }));
        assert_eq!(parsed.staleness_sensitivity, 10.0);
        let parsed = section(json!({ "git": { "stalenessSensitivity": 0 } }));
        assert_eq!(parsed.staleness_sensitivity, 0.1);

        // A non-number is a problem and keeps the default — never a load error.
        let parsed = section(json!({ "git": { "stalenessSensitivity": "fast" } }));
        assert_eq!(parsed.staleness_sensitivity, 1.0);
        assert!(
            parsed
                .problems
                .iter()
                .any(|p| p.location == "ide.git.stalenessSensitivity"),
            "{:?}",
            parsed.problems
        );

        // `ide.git` itself must be an object; a scalar is a problem.
        let parsed = section(json!({ "git": 3 }));
        assert_eq!(parsed.staleness_sensitivity, 1.0);
        assert!(
            parsed.problems.iter().any(|p| p.location == "ide.git"),
            "{:?}",
            parsed.problems
        );

        // An unknown key under `git` stays reserved (reported, not dropped into
        // the interpreted set) and is prefixed so lint names it under `git`.
        let parsed = section(json!({ "git": { "autoUpdate": true } }));
        assert_eq!(parsed.uninterpreted, vec!["git.autoUpdate"]);
    }

    #[test]
    fn unknown_keys_stay_uninterpreted_rather_than_becoming_problems() {
        let parsed = section(json!({
            "my-extension": { "title": "Mine" },
            "another": 1,
        }));
        assert_eq!(parsed.uninterpreted, vec!["another", "my-extension"]);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
    }

    #[test]
    fn quicklinks_parse_and_reject_non_http_urls() {
        let parsed = section(json!({
            "quicklinks": [
                { "label": "Staging", "url": "https://staging.example.com" },
                { "label": "Editor", "url": "vscode://file/tmp" },
                { "label": "", "url": "https://example.com" },
                { "url": "https://example.com" },
                "nope",
            ]
        }));
        assert_eq!(
            parsed.quicklinks,
            vec![Quicklink {
                label: "Staging".to_owned(),
                url: "https://staging.example.com".to_owned(),
            }]
        );
        let locations: Vec<&str> = parsed
            .problems
            .iter()
            .map(|p| p.location.as_str())
            .collect();
        assert_eq!(
            locations,
            vec![
                "ide.quicklinks[1].url",
                "ide.quicklinks[2].label",
                "ide.quicklinks[3]",
                "ide.quicklinks[4]",
            ]
        );
    }

    #[test]
    fn a_wrong_typed_section_is_ignored_whole_and_reported() {
        let parsed = section(json!({ "quicklinks": { "label": "x" } }));
        assert!(parsed.quicklinks.is_empty());
        assert_eq!(parsed.problems.len(), 1);
        assert_eq!(parsed.problems[0].location, "ide.quicklinks");
    }

    #[test]
    fn permissions_normalise_the_origin() {
        let parsed = section(json!({
            "permissions": [
                { "origin": "http://LOCALHOST:*", "allow": ["camera", "camera"] },
                { "origin": "https://staging.example.com", "allow": ["geolocation"] },
                { "origin": "http://127.0.0.1:3000", "deny": ["display-capture"] },
            ]
        }));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let origins: Vec<(&str, &str, Option<u16>)> = parsed
            .permissions
            .iter()
            .map(|r| {
                (
                    r.origin.scheme.as_str(),
                    r.origin.host.as_str(),
                    r.origin.port,
                )
            })
            .collect();
        assert_eq!(
            origins,
            vec![
                ("http", "localhost", None),
                ("https", "staging.example.com", Some(443)),
                ("http", "127.0.0.1", Some(3000)),
            ]
        );
        // Duplicates collapse rather than being reported — repeating an id is
        // harmless and saying so would be noise.
        assert_eq!(parsed.permissions[0].allow, vec!["camera"]);
        assert_eq!(parsed.permissions[2].deny, vec!["display-capture"]);
    }

    #[test]
    fn an_omitted_port_is_the_default_port_not_any_port() {
        let parsed = section(json!({
            "permissions": [{ "origin": "http://example.com", "allow": ["camera"] }]
        }));
        assert_eq!(parsed.permissions[0].origin.port, Some(80));
    }

    #[test]
    fn an_ipv6_origin_keeps_its_brackets() {
        let parsed = section(json!({
            "permissions": [{ "origin": "http://[::1]:5173", "allow": ["camera"] }]
        }));
        assert_eq!(parsed.permissions[0].origin.host, "[::1]");
        assert_eq!(parsed.permissions[0].origin.port, Some(5173));
    }

    /// The case an exact host cannot express: veld's own URLs carry the run name
    /// in the hostname (`{service}.{run}.{project}.localhost`), and run names come
    /// from the worktree folder, the branch or `--name`.
    #[test]
    fn a_leading_wildcard_is_stripped_and_flagged() {
        let parsed = section(json!({
            "permissions": [{ "origin": "https://*.veld.localhost", "allow": ["notifications"] }]
        }));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let origin = &parsed.permissions[0].origin;
        assert!(origin.wildcard);
        assert_eq!(origin.host, "veld.localhost");
        // `raw` keeps the `*.` — it is what the permission panel shows the user.
        assert_eq!(origin.raw, "https://*.veld.localhost");
    }

    #[test]
    fn a_wildcard_over_a_whole_tld_is_refused() {
        for origin in ["https://*.com", "https://*.dev", "http://*."] {
            let parsed = section(json!({
                "permissions": [{ "origin": origin, "allow": ["camera"] }]
            }));
            assert!(parsed.permissions.is_empty(), "{origin} must be refused");
        }
        // …but the loopback TLD is fine: RFC 6761 pins `.localhost` to the local
        // machine, so it cannot name anything on the network.
        let ok = section(json!({
            "permissions": [{ "origin": "http://*.localhost", "allow": ["camera"] }]
        }));
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        assert!(ok.permissions[0].origin.wildcard);
    }

    #[test]
    fn a_wildcard_anywhere_but_the_leading_label_is_refused() {
        for origin in [
            "https://web*.veld.localhost",
            "https://*.veld.*",
            "https://a.*.b.com",
            "https://[*::1]:80",
        ] {
            let parsed = section(json!({
                "permissions": [{ "origin": origin, "allow": ["camera"] }]
            }));
            assert!(parsed.permissions.is_empty(), "{origin} must be refused");
        }
    }

    #[test]
    fn a_bad_rule_is_dropped_whole_rather_than_partially_applied() {
        let parsed = section(json!({
            "permissions": [
                { "origin": "localhost:3000", "allow": ["camera"] },
                { "origin": "http://evil.com@localhost", "allow": ["camera"] },
                { "origin": "file:///tmp", "allow": ["camera"] },
                { "origin": "http://localhost:99999", "allow": ["camera"] },
                { "origin": "http://localhost:3000/", "allow": ["camera"] },
                { "origin": "http://localhost:3000" },
            ]
        }));
        assert!(
            parsed.permissions.is_empty(),
            "nothing may be granted: {:?}",
            parsed.permissions
        );
        assert_eq!(parsed.problems.len(), 6);
    }

    #[test]
    fn an_unknown_permission_id_is_dropped_and_named() {
        let parsed = section(json!({
            "permissions": [{ "origin": "http://localhost:*", "allow": ["mic", "camera"] }]
        }));
        assert_eq!(parsed.permissions[0].allow, vec!["camera"]);
        assert_eq!(parsed.problems.len(), 1);
        assert!(
            parsed.problems[0].message.contains("microphone"),
            "the message should list the real ids: {}",
            parsed.problems[0].message
        );
    }

    /// The JSON schema is hand-maintained, so the id list can drift away from the
    /// parser and nothing would notice: the schema would accept a permission
    /// `veld lint` then reports as unknown, or refuse one that works.
    ///
    /// The desktop app's own table is checked against the *schema* on its side
    /// (`desktop/src/permissions.test.js`), which makes this the single link that
    /// ties all three together.
    #[test]
    fn the_schema_enum_matches_the_parser() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("schema/v3/veld.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("schema is readable"))
                .expect("schema is valid JSON");
        let ids: Vec<&str> = schema["$defs"]["permissionId"]["enum"]
            .as_array()
            .expect("$defs.permissionId.enum must exist")
            .iter()
            .map(|v| v.as_str().expect("ids are strings"))
            .collect();
        assert_eq!(ids, PERMISSION_IDS.to_vec());
    }

    /// The quicklink scheme guard and a config-granted `open-external` are the
    /// same hazard — a config file choosing what the OS launches — so they cannot
    /// both be policy.
    #[test]
    fn open_external_can_be_denied_by_a_project_but_never_granted() {
        let granted = section(json!({
            "permissions": [{ "origin": "http://localhost:*", "allow": ["open-external", "camera"] }]
        }));
        assert_eq!(granted.permissions[0].allow, vec!["camera"]);
        assert_eq!(granted.problems.len(), 1);
        assert_eq!(granted.problems[0].location, "ide.permissions[0].allow[0]");

        let denied = section(json!({
            "permissions": [{ "origin": "http://localhost:*", "deny": ["open-external"] }]
        }));
        assert!(denied.problems.is_empty(), "{:?}", denied.problems);
        assert_eq!(denied.permissions[0].deny, vec!["open-external"]);
    }

    /// The JS matcher runs origins through `new URL()`, which *compresses* an
    /// IPv6 literal. A rule that kept the expanded form could never match.
    #[test]
    fn an_ipv6_host_is_canonicalised_the_way_the_matcher_will_see_it() {
        let parsed = section(json!({
            "permissions": [{ "origin": "http://[0:0:0:0:0:0:0:1]:5173", "allow": ["camera"] }]
        }));
        assert_eq!(parsed.permissions[0].origin.host, "[::1]");
    }

    /// Host forms where this parser and the JavaScript matcher would normalise
    /// differently. Each one used to be stored and then silently match
    /// nothing; a rule that cannot fire has to be a lint message, not a mystery.
    #[test]
    fn a_host_the_matcher_would_spell_differently_is_refused_or_normalised() {
        let one =
            |raw: &str| section(json!({ "permissions": [{ "origin": raw, "allow": ["camera"] }] }));

        // Trailing dot: normalised, because the matcher strips it too. Left alone
        // it also made `is_local_origin` call loopback "not this machine".
        let dotted = one("http://localhost.:3000");
        assert!(dotted.problems.is_empty(), "{:?}", dotted.problems);
        assert_eq!(dotted.permissions[0].origin.host, "localhost");
        assert!(is_local_origin(&dotted.permissions[0].origin));

        // IPv4-mapped IPv6: Rust prints `::ffff:127.0.0.1`, Chromium prints
        // `::ffff:7f00:1`. Refused rather than written out unmatchable.
        let mapped = one("http://[::ffff:127.0.0.1]:3000");
        assert!(mapped.permissions.is_empty());
        assert!(mapped.problems[0].message.contains("plain IPv4 form"));

        // A Unicode host: the matcher punycodes it, this parser does not.
        let unicode = one("https://bücher.example");
        assert!(unicode.permissions.is_empty());
        assert!(unicode.problems[0].message.contains("punycode"));
        // …and the punycode spelling is accepted.
        assert!(one("https://xn--bcher-kva.example").problems.is_empty());

        // IPv4 shorthand and alternate radix: every URL parser folds these to
        // 127.0.0.1, so a rule storing the literal could never fire — and
        // `is_local_origin` would call the author's own loopback grant remote.
        for raw in [
            "http://127.1:3000",
            "http://2130706433:3000",
            "http://0177.0.0.1:3000",
            "http://127.0.0.01:3000",
        ] {
            let parsed = one(raw);
            assert!(parsed.permissions.is_empty(), "{raw} must be refused");
            assert!(
                parsed.problems[0]
                    .message
                    .contains("four plain decimal octets"),
                "{raw}: {}",
                parsed.problems[0].message
            );
        }
        // The canonical form is fine, and a hostname that merely ends in digits
        // is not an address.
        assert!(one("http://127.0.0.1:3000").problems.is_empty());
        assert!(one("https://api2.example.com").problems.is_empty());
    }

    #[test]
    fn local_origins_are_recognised_whatever_they_are_spelled_as() {
        let local = |raw: &str| {
            let parsed =
                section(json!({ "permissions": [{ "origin": raw, "allow": ["camera"] }] }));
            is_local_origin(&parsed.permissions[0].origin)
        };
        assert!(local("http://localhost:*"));
        assert!(local("http://website.run.veld.localhost"));
        assert!(local("http://127.0.0.1:3000"));
        assert!(local("http://[::1]:5173"));
        assert!(!local("https://staging.example.com"));
        // Not fooled by a hostname that merely ends in the right letters.
        assert!(!local("https://evil-localhost.com"));
    }

    fn one_pane(value: serde_json::Value) -> IdeSection {
        section(json!({ "panes": [value] }))
    }

    fn extensions(value: serde_json::Value) -> IdeSection {
        section(json!({ "extensions": value }))
    }

    fn one_extension(value: serde_json::Value) -> IdeSection {
        extensions(json!([value]))
    }

    fn status_badge() -> serde_json::Value {
        json!({ "id": "pr", "slot": "topBar", "type": "status", "argv": ["gh", "pr", "view"] })
    }

    fn problem_at(parsed: &IdeSection, suffix: &str) -> bool {
        parsed.problems.iter().any(|p| p.location.ends_with(suffix))
    }

    #[test]
    fn a_status_extension_parses_with_its_defaults() {
        let parsed = one_extension(status_badge());
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let ext = &parsed.extensions[0];
        // An omitted label is the id, matching a pane.
        assert_eq!(ext.label, "pr");
        assert_eq!(ext.slot.as_deref(), Some("topBar"));
        assert_eq!(
            ext.align,
            ExtensionAlign::Start,
            "the project's own cluster"
        );
        assert_eq!(
            ext.when_missing,
            WhenMissing::Hint,
            "an extension teaches by default; silence teaches nothing"
        );
        let ExtensionBody::Status(status) = &ext.body else {
            panic!("expected a status body, got {:?}", ext.body);
        };
        assert_eq!(status.refresh_seconds, DEFAULT_EXTENSION_REFRESH_SECONDS);
        assert_eq!(
            status.open_in,
            OpenIn::System,
            "a badge's link is a provider page the user is already signed into"
        );
        assert_eq!(
            status.display,
            BadgeDisplay::Text,
            "today's rendering, unchanged for a config that says nothing about it"
        );
    }

    #[test]
    fn an_extension_declaring_everything_round_trips() {
        let parsed = one_extension(json!({
            "id": "pr",
            "slot": "topBar",
            "align": "end",
            "type": "status",
            "label": "PR",
            "description": "This branch's pull request",
            "icon": "git-branch",
            "requires_bin": ["gh", "gh"],
            "shell": "gh pr view --json state",
            "refresh_seconds": 120,
            "open_in": "pane",
            "display": "icon",
            "when_missing": "disable",
            "hint": { "text": "install gh", "href": "https://cli.github.com" },
        }));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let ext = &parsed.extensions[0];
        assert_eq!(ext.label, "PR");
        assert_eq!(ext.align, ExtensionAlign::End);
        assert_eq!(ext.when_missing, WhenMissing::Disable);
        // Deduplicated, like a pane's.
        assert_eq!(ext.requires_bin, vec!["gh".to_owned()]);
        assert_eq!(ext.hint.as_ref().unwrap().text, "install gh");
        let ExtensionBody::Status(status) = &ext.body else {
            panic!("expected a status body");
        };
        assert_eq!(status.refresh_seconds, 120);
        assert_eq!(status.open_in, OpenIn::Pane);
        assert_eq!(status.display, BadgeDisplay::Icon);
        assert_eq!(
            ext.command(),
            Some(&crate::config::CommandSpec::Shell(
                "gh pr view --json state".to_owned()
            )),
            "`shell` is accepted here exactly as it is everywhere else"
        );
    }

    #[test]
    fn an_unknown_display_value_is_a_problem() {
        let parsed = one_extension(json!({
            "id": "pr", "slot": "topBar", "type": "status",
            "argv": ["gh", "pr", "view"], "display": "chartreuse",
        }));
        assert!(parsed.extensions.is_empty());
        assert!(problem_at(&parsed, ".display"), "{:?}", parsed.problems);
    }

    #[test]
    fn an_extension_must_name_exactly_one_command() {
        for entry in [
            json!({ "id": "pr", "slot": "topBar", "type": "status" }),
            json!({ "id": "pr", "slot": "topBar", "type": "status", "argv": ["gh"], "shell": "gh" }),
            json!({ "id": "pr", "slot": "topBar", "type": "status", "argv": [] }),
        ] {
            let parsed = one_extension(entry.clone());
            assert!(parsed.extensions.is_empty(), "accepted {entry}");
            assert!(!parsed.problems.is_empty(), "silent about {entry}");
        }
    }

    #[test]
    fn a_variable_an_extension_will_not_have_is_refused() {
        // `pane.*` exists only while a pane launches. An extension runs against a
        // worktree, so this would resolve to nothing at spawn time.
        let parsed = one_extension(json!({
            "id": "pr", "slot": "topBar", "type": "status",
            "argv": ["gh", "--token", "${veld.pane.token}"],
        }));
        assert!(parsed.extensions.is_empty());
        assert!(
            parsed.problems[0].message.contains("extension command"),
            "{:?}",
            parsed.problems
        );

        // The ones it does have are accepted.
        let parsed = one_extension(json!({
            "id": "open", "type": "action", "argv": ["webstorm", "${veld.root}"],
        }));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
    }

    #[test]
    fn only_an_action_may_omit_its_slot() {
        // An action with no slot is the referenced-only shape the menu depends on.
        let parsed =
            one_extension(json!({ "id": "code", "type": "action", "argv": ["code", "."] }));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert!(parsed.extensions[0].slot.is_none());
        assert!(
            parsed.extensions_in_slot("topBar").is_empty(),
            "a slotless extension must not render on its own"
        );

        // A badge with no slot could never be reached at all, so it is reported
        // rather than parked in the config doing nothing.
        let parsed =
            one_extension(json!({ "id": "pr", "type": "status", "argv": ["gh", "pr", "view"] }));
        assert!(parsed.extensions.is_empty());
        assert!(problem_at(&parsed, ".slot"), "{:?}", parsed.problems);
    }

    #[test]
    fn a_menu_resolves_its_items_against_the_declarations() {
        let parsed = extensions(json!([
            // The menu references a member declared *below* it, which is why the
            // check cannot live inside the per-entry parse.
            { "id": "open-in", "slot": "topBar", "type": "menu", "items": ["code", "code"] },
            { "id": "code", "type": "action", "argv": ["code", "${veld.root}"] },
        ]));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let ExtensionBody::Menu(menu) = &parsed.extension("open-in").unwrap().body else {
            panic!("expected a menu");
        };
        assert_eq!(menu.items, vec!["code".to_owned()], "deduplicated");
    }

    #[test]
    fn a_menu_item_that_is_not_a_declared_action_is_dropped() {
        // Three ways to be wrong: absent, the wrong type, and a menu of menus.
        let parsed = extensions(json!([
            { "id": "menu-a", "slot": "topBar", "type": "menu", "items": ["ghost", "code"] },
            { "id": "menu-b", "slot": "topBar", "type": "menu", "items": ["pr"] },
            { "id": "menu-c", "slot": "topBar", "type": "menu", "items": ["menu-a"] },
            { "id": "code", "type": "action", "argv": ["code", "${veld.root}"] },
            status_badge(),
        ]));
        let ExtensionBody::Menu(menu) = &parsed.extension("menu-a").unwrap().body else {
            panic!("expected a menu");
        };
        assert_eq!(menu.items, vec!["code".to_owned()], "the ghost is gone");
        // A menu left with nothing usable is dropped whole — an empty popover is
        // worse than an absent control.
        assert!(
            parsed.extension("menu-b").is_none(),
            "a badge is not clickable"
        );
        assert!(
            parsed.extension("menu-c").is_none(),
            "no menus inside menus"
        );
        assert_eq!(
            parsed.problems.len(),
            5,
            "each drop is reported: {:?}",
            parsed.problems
        );
    }

    #[test]
    fn a_refresh_below_the_floor_is_clamped_and_reported() {
        let parsed = one_extension(json!({
            "id": "pr", "slot": "topBar", "type": "status", "argv": ["gh"], "refresh_seconds": 1,
        }));
        let ExtensionBody::Status(status) = &parsed.extensions[0].body else {
            panic!("expected a status body");
        };
        assert_eq!(status.refresh_seconds, MIN_EXTENSION_REFRESH_SECONDS);
        assert!(
            problem_at(&parsed, ".refresh_seconds"),
            "an effective value that is not what was written must be said out loud"
        );
    }

    #[test]
    fn more_extensions_than_the_cap_keeps_the_first_and_reports_once() {
        let many: Vec<serde_json::Value> = (0..MAX_EXTENSIONS_PER_PROJECT + 6)
            .map(|i| json!({ "id": format!("e{i}"), "type": "action", "argv": ["true"] }))
            .collect();
        let parsed = extensions(json!(many));
        assert_eq!(parsed.extensions.len(), MAX_EXTENSIONS_PER_PROJECT);
        assert_eq!(parsed.extensions[0].id, "e0", "the first ones win");
        assert_eq!(
            parsed.problems.len(),
            1,
            "reported once, not once per dropped entry: {:?}",
            parsed.problems
        );
    }

    #[test]
    fn a_duplicate_extension_id_keeps_the_first_and_reports_the_second() {
        let parsed = extensions(json!([
            { "id": "code", "type": "action", "label": "first", "argv": ["code"] },
            { "id": "code", "type": "action", "label": "second", "argv": ["code"] },
        ]));
        assert_eq!(parsed.extensions.len(), 1);
        assert_eq!(parsed.extensions[0].label, "first");
        assert!(problem_at(&parsed, "code\".id") || problem_at(&parsed, "[1].id"));
    }

    #[test]
    fn an_unknown_slot_or_type_is_skipped_and_the_rest_still_applies() {
        // A project written for a newer veld: the entry it does not understand is
        // skipped with a message naming what this version knows, and the entries
        // it does understand still work.
        let parsed = extensions(json!([
            { "id": "later", "slot": "sidebar", "type": "status", "argv": ["true"] },
            { "id": "newer", "slot": "topBar", "type": "sparkline", "argv": ["true"] },
            status_badge(),
        ]));
        assert_eq!(parsed.extensions.len(), 1);
        assert_eq!(parsed.extensions[0].id, "pr");
        assert_eq!(parsed.problems.len(), 2, "{:?}", parsed.problems);
        assert!(
            parsed
                .problems
                .iter()
                .all(|p| p.message.contains("this version")),
            "an author must be able to tell 'too new' from 'typo': {:?}",
            parsed.problems
        );
    }

    #[test]
    fn an_unknown_key_is_reported_against_the_type_that_may_not_have_it() {
        // `refresh_seconds` is real — on a badge. On an action it is a mistake
        // worth naming, which is why the allowlist is per type.
        let parsed = one_extension(json!({
            "id": "code", "type": "action", "argv": ["code"], "refresh_seconds": 60,
        }));
        assert!(
            parsed.problems[0].message.contains("refresh_seconds"),
            "{:?}",
            parsed.problems
        );
        // Still accepted: an unknown key is a warning, never a dropped entry —
        // the same leniency panes have.
        assert_eq!(parsed.extensions.len(), 1);
    }

    #[test]
    fn an_icon_from_a_runtime_value_is_bounded() {
        // A badge's stdout may name an icon, and an emoji is unbounded by nature —
        // but it is rendered as text into a 42px bar, so a 20KB "emoji" would
        // destroy the bar. Refused rather than truncated: half a ZWJ sequence is a
        // different glyph.
        assert_eq!(
            parse_icon_name("🦊"),
            Some(PaneIcon::Emoji("🦊".to_owned()))
        );
        // A flag is two scalars plus a joiner; a family is more. These must pass.
        for ok in ["🏳️\u{200d}🌈", "👩\u{200d}💻"] {
            assert!(parse_icon_name(ok).is_some(), "{ok:?} is one glyph");
        }
        assert_eq!(parse_icon_name(&"好".repeat(200)), None);
        assert_eq!(parse_icon_name("not-an-icon-name"), None);
        assert_eq!(
            parse_icon_name("code"),
            Some(PaneIcon::Name("code".to_owned()))
        );
    }

    #[test]
    fn a_hint_href_may_only_be_a_web_url() {
        let parsed = one_extension(json!({
            "id": "pr", "slot": "topBar", "type": "status", "argv": ["gh"],
            "hint": { "text": "install it", "href": "vscode://install" },
        }));
        assert!(parsed.extensions.is_empty());
        assert!(problem_at(&parsed, ".hint.href"), "{:?}", parsed.problems);
    }

    #[test]
    fn a_wrong_typed_extensions_key_is_ignored_whole_and_reported() {
        let parsed = extensions(json!({ "pr": {} }));
        assert!(parsed.extensions.is_empty());
        assert!(problem_at(&parsed, "ide.extensions"));
    }

    /// The extension key lists, slot set and type set are hand-maintained in two
    /// places — here and the JSON schema — and nothing but this ties them
    /// together. A drifted schema red-squiggles a key `veld lint` accepts, or
    /// accepts one the parser will report, and both failures are silent.
    #[test]
    fn the_schema_extension_branches_match_the_parser() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("schema/v3/veld.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("schema is readable"))
                .expect("schema is valid JSON");
        let def = &schema["$defs"]["extension"];

        for (index, kind) in ["status", "action", "menu"].iter().enumerate() {
            let arm = &def["allOf"][index];
            assert_eq!(
                arm["if"]["properties"]["type"]["const"],
                json!(kind),
                "the branches must stay in this order for the indexing below"
            );
            let branch = &arm["then"];
            assert_eq!(
                branch["additionalProperties"],
                json!(false),
                "the key check below only means anything while the branch is closed"
            );
            let mut keys: Vec<&str> = branch["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("$defs.extension {kind} branch must list properties"))
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, extension_keys(kind), "{kind} branch keys");
        }

        // The three enums the parser rejects values outside of.
        let props = &def["allOf"][0]["then"]["properties"];
        for (field, expected) in [
            ("slot", EXTENSION_SLOTS),
            ("align", EXTENSION_ALIGNS),
            ("when_missing", EXTENSION_WHEN_MISSING),
            ("open_in", EXTENSION_OPEN_IN),
            ("display", EXTENSION_DISPLAY),
        ] {
            let mut values: Vec<&str> = props[field]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{field} must be an enum in the schema"))
                .iter()
                .map(|v| v.as_str().expect("enum values are strings"))
                .collect();
            values.sort_unstable();
            assert_eq!(values, expected.to_vec(), "{field} enum");
        }
        let mut types: Vec<&str> = props["type"]["enum"]
            .as_array()
            .expect("type must be an enum")
            .iter()
            .map(|v| v.as_str().expect("enum values are strings"))
            .collect();
        types.sort_unstable();
        assert_eq!(types, vec!["action", "menu", "status"]);

        // The two numeric bounds veld owns rather than the repo.
        assert_eq!(
            props["refresh_seconds"]["minimum"],
            json!(MIN_EXTENSION_REFRESH_SECONDS)
        );
        assert_eq!(
            props["refresh_seconds"]["default"],
            json!(DEFAULT_EXTENSION_REFRESH_SECONDS)
        );
        assert_eq!(
            schema["properties"]["ide"]["properties"]["extensions"]["maxItems"],
            json!(MAX_EXTENSIONS_PER_PROJECT)
        );
    }

    #[test]
    fn extensions_is_no_longer_reported_as_uninterpreted() {
        let parsed = one_extension(status_badge());
        assert!(
            parsed.uninterpreted.is_empty(),
            "F8 must stop naming a key this version renders: {:?}",
            parsed.uninterpreted
        );
        assert!(!parsed.is_empty(), "a section with extensions is not empty");
    }

    #[test]
    fn a_terminal_pane_parses_with_its_defaults() {
        let parsed = one_pane(json!({
            "id": "claude",
            "type": "terminal",
            "argv": ["claude", "--session-id", "${veld.pane.token}"],
        }));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let pane = &parsed.panes[0];
        // An omitted label is the id, not an empty tab.
        assert_eq!(pane.label, "claude");
        assert!(pane.icon.is_none());
        assert!(pane.requires_bin.is_empty());
        let PaneBody::Terminal(terminal) = &pane.body;
        assert!(terminal.resume.is_none());
        assert!(!terminal.auto_resume, "auto_resume must default to false");
        assert!(
            !terminal.allow_terminal_renaming,
            "a config pane must not let its process rename the tab by default"
        );
    }

    #[test]
    fn allow_terminal_renaming_round_trips_and_is_fail_closed() {
        let parsed = one_pane(json!({ "id": "claude", "type": "terminal", "argv": ["claude"] }));
        let PaneBody::Terminal(terminal) = &parsed.panes[0].body;
        assert!(!terminal.allow_terminal_renaming);

        let parsed = one_pane(
            json!({ "id": "claude", "type": "terminal", "argv": ["claude"], "allow_terminal_renaming": true }),
        );
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let PaneBody::Terminal(terminal) = &parsed.panes[0].body;
        assert!(terminal.allow_terminal_renaming);

        // A non-boolean is a problem, not a silent default.
        let parsed = one_pane(
            json!({ "id": "claude", "type": "terminal", "argv": ["claude"], "allow_terminal_renaming": "yes" }),
        );
        assert_eq!(parsed.panes.len(), 0);
        assert!(
            parsed
                .problems
                .iter()
                .any(|p| p.location.ends_with("allow_terminal_renaming")),
            "{:?}",
            parsed.problems
        );
    }

    #[test]
    fn a_pane_declaring_everything_round_trips() {
        let parsed = one_pane(json!({
            "id": "claude",
            "type": "terminal",
            "label": "Claude",
            "description": "Claude Code in this worktree",
            "icon": "sparkles",
            "requires_bin": ["claude", "claude"],
            "argv": ["claude", "--session-id", "${veld.pane.token}"],
            "resume": { "argv": ["claude", "--resume", "${veld.pane.token}"] },
            "auto_resume": true,
        }));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        let pane = &parsed.panes[0];
        assert_eq!(pane.label, "Claude");
        assert_eq!(pane.icon, Some(PaneIcon::Name("sparkles".to_owned())));
        // Duplicates collapse, the way permission ids do.
        assert_eq!(pane.requires_bin, vec!["claude"]);
        let PaneBody::Terminal(terminal) = &pane.body;
        assert!(terminal.auto_resume);
        assert_eq!(
            terminal.resume,
            Some(crate::config::CommandSpec::Argv(vec![
                "claude".to_owned(),
                "--resume".to_owned(),
                "${veld.pane.token}".to_owned(),
            ]))
        );
    }

    /// A config written for a newer veld must cost the author the one pane it
    /// names, never the whole section — otherwise upgrading the config breaks
    /// every older Desktop that opens the repo.
    #[test]
    fn an_unknown_pane_type_is_skipped_without_taking_the_block_down() {
        let parsed = section(json!({
            "panes": [
                { "id": "future", "type": "webview", "url": "https://example.com" },
                { "id": "claude", "type": "terminal", "argv": ["claude"] },
            ]
        }));
        assert_eq!(parsed.panes.len(), 1);
        assert_eq!(parsed.panes[0].id, "claude");
        assert_eq!(parsed.problems.len(), 1);
        assert_eq!(parsed.problems[0].location, "ide.panes[0].type");
        assert!(
            parsed.problems[0].message.contains("this version"),
            "the message should point at the veld version, not at a typo: {}",
            parsed.problems[0].message
        );
    }

    #[test]
    fn a_pane_must_name_exactly_one_command() {
        let neither = one_pane(json!({ "id": "a", "type": "terminal" }));
        assert!(neither.panes.is_empty());
        assert!(neither.problems[0].message.contains("`argv` or `shell`"));

        let both = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["x"], "shell": "x",
        }));
        assert!(both.panes.is_empty());
        assert!(both.problems[0].message.contains("exactly one"));

        let empty = one_pane(json!({ "id": "a", "type": "terminal", "argv": [] }));
        assert!(empty.panes.is_empty());
        assert!(empty.problems[0].message.contains("runs nothing"));
    }

    /// A pane command resolves against a much smaller variable scope than a
    /// node's, and an unresolvable reference means the pane never starts. The
    /// author should learn that from `veld lint`, not from a dead tab.
    #[test]
    fn a_variable_a_pane_will_not_have_is_refused() {
        for argv in [
            json!(["claude", "--port", "${veld.port}"]),
            json!(["claude", "${output.URL}"]),
            json!(["claude", "${nodes.web.url}"]),
            json!(["claude", "${param.FOO}"]),
        ] {
            let parsed = one_pane(json!({ "id": "a", "type": "terminal", "argv": argv }));
            assert!(parsed.panes.is_empty(), "{argv} must be refused");
            assert!(
                parsed.problems[0].message.contains("${veld.pane.token}"),
                "the message should list what a pane may use: {}",
                parsed.problems[0].message
            );
        }
        // …and the pane scope itself resolves, in both command forms.
        let ok = one_pane(json!({
            "id": "a",
            "type": "terminal",
            "shell": "claude --session-id ${veld.pane.token} # ${veld.worktree} ${veld.branch}",
            "resume": { "argv": ["claude", "-r", "${veld.pane.token}"] },
        }));
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
    }

    /// `SHELL_REFUSED_BUILTINS` only says anything if every name in it is one
    /// `argv` actually accepts in both scopes — a typo'd entry here would make
    /// `check_command_variables` tell an author "refused in `shell`, use
    /// `argv`" for a name `argv` rejects too, which is advice that leads
    /// nowhere.
    #[test]
    fn shell_refused_builtins_is_a_subset_of_both_argv_scopes() {
        for name in SHELL_REFUSED_BUILTINS {
            assert!(
                PANE_BUILTINS.contains(name),
                "{name} missing from PANE_BUILTINS"
            );
            assert!(
                EXTENSION_BUILTINS.contains(name),
                "{name} missing from EXTENSION_BUILTINS"
            );
        }
    }

    /// The whole point of the `argv`/`shell` split: deleting either half of
    /// `check_command_variables`'s enforcement should fail this, not just the
    /// two tests above that only look at the allowlists.
    #[test]
    fn branch_raw_is_argv_only_in_both_scopes() {
        let shell_pane = one_pane(json!({
            "id": "a", "type": "terminal", "shell": "gh pr view ${veld.branch_raw}",
        }));
        assert!(shell_pane.panes.is_empty(), "shell must refuse branch_raw");
        assert!(
            shell_pane.problems[0]
                .message
                .contains("refused in `shell`"),
            "should get the specific refusal, not the generic one: {:?}",
            shell_pane.problems
        );
        assert!(
            !shell_pane.problems[0].message.contains("may use"),
            "the specific refusal should not also suggest an allowed-variable list: {:?}",
            shell_pane.problems
        );

        let argv_pane = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["gh", "pr", "view", "${veld.branch_raw}"],
        }));
        assert!(argv_pane.problems.is_empty(), "{:?}", argv_pane.problems);

        let shell_ext = one_extension(json!({
            "id": "pr", "slot": "topBar", "type": "status",
            "shell": "gh pr view ${veld.branch_raw}",
        }));
        assert!(
            shell_ext.extensions.is_empty(),
            "shell must refuse branch_raw"
        );
        assert!(
            shell_ext.problems[0].message.contains("refused in `shell`"),
            "{:?}",
            shell_ext.problems
        );

        let argv_ext = one_extension(json!({
            "id": "pr", "slot": "topBar", "type": "status",
            "argv": ["gh", "pr", "view", "${veld.branch_raw}"],
        }));
        assert!(argv_ext.problems.is_empty(), "{:?}", argv_ext.problems);

        // A `shell` command's generic "not available" message (for some other,
        // entirely unknown name) must not dangle `branch_raw` as an option it
        // cannot actually use.
        let shell_unknown = one_pane(json!({
            "id": "a", "type": "terminal", "shell": "echo ${veld.nonsense}",
        }));
        assert!(
            !shell_unknown.problems[0].message.contains("branch_raw"),
            "a shell command's suggestion list must not include an argv-only name: {}",
            shell_unknown.problems[0].message
        );
    }

    /// The same scope check has to reach inside `resume`, which is the command
    /// that runs least often and would therefore fail latest.
    #[test]
    fn a_bad_resume_command_is_reported_against_resume() {
        let parsed = one_pane(json!({
            "id": "a",
            "type": "terminal",
            "argv": ["claude"],
            "resume": { "argv": ["claude", "-r", "${veld.run}"] },
        }));
        assert!(parsed.panes.is_empty());
        assert_eq!(parsed.problems[0].location, "ide.panes[0].resume");
    }

    #[test]
    fn close_on_exit_defaults_to_true_and_is_settable() {
        let default = one_pane(json!({ "id": "a", "type": "terminal", "argv": ["x"] }));
        let PaneBody::Terminal(terminal) = &default.panes[0].body;
        assert!(
            terminal.close_on_exit,
            "a pane whose command exits cleanly should tidy itself up by default"
        );

        let off = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["x"], "close_on_exit": false,
        }));
        let PaneBody::Terminal(terminal) = &off.panes[0].body;
        assert!(!terminal.close_on_exit);

        let wrong = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["x"], "close_on_exit": "yes",
        }));
        assert!(wrong.panes.is_empty());
        assert_eq!(wrong.problems[0].location, "ide.panes[0].close_on_exit");
    }

    #[test]
    fn auto_resume_without_a_resume_command_is_downgraded_not_obeyed() {
        let parsed = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["claude"], "auto_resume": true,
        }));
        // The pane survives — it is a fine pane, the flag just cannot mean
        // anything — but nothing may auto-run.
        let PaneBody::Terminal(terminal) = &parsed.panes[0].body;
        assert!(!terminal.auto_resume);
        assert_eq!(parsed.problems[0].location, "ide.panes[0].auto_resume");
    }

    #[test]
    fn icons_are_a_name_from_the_allowlist_or_an_emoji() {
        let named = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["x"], "icon": "robot",
        }));
        assert_eq!(
            named.panes[0].icon,
            Some(PaneIcon::Name("robot".to_owned()))
        );

        let emoji = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["x"], "icon": "🤖",
        }));
        assert_eq!(emoji.panes[0].icon, Some(PaneIcon::Emoji("🤖".to_owned())));

        // A misspelled name is reported rather than rendered as literal text —
        // which is the whole reason ASCII means "name".
        let typo = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["x"], "icon": "sparkle",
        }));
        assert!(typo.panes.is_empty());
        assert!(typo.problems[0].message.contains("sparkles"));
    }

    #[test]
    fn requires_bin_takes_names_not_paths() {
        let parsed = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["x"],
            "requires_bin": ["/opt/homebrew/bin/claude"],
        }));
        assert!(parsed.panes.is_empty());
        assert!(parsed.problems[0].message.contains("not a path"));
    }

    /// The schema's terminal branch is `additionalProperties: false`, so an
    /// editor already rejects these. `veld lint` accepting them meant a typo
    /// silently took a default — and both reachable defaults change behaviour.
    #[test]
    fn an_unknown_key_inside_a_pane_is_named_rather_than_ignored() {
        let parsed = one_pane(json!({
            "id": "a", "type": "terminal", "argv": ["x"],
            "autoresume": true, "requiresBin": ["claude"],
        }));
        // The pane still works — an unknown key is a warning, not a reason to
        // drop a pane that is otherwise fine.
        assert_eq!(parsed.panes.len(), 1);
        assert_eq!(parsed.problems.len(), 1);
        assert_eq!(parsed.problems[0].location, "ide.panes[0]");
        assert!(
            parsed.problems[0].message.contains("\"autoresume\""),
            "{:?}",
            parsed.problems
        );
        assert!(
            parsed.problems[0].message.contains("\"requiresBin\""),
            "{:?}",
            parsed.problems
        );
    }

    /// The documented icon list is the fourth copy of this set, and was the only one
    /// with nothing checking it.
    ///
    /// It went stale immediately: the list stayed at the original 32 names while the
    /// allowlist grew to 63, and `docs/configuration.md` points `ide.extensions`'
    /// own `icon` field at that section — so its worked examples used
    /// `external-link` and `git-pull-request`, neither of which the list it cites
    /// contained. A reader checking the example against the allowlist in the same
    /// document found a contradiction, and nothing failed.
    #[test]
    fn the_documented_icon_list_matches_the_allowlist() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("docs/configuration.md");
        let doc = std::fs::read_to_string(&path).expect("configuration.md is readable");
        let marker = "the same allowlist [`ide.extensions`](#ideextensions) draws on";
        let start = doc
            .find(marker)
            .unwrap_or_else(|| panic!("the pane-icon paragraph moved; update this test"));
        // The `·`-separated run that follows, up to the next blank line.
        let list_start = doc[start..]
            .find("\n\n`")
            .map(|o| start + o + 2)
            .expect("a list follows the paragraph");
        let list_end = doc[list_start..]
            .find("\n\n")
            .map(|o| list_start + o)
            .expect("the list ends");
        let mut documented: Vec<&str> = doc[list_start..list_end]
            .split('·')
            .map(|n| n.trim().trim_matches('`'))
            .filter(|n| !n.is_empty())
            .collect();
        documented.sort_unstable();
        assert_eq!(
            documented,
            PANE_ICON_NAMES.to_vec(),
            "docs/configuration.md's icon list has drifted from PANE_ICON_NAMES"
        );
    }

    /// The key list and the schema's terminal branch are two hand-maintained
    /// copies of one set; nothing but this ties them together.
    #[test]
    fn the_schema_terminal_pane_keys_match_the_parser() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("schema/v3/veld.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("schema is readable"))
                .expect("schema is valid JSON");
        let branch = &schema["$defs"]["pane"]["allOf"][0]["then"];
        assert_eq!(
            branch["additionalProperties"],
            serde_json::json!(false),
            "the key check below only means anything while the schema is closed"
        );
        let mut keys: Vec<&str> = branch["properties"]
            .as_object()
            .expect("$defs.pane terminal branch must list properties")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, TERMINAL_PANE_KEYS.to_vec());
    }

    #[test]
    fn a_duplicate_pane_id_keeps_the_first_and_reports_the_second() {
        let parsed = section(json!({
            "panes": [
                { "id": "a", "type": "terminal", "label": "First", "argv": ["x"] },
                { "id": "a", "type": "terminal", "label": "Second", "argv": ["y"] },
            ]
        }));
        assert_eq!(parsed.panes.len(), 1);
        assert_eq!(parsed.panes[0].label, "First");
        assert_eq!(parsed.problems[0].location, "ide.panes[1].id");
    }

    #[test]
    fn a_pane_missing_its_required_keys_is_reported_not_silently_dropped() {
        let no_id = one_pane(json!({ "type": "terminal", "argv": ["x"] }));
        assert_eq!(no_id.problems[0].location, "ide.panes[0].id");

        let no_type = one_pane(json!({ "id": "a", "argv": ["x"] }));
        assert_eq!(no_type.problems[0].location, "ide.panes[0].type");

        let bad_id = one_pane(json!({ "id": "a b", "type": "terminal", "argv": ["x"] }));
        assert_eq!(bad_id.problems[0].location, "ide.panes[0].id");
    }

    #[test]
    fn a_wrong_typed_panes_key_is_ignored_whole_and_reported() {
        let parsed = section(json!({ "panes": { "id": "a" } }));
        assert!(parsed.panes.is_empty());
        assert_eq!(parsed.problems[0].location, "ide.panes");
    }

    /// `panes` moved out of `uninterpreted` when it gained a meaning; F8 must
    /// stop naming it or `veld lint` tells authors their panes are not rendered.
    #[test]
    fn panes_is_no_longer_reported_as_uninterpreted() {
        let parsed = one_pane(json!({ "id": "a", "type": "terminal", "argv": ["x"] }));
        assert!(parsed.uninterpreted.is_empty());
        assert!(!parsed.is_empty());
    }

    /// Same drift gate as the permission ids: the schema's enum is what an
    /// editor autocompletes from, and nothing else ties it to this list.
    #[test]
    fn the_schema_icon_enum_matches_the_allowlist() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("schema/v3/veld.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("schema is readable"))
                .expect("schema is valid JSON");
        let names: Vec<&str> = schema["$defs"]["paneIconName"]["enum"]
            .as_array()
            .expect("$defs.paneIconName.enum must exist")
            .iter()
            .map(|v| v.as_str().expect("names are strings"))
            .collect();
        assert_eq!(names, PANE_ICON_NAMES.to_vec());
    }

    #[test]
    fn pane_icon_names_and_builtins_are_sorted_and_unique() {
        for list in [PANE_ICON_NAMES, PANE_BUILTINS, TERMINAL_PANE_KEYS] {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted, list.to_vec());
        }
    }

    #[test]
    fn permission_ids_are_sorted_and_unique() {
        let mut sorted = PERMISSION_IDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, PERMISSION_IDS.to_vec());
    }

    // -- ide.news --------------------------------------------------------------

    /// A well-formed item. Its `since` is **comfortably in the past**, not "today":
    /// `parse_news` reads the real clock (`today_anywhere`), so a fixture dated on
    /// the day these tests were written would be refused as future-dated on any
    /// machine whose clock reads earlier — a stale container image, or a clock rolled
    /// back to bisect. The failure would then point at the date gate while claiming
    /// to test the cap. Only `a_news_item_dated_in_the_future_is_dropped` should
    /// depend on where "now" is.
    fn news_item(extra: serde_json::Value) -> serde_json::Value {
        let mut item = json!({
            "id": "build-moved",
            "since": "2020-01-02",
            "eyebrow": "Heads up",
            "headline": "Run the suite with one command",
            "body": "The test script moved behind `just test`, so a stale local wrapper is the one thing that will still fail today.",
        });
        let (Some(base), Some(extra)) = (item.as_object_mut(), extra.as_object()) else {
            panic!("both must be objects");
        };
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
        item
    }

    fn one_news(item: serde_json::Value) -> IdeSection {
        section(json!({ "news": [item] }))
    }

    #[test]
    fn a_news_item_takes_the_safe_defaults_for_what_it_omits() {
        let parsed = one_news(news_item(json!({})));
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert_eq!(parsed.news.len(), 1);
        // The glyph is the only optional field. Everything an item says about
        // *who sees it* — the date — is required, so there is no default that can
        // silently change its audience.
        assert_eq!(parsed.news[0].glyph, "inbox");
        assert!(!parsed.is_empty(), "news alone is worth sending to a UI");
    }

    /// The caps are the whole mitigation for an interrupting surface a teammate
    /// can push through, so a breach must not be quietly truncated into a
    /// half-sentence that reads as a veld bug.
    #[test]
    fn a_news_item_over_a_copy_cap_is_dropped_and_named() {
        for (key, max) in [
            ("eyebrow", MAX_NEWS_EYEBROW),
            ("headline", MAX_NEWS_HEADLINE),
            ("body", MAX_NEWS_BODY),
        ] {
            let parsed = one_news(news_item(json!({ key: "x".repeat(max + 1) })));
            assert!(parsed.news.is_empty(), "{key} over cap must not ship");
            assert_eq!(parsed.problems[0].location, format!("ide.news[0].{key}"));
        }
        // Exactly at the cap is fine — the bound is inclusive, and it is measured
        // in characters rather than bytes so an em dash does not cost three.
        let ok = one_news(news_item(
            json!({ "headline": "—".repeat(MAX_NEWS_HEADLINE) }),
        ));
        assert_eq!(ok.news.len(), 1, "{:?}", ok.problems);

        // Present-but-blank is a missing field, not a valid empty card.
        let blank = one_news(news_item(json!({ "eyebrow": "   " })));
        assert!(blank.news.is_empty());
    }

    #[test]
    fn a_news_item_needs_a_plausible_day_and_gets_no_default() {
        for bad in [
            json!("2026-13-04"),
            json!("2026-08-32"),
            json!("26-08-12"),
            json!("2026/08/12"),
            json!("2026-8-1"),
            json!(20260812),
            serde_json::Value::Null,
        ] {
            let mut item = news_item(json!({}));
            item.as_object_mut()
                .unwrap()
                .insert("since".into(), bad.clone());
            let parsed = one_news(item);
            assert!(parsed.news.is_empty(), "{bad} must not ship");
            assert_eq!(parsed.problems[0].location, "ide.news[0].since");
        }
        // A missing key is the same answer as a malformed one: "today" would be
        // wrong the moment the config is read on a different day.
        let mut without = news_item(json!({}));
        without.as_object_mut().unwrap().remove("since");
        assert!(one_news(without).news.is_empty());
    }

    #[test]
    fn the_live_item_cap_keeps_the_newest_and_names_what_it_dropped() {
        // Same date on every entry, so the tie-break is what is under test: the
        // author appended, and appending must never cost you the card you just
        // wrote. If this ever keeps `item-0`, dropping-from-the-end is back.
        let items: Vec<serde_json::Value> = (0..MAX_NEWS_ITEMS + 2)
            .map(|i| news_item(json!({ "id": format!("item-{i}") })))
            .collect();
        let parsed = section(json!({ "news": items }));
        assert_eq!(parsed.news.len(), MAX_NEWS_ITEMS);
        assert_eq!(parsed.news[0].id, "item-2");
        assert_eq!(parsed.news[MAX_NEWS_ITEMS - 1].id, "item-6");
        // One problem for the whole overflow, naming every id the author has to go
        // and delete — not one anonymous complaint per entry.
        assert_eq!(parsed.problems.len(), 1);
        assert_eq!(parsed.problems[0].location, "ide.news");
        assert!(parsed.problems[0].message.contains("\"item-0\""));
        assert!(parsed.problems[0].message.contains("\"item-1\""));
        assert!(parsed.problems[0].message.contains("Retiring"));
    }

    /// **Array position is not chronology.** `ide` arrays concatenate across
    /// `include` files in sorted-filename order, and `docs/configuration.md`
    /// suggests putting news in `veld.d/news.jsonc` — so a file sorting earlier can
    /// put a *newer* card at the front, and an author who prepends does the same. A
    /// cap that dropped the front of the array would then drop the newest card while
    /// reporting it as the oldest.
    #[test]
    fn the_live_item_cap_goes_by_date_not_by_position() {
        // Newest first, oldest last — the exact inverse of the append convention.
        // All in the past, so the future-date gate can never be what fails here.
        let days = [
            "2020-06-12",
            "2020-06-11",
            "2020-06-10",
            "2020-06-09",
            "2020-06-08",
            "2020-01-02",
            "2020-01-01",
        ];
        let items: Vec<serde_json::Value> = days
            .iter()
            .enumerate()
            .map(|(i, day)| news_item(json!({ "id": format!("item-{i}"), "since": day })))
            .collect();
        let parsed = section(json!({ "news": items }));
        assert_eq!(parsed.news.len(), MAX_NEWS_ITEMS);
        // The two January entries go; every August one survives, in the order the
        // merged array had them, because `historyOf` ties-breaks on that order.
        let kept: Vec<&str> = parsed.news.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            kept,
            ["item-0", "item-1", "item-2", "item-3", "item-4"],
            "the cap must drop the oldest by `since`, not the front of the array"
        );
        assert!(parsed.problems[0].message.contains("\"item-5\""));
        assert!(parsed.problems[0].message.contains("\"item-6\""));
    }

    /// The gate's load-bearing half: `since` is the only thing that retires a card,
    /// so a day that has not happened yet is after every arrival, forever. That is
    /// the never-expiring card the `onboarding` kind was deleted to be rid of, and
    /// `2062` for `2026` is one keystroke.
    #[test]
    fn a_news_item_dated_in_the_future_is_dropped() {
        for future in ["2062-08-12", "2099-01-01", "9999-12-31"] {
            let parsed = one_news(news_item(json!({ "since": future })));
            assert!(parsed.news.is_empty(), "{future} must not ship");
            assert_eq!(parsed.problems[0].location, "ide.news[0].since");
            assert!(parsed.problems[0].message.contains("future"));
        }
        // Today itself is fine — a card written and merged the same day is the
        // normal case, and the local day is what the author is working in.
        let today = today_anywhere();
        let parsed = one_news(news_item(json!({ "since": today })));
        assert_eq!(parsed.news.len(), 1, "{:?}", parsed.problems);
    }

    /// The caps bound how much text a card carries; this bounds whether it carries
    /// any. `trim` only removes `White_Space`, so zero-width padding is text by the
    /// cap's reckoning and nothing at all by the reader's.
    #[test]
    fn invisible_copy_is_not_text() {
        for bad in [
            "\u{200b}\u{200b}\u{200b}",
            "\u{feff}",
            "ok\u{202e}reversed",
            "line\u{7}break",
            "\u{00ad}",
        ] {
            let parsed = one_news(news_item(json!({ "eyebrow": bad })));
            assert!(parsed.news.is_empty(), "{bad:?} must not ship");
            assert_eq!(parsed.problems[0].location, "ide.news[0].eyebrow");
        }
        // Ordinary copy with punctuation, an em dash and non-ASCII letters is text.
        let ok = one_news(news_item(json!({ "eyebrow": "Grüße — ok" })));
        assert_eq!(ok.news.len(), 1, "{:?}", ok.problems);
    }

    /// The reason `proj:<hash>:<slug>` is unambiguous, asserted rather than
    /// commented. `NAMESPACE_SEPARATOR` in the bundle's `model.ts` reserves `:`,
    /// and a project id that could contain one would let a repo write an id
    /// indistinguishable from one of Veld's.
    #[test]
    fn a_news_id_can_never_contain_the_namespace_separator() {
        for id in ["a:b", ":", "proj:x:y", "veld:news"] {
            assert!(!valid_news_id(id), "{id} must not be a valid news id");
        }
        for id in ["a", "build-moved", "a1-b2-c3"] {
            assert!(valid_news_id(id), "{id} must be a valid news id");
        }
        // Not kebab-case: capitals, underscores, spaces, doubled or edge dashes.
        for id in [
            "Build-Moved",
            "build_moved",
            "build moved",
            "build--moved",
            "-build",
            "build-",
            "",
        ] {
            assert!(!valid_news_id(id), "{id:?} must not be a valid news id");
        }
        // Bounded so the namespaced form fits the promotions endpoint's ceiling.
        assert!(valid_news_id(&"a".repeat(64)));
        assert!(!valid_news_id(&"a".repeat(65)));
    }

    #[test]
    fn a_duplicate_news_id_keeps_the_first_and_reports_the_second() {
        let parsed = section(json!({
            "news": [
                news_item(json!({ "eyebrow": "First" })),
                news_item(json!({ "eyebrow": "Second" })),
            ]
        }));
        assert_eq!(parsed.news.len(), 1);
        assert_eq!(parsed.news[0].eyebrow, "First");
        assert_eq!(parsed.problems[0].location, "ide.news[1].id");
    }

    #[test]
    fn a_bad_glyph_is_named_rather_than_guessed() {
        let glyph = one_news(news_item(json!({ "glyph": "rocket" })));
        assert!(glyph.news.is_empty());
        assert_eq!(glyph.problems[0].location, "ide.news[0].glyph");
        // The pane icon vocabulary is a different, much larger set, and naming one
        // of its members here must not quietly work.
        assert!(PANE_ICON_NAMES.contains(&"rocket"));
    }

    /// An unknown key reports but **keeps** the card, unlike `ide.permissions`.
    /// Fail-closed exists because guessing at a permission hands out a
    /// capability; a card grants nothing, and dropping one over a stray key is a
    /// change the team never hears about.
    #[test]
    fn an_unknown_news_key_is_reported_but_the_card_still_ships() {
        let parsed = one_news(news_item(json!({ "kind": "onboarding" })));
        assert_eq!(parsed.news.len(), 1);
        assert_eq!(parsed.problems.len(), 1);
        assert_eq!(parsed.problems[0].location, "ide.news[0]");
        // `kind` was a real field until the onboarding kind was removed. A config
        // still carrying one keeps working and is told what to delete, rather than
        // losing its card to a key that used to be valid.
        assert!(parsed.problems[0].message.contains("\"kind\""));
    }

    #[test]
    fn a_wrong_typed_news_key_is_ignored_whole_and_reported() {
        let parsed = section(json!({ "news": { "id": "a" } }));
        assert!(parsed.news.is_empty());
        assert_eq!(parsed.problems.len(), 1);
        assert_eq!(parsed.problems[0].location, "ide.news");
    }

    /// `news` moved out of `uninterpreted` when it gained a meaning; F8 must stop
    /// naming it or `veld lint` tells authors their news is not rendered.
    #[test]
    fn news_is_no_longer_reported_as_uninterpreted() {
        let parsed = one_news(news_item(json!({})));
        assert!(parsed.uninterpreted.is_empty());
    }

    /// Same drift gate as the pane icons and the permission ids. The bundle's half
    /// of it is `it("matches the published schema's glyph set and every cap")`, so
    /// all three — parser, schema, renderer — are pinned to one list.
    #[test]
    fn the_glyph_set_matches_the_published_schema() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("schema/v3/veld.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("schema is readable"))
                .expect("schema is valid JSON");
        let item = &schema["$defs"]["newsItem"];
        assert_eq!(
            item["additionalProperties"],
            serde_json::json!(false),
            "the key check below only means anything while the schema is closed"
        );
        let mut names: Vec<&str> = item["properties"]["glyph"]["enum"]
            .as_array()
            .expect("$defs.newsItem.properties.glyph.enum must exist")
            .iter()
            .map(|v| v.as_str().expect("names are strings"))
            .collect();
        names.sort_unstable();
        assert_eq!(names, NEWS_GLYPHS.to_vec());

        let mut keys: Vec<&str> = item["properties"]
            .as_object()
            .expect("$defs.newsItem must list properties")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, NEWS_ITEM_KEYS.to_vec());

        // **Every cap, not just the key names.** The caps are the mechanism, and
        // widening one in the schema alone fails in the worst direction: the
        // author's editor accepts a headline this parser then drops, so the change
        // the card was announcing never reaches the team, and the only report is a
        // `veld lint` run by whoever thinks to.
        for (key, max) in [
            ("eyebrow", MAX_NEWS_EYEBROW),
            ("headline", MAX_NEWS_HEADLINE),
            ("body", MAX_NEWS_BODY),
        ] {
            assert_eq!(
                item["properties"][key]["maxLength"],
                serde_json::json!(max),
                "schema {key}.maxLength must match this parser's cap"
            );
        }
        assert_eq!(
            item["properties"]["id"]["maxLength"],
            serde_json::json!(64),
            "schema id.maxLength must match valid_news_id"
        );
        // The array cap too, so an editor cannot bless a sixth item the parser will
        // silently retire.
        assert_eq!(
            schema["properties"]["ide"]["properties"]["news"]["maxItems"],
            serde_json::json!(MAX_NEWS_ITEMS),
            "schema ide.news.maxItems must match MAX_NEWS_ITEMS"
        );
        // And the day pattern. `plausible_day` range-checks the month and the day,
        // so a looser schema would green-light `2026-13-45` in the editor and then
        // drop the card at load — the exact inversion of what `NEWS_ITEM_KEYS`
        // exists to prevent. Pinned as a literal rather than executed, because
        // veld-core has no regex dependency and adding one to assert four dates
        // would be the more expensive half of this check.
        assert_eq!(
            item["properties"]["since"]["pattern"],
            serde_json::json!("^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$"),
            "the published pattern must range-check the month and day like plausible_day"
        );
        for bad in ["2026-13-04", "2026-00-10", "2026-08-32", "2026-08-00"] {
            assert!(!plausible_day(bad), "{bad} must not be a plausible day");
        }
    }

    #[test]
    fn news_glyphs_and_keys_are_sorted_and_unique() {
        for list in [NEWS_GLYPHS, NEWS_ITEM_KEYS] {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted, list.to_vec());
        }
    }
}
