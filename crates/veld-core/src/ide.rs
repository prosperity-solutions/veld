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

/// Every icon name a pane may name, in sorted order.
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
    "atom",
    "bolt",
    "book",
    "brain",
    "bug",
    "bulb",
    "chart-line",
    "cloud",
    "code",
    "compass",
    "cpu",
    "database",
    "flask",
    "git-branch",
    "key",
    "map",
    "message-chatbot",
    "notebook",
    "package",
    "player-play",
    "plug",
    "puzzle",
    "refresh",
    "robot",
    "rocket",
    "search",
    "server",
    "shield",
    "sparkles",
    "terminal-2",
    "tool",
    "wand",
];

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
    "pane.id",
    "pane.label",
    "pane.token",
    "project",
    "root",
    "username",
    "worktree",
];

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
    /// (`ide.stalenessSensitivity`).
    ///
    /// A multiplier on the "update main" count pill's severity curve. `1` is the
    /// baseline — a single commit a week old, or fifty commits in a day, are both
    /// at the top of the scale (red); `2` halves both thresholds (a 3.5-day-old
    /// commit or 25 commits read red); `0.5` halves the sensitivity. Clamped to
    /// `[0.1, 10]`. A project that lives on a fast-moving trunk tunes it up; a
    /// project whose worktrees naturally drift tunes it down.
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
#[must_use]
/// The baseline sensitivity for the staleness indicator: `1`.
fn default_staleness_sensitivity() -> f64 {
    1.0
}

/// Parse `ide.stalenessSensitivity` — a positive multiplier, clamped to
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
            location: "ide.stalenessSensitivity".to_owned(),
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
            "quicklinks" => parse_quicklinks(child, &mut section),
            "permissions" => parse_permissions(child, &mut section),
            "panes" => parse_panes(child, &mut section),
            "externalOrigins" => parse_external_origins(child, &mut section),
            "stalenessSensitivity" => parse_staleness_sensitivity(child, &mut section),
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
    check_pane_variables(&spec, location, out)?;
    Some(spec)
}

/// Refuse a pane command that references a variable a pane will not have.
///
/// A pane command is interpolated against a much smaller context than a node's
/// ([`PANE_BUILTINS`]), and an unresolvable reference is not a soft failure at
/// spawn time — the pane simply never starts, with an error the author has to
/// read backwards from. Catching it in `veld lint` is the whole point of the
/// scope being closed.
fn check_pane_variables(
    spec: &crate::config::CommandSpec,
    location: &str,
    out: &mut IdeSection,
) -> Option<()> {
    let parts: Vec<String> = match spec {
        crate::config::CommandSpec::Argv(argv) => argv.clone(),
        crate::config::CommandSpec::Shell(s) => vec![s.clone()],
    };
    for part in &parts {
        for reference in all_references(part) {
            let Some(name) = reference.strip_prefix("veld.") else {
                out.problems.push(IdeProblem {
                    location: location.to_owned(),
                    message: format!(
                        "`${{{reference}}}` is not available in a pane command. A pane may use: {}",
                        pane_variable_list()
                    ),
                });
                return None;
            };
            if !PANE_BUILTINS.contains(&name) {
                out.problems.push(IdeProblem {
                    location: location.to_owned(),
                    message: format!(
                        "`${{veld.{name}}}` is not available in a pane command. A pane may use: {}",
                        pane_variable_list()
                    ),
                });
                return None;
            }
        }
    }
    Some(())
}

fn pane_variable_list() -> String {
    PANE_BUILTINS
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
        let parsed = section(json!({ "stalenessSensitivity": 2 }));
        assert_eq!(parsed.staleness_sensitivity, 2.0);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);

        // Clamped to [0.1, 10].
        let parsed = section(json!({ "stalenessSensitivity": 500 }));
        assert_eq!(parsed.staleness_sensitivity, 10.0);
        let parsed = section(json!({ "stalenessSensitivity": 0 }));
        assert_eq!(parsed.staleness_sensitivity, 0.1);

        // A non-number is a problem and keeps the default — never a load error.
        let parsed = section(json!({ "stalenessSensitivity": "fast" }));
        assert_eq!(parsed.staleness_sensitivity, 1.0);
        assert!(
            parsed
                .problems
                .iter()
                .any(|p| p.location == "ide.stalenessSensitivity"),
            "{:?}",
            parsed.problems
        );
        // And it is interpreted, not parked in `uninterpreted`.
        assert!(parsed.uninterpreted.is_empty());
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
}
