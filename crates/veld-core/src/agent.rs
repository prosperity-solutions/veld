//! Knowing whether a coding agent in a terminal pane is working, waiting on the
//! user, or done.
//!
//! # Why this cannot be read off the byte stream
//!
//! It was measured, not assumed. Claude Code's inline TUI emits **OSC 0** (title),
//! **OSC 8** (hyperlinks), **OSC 9;4** (progress) and **OSC 52** (clipboard) — and
//! nothing that says what state it is in. It does not take the alternate screen (it
//! redraws inline with cursor control), it emits no OSC 133 semantic prompt marks
//! (`anthropics/claude-code#26235`, closed *not-planned*), and it emits no OSC 9
//! notification. So the two signals that would otherwise generalise across tools —
//! a notification sequence, and an alt-screen toggle — both miss the tool that
//! matters most.
//!
//! An agent's working/waiting/finished state is an **application-level fact**. The
//! only honest way to learn it is to be told, which is what this module arranges:
//! a per-tool hook installer, and one generic receiving end.
//!
//! # Told, not inferred — and the authority that makes that stick
//!
//! A state is only as good as what told us, so the inbox ranks its sources
//! (`hook > socket > detected`) and never lets a lower one overwrite a higher. Without
//! that rule the passive fallbacks are worse than useless: an OSC 9 notification arriving
//! after a real `Stop` hook would flip a finished session back to "needs you", and the
//! feature would train the user to ignore the badge.
//!
//! **That rule lives in `ui/src/inbox/inbox.ts`, not here**, because the store it guards
//! is the browser's. This module had a `Source` enum and a `supersedes` method mirroring
//! it, with a test pinning the ordering — and none of it was reachable: the wire carries
//! only `tool` and `state`, and the client attributes the authority itself. A second copy
//! of a rule, with a passing test and no caller, is worse than no copy: changing it
//! changes nothing while looking like it changed something.
//!
//! # What veld does *not* do
//!
//! It does not touch `~/.claude/settings.json`, `~/.claude/settings.local.json`, or
//! any `.claude/` directory in the user's project. The hooks ride an ephemeral
//! `--settings` file, written into this daemon's own shim directory and named after
//! the terminal session. `--settings` **merges** into the settings hierarchy just
//! below managed policy — it does not replace what the user configured — which is
//! the property that makes this safe and the one to re-check if the flag's semantics
//! ever change.
//!
//! # Adding another agent
//!
//! Everything downstream of this module is **already generic** — the daemon endpoint
//! takes a state rather than a vendor payload, the wire carries a tool name, and the
//! browser store, the rail glyph, the pane dot and the notification table all key on
//! [`State`] and never on which tool produced it. So a new tool is an installer plus a
//! mapping, in five edits, and nothing else has to move:
//!
//! 1. A variant on [`AgentTool`] (and its `ALL`, `shim_name`, `as_str`, `injection`).
//!    `shim_name` is the command the wrapper stands in front of.
//! 2. A `<tool>_state(&HookPayload) -> State` beside [`claude_state`]/[`codex_state`],
//!    and an arm in `veld agent-state`'s `match tool` (`crates/veld/src/commands/agent.rs`).
//! 3. A `<tool>_settings_doc`/`<tool>_notify_config` beside [`claude_settings_doc`]/
//!    [`codex_notify_config`], depending on [`Injection`] — see below. `prepare_in` in
//!    `veld-daemon/src/pty/shims.rs` already generates one wrapper per `AgentTool::ALL`,
//!    so the script itself comes for free either way.
//! 4. Whatever [`HookPayload`] is missing for the new tool's schema — every field is
//!    optional and unknown fields are ignored, so adding one cannot break an existing tool.
//! 5. Docs: the two settings rows, README, `skills/veld/SKILL.md`, `llms-full.txt`.
//!
//! ## The four traps, each already paid for once
//!
//! - **Only hook events the tool does not wait on.** Claude's `PreToolUse`,
//!   `UserPromptSubmit`, `PermissionRequest` and `Stop` block, with ceilings up to
//!   600s. Installing veld on a blocking path means a wedged daemon can stall somebody's
//!   agent, which is never worth a badge. Prefer the fire-and-forget events, and bound
//!   whatever you must use twice ([`HOOK_TIMEOUT_SECS`] in the generated config *and*
//!   [`HOOK_REQUEST_TIMEOUT_MS`] in the CLI). Codex's `notify` needs neither: it
//!   `spawn()`s the program without ever awaiting it, so there is no ceiling to bound —
//!   the trap does not disappear, it just moves to whichever tool arrives blocking next.
//! - **Do not assume stdin.** Claude pipes the payload as JSON on stdin; **Codex's
//!   `notify` hook appends the event JSON as the final `argv` entry instead.** This is
//!   why `veld agent-state`'s payload parse lives in the CLI (`crates/veld/src/commands/agent.rs`)
//!   and not in the daemon, and why its clap definition carries a trailing positional
//!   for the argument-borne payload alongside the stdin path.
//! - **Never merge into a user's config file.** The ephemeral `--settings` shape exists
//!   for this, and Codex has its own equivalent for the same reason: `-c key=value`
//!   overrides a config value for one invocation without touching `~/.codex/config.toml`.
//!   If a tool has no equivalent flag, that is a reason to leave it unsupported and say
//!   so, not a reason to edit somebody's dotfile. See [`Injection`] for the two shapes
//!   this can take and why they need different wrapper logic.
//! - **The wrapper must be unreachable for anything but a plain interactive launch.**
//!   See `agent_script` in `veld-daemon/src/pty/shims.rs` for the rule and for the
//!   upstream bug (`anthropics/claude-code#42485`) that makes it necessary.
//!
//! ## What is deliberately *not* extensible
//!
//! There is no config surface for this and there must not be one. A tool veld shims is
//! a tool veld has a tested mapping for; a `veld.json` that could name an arbitrary
//! binary to wrap and an arbitrary command to run on its lifecycle events is remote code
//! execution with extra steps, and it would be repo-supplied rather than user-supplied
//! (see AGENTS.md on why hooks may never originate from a fetched extension).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A coding agent veld can install hooks into.
///
/// The receiving end ([`State`], the daemon endpoint, the inbox) is deliberately
/// generic, so a new tool is a hook installer plus a variant here — not a redesign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTool {
    Claude,
    Codex,
}

impl AgentTool {
    /// Every tool a shim is generated for. Iterated by the generator and its tests
    /// rather than a hand-written list, for the reason `opener::Tool::ALL` exists.
    pub const ALL: &'static [AgentTool] = &[Self::Claude, Self::Codex];

    /// The command name the shim stands in front of, and the name of the generated
    /// file.
    #[must_use]
    pub fn shim_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// The `--tool` spelling on the wire and on the CLI.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.as_str() == s)
    }

    /// How this tool's ephemeral hook configuration reaches its own invocation, and
    /// the flag the wrapper prepends it with. See [`Injection`] for why the two
    /// tools need different wrapper logic and not just a different flag name.
    #[must_use]
    pub fn injection(self) -> (&'static str, Injection) {
        match self {
            Self::Claude => ("--settings", Injection::SettingsFile),
            Self::Codex => ("-c", Injection::ConfigOverride),
        }
    }

    /// Shell `case` patterns (already `|`-joined, ready to drop into a POSIX `case`
    /// arm) matching an argv token that is this tool's own spelling of the
    /// injection flag above, or something close enough that veld's own must not be
    /// added on top of it. See rule 2 in `agent_script`
    /// (`veld-daemon/src/pty/shims.rs`): two of these in one invocation means one
    /// loses silently, and it must not be the user's.
    #[must_use]
    pub fn own_injection_flag_patterns(self) -> &'static str {
        match self {
            // `-p*` (not just `-p`) catches a glued short option like `-pfoo`; the exact
            // spelling alone would inject `--settings` ahead of the very path the docs
            // promise is left untouched.
            Self::Claude => "-p* | --print | --settings | --settings=*",
            // `-c*` catches `-cnotify=...` the same way. Any `-c`/`--config` at all is
            // excluded, not just one that happens to set `notify` — a second `-c` for an
            // unrelated key is still two overrides in one invocation, and which one a
            // duplicate key resolves to is not veld's to gamble on.
            Self::Codex => "-c* | --config | --config=*",
        }
    }
}

/// The two shapes a tool's ephemeral hook configuration can take, and therefore the
/// two things `agent_script` (`veld-daemon/src/pty/shims.rs`) has to do differently
/// after calling `veld agent-settings`.
///
/// Claude has no CLI override for `--settings`'s contents, so `agent-settings`
/// writes a **file** and prints its *path*; the wrapper only injects once that path
/// actually exists on disk, because a script that failed midway must not hand a
/// nonexistent path to `--settings` and get a hard error instead of a quiet
/// passthrough. Codex's `-c key=value` takes a literal value on the command line, so
/// `agent-settings` prints the *value* directly and there is no file to check —
/// injecting an empty string would be the only failure mode, and an empty check
/// already covers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Injection {
    /// `agent-settings` writes a file and prints its path; inject only once that
    /// file exists.
    SettingsFile,
    /// `agent-settings` prints a literal value with no file behind it; inject
    /// whenever it printed anything at all.
    ConfigOverride,
}

/// What a surface running an agent is doing.
///
/// Deliberately five values and not three: `Unknown` is what an unrecognised
/// signal maps to, and it must be distinguishable from `Idle` so that "we were told
/// something we do not understand" cannot render as "it finished".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// An agent has just launched in this pane and is waiting for its first prompt.
    ///
    /// **Reported by the wrapper, not by a hook** — it is the one fact only the thing
    /// that launches the agent knows, and it exists to answer a question the shell gets
    /// wrong. A pane running `claude` is one long shell command, so OSC 133 says "a
    /// command is running here" for the entire session; before any hook has fired there
    /// is nothing to contradict it, and the activity spinner ran on a session sitting
    /// idle at its prompt.
    ///
    /// So this claims **hook authority without reporting an event**: it tells the inbox
    /// that an agent owns this pane (so the shell stops speaking for it) while saying
    /// that nothing is happening. Deliberately *not* [`Self::Idle`], which means "a turn
    /// ended" and would put a spurious "agent finished" in the inbox on every launch.
    Ready,
    /// Running. Nothing is wanted from the user.
    Working,
    /// Waiting on the user — a permission prompt, a question, a plan to approve.
    /// This is the one that becomes an `attention` event.
    Blocked,
    /// The turn ended and the agent is waiting for the next prompt. A `finished`
    /// event: something happened while you weren't looking, and it is done.
    Idle,
    /// The session ended.
    Done,
    /// Told something we do not understand. Produces no inbox event — silence beats
    /// a badge the user cannot act on.
    Unknown,
}

impl State {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Ready,
            Self::Working,
            Self::Blocked,
            Self::Idle,
            Self::Done,
            Self::Unknown,
        ]
        .into_iter()
        .find(|st| st.as_str() == s)
    }
}

/// A hook payload, as far as veld reads it.
///
/// Every field optional and `deny_unknown_fields` deliberately **absent**: this is
/// somebody else's schema (two of them, now), it grows, and a hook that fails to
/// parse is a hook that silently stops reporting. Claude's fields are snake_case and
/// discriminated by `hook_event_name`; Codex's are kebab-case (`serde(rename)`) and
/// discriminated by `event_type`. Neither tool's fields collide with the other's
/// spelling, so one struct reads both without either seeing the fields it does not
/// understand.
#[derive(Debug, Default, Deserialize)]
pub struct HookPayload {
    #[serde(default)]
    pub hook_event_name: String,
    /// `Notification`'s discriminator. The human-readable `message` is *not* the
    /// discriminator and must never be matched on — it is prose and it changes.
    #[serde(default)]
    pub notification_type: Option<String>,
    /// `PreToolUse`/`PostToolUse`.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Codex's discriminator, `"type"` on the wire. Today only ever
    /// `"agent-turn-complete"` — Codex's `notify` fires on exactly one event — but
    /// matched by value rather than assumed, the same as Claude's `notification_type`,
    /// so a future event this code has never seen becomes [`State::Unknown`] instead
    /// of a guess.
    #[serde(default, rename = "type")]
    pub event_type: Option<String>,
}

/// The state a Claude Code hook payload reports.
///
/// # The `idle_prompt` question, which the brief got backwards
///
/// `idle_prompt` is not a false positive to be suppressed — it is Claude's
/// *end-of-turn* notification, "I have finished and I am waiting for your next
/// prompt". Treating it as `Blocked` is what produces the notorious
/// attention-after-every-turn badge; treating it as [`State::Idle`] puts it in the
/// right bucket and keeps the event. Only a real permission prompt, a question, or
/// an elicitation dialog means the user is being *waited on*.
///
/// # Everything unrecognised is `Unknown`, and that is load-bearing
///
/// `auth_success` is a `Notification` too, and it wants nothing from anybody. A
/// default of `Blocked` would badge on it — and on every notification type Claude
/// adds after this code was written.
#[must_use]
pub fn claude_state(payload: &HookPayload) -> State {
    match payload.hook_event_name.as_str() {
        // **Deliberately not `Working`, and deliberately not installed.**
        //
        // A session starting is not a session *working*: Claude prints its prompt and
        // waits for you. Mapping it to `Working` set the state once and left it —
        // nothing else fires until a `Notification` or a `Stop` — so a pane sitting idle
        // at its prompt showed a spinner for the whole session, and when it genuinely
        // started working the indicator did not change. Reported from real use, and the
        // reason `SessionStart` is no longer in `claude_settings_doc`: it is a *blocking*
        // event, so it was costing an agent latency to produce a misleading state.
        //
        // The arm stays so the receiving end is not narrower than a future installer —
        // see the `PreToolUse` note below for the same reasoning — and it answers
        // `Unknown`, which produces no event and no state.
        "SessionStart" => State::Unknown,
        // The one honest "it started working" signal, and the reason the wrapper's
        // `Ready` is worth having: between a prompt going in and a `Stop` coming out, the
        // agent *is* working, and this is the event that says the turn began.
        //
        // Blocking, like `Stop` — but once per **turn**, not per tool call, and bounded
        // twice ([`HOOK_TIMEOUT_SECS`] here, [`HOOK_REQUEST_TIMEOUT_MS`] in the CLI). Its
        // own ceiling is 30s rather than the 600s the other blocking events carry, so it
        // is the cheapest place to be on that path. `PostToolUse` is the async
        // alternative and is worse: hundreds of process spawns a session to learn
        // something this says once.
        "UserPromptSubmit" => State::Working,
        "Notification" => match payload.notification_type.as_deref() {
            // Blocked on the user. `permission_prompt` is the tool-approval dialog;
            // `agent_needs_input` is a subagent asking; the two `elicitation_*`
            // dialogs are an MCP server asking through Claude. All four mean the
            // session is stopped until a human answers.
            Some(
                "permission_prompt"
                | "agent_needs_input"
                | "elicitation_dialog"
                | "elicitation_url_dialog",
            ) => State::Blocked,
            // The turn ended. See the note above.
            Some("idle_prompt" | "agent_completed") => State::Idle,
            _ => State::Unknown,
        },
        // The turn ended without a notification. Redundant with `idle_prompt` on a
        // Claude that sends one, and the only signal on a Claude that does not —
        // the two collapse to the same state, so a duplicate costs nothing.
        "Stop" => State::Idle,
        "SessionEnd" => State::Done,
        // A tool call that cannot proceed without the user. `PreToolUse` is a
        // *blocking* event, so veld does not install it — it is matched here only
        // because a future installer might, and a receiving end that understood
        // fewer events than the installer registers is how a signal goes missing.
        "PreToolUse" => match payload.tool_name.as_deref() {
            Some("AskUserQuestion" | "ExitPlanMode") => State::Blocked,
            _ => State::Working,
        },
        _ => State::Unknown,
    }
}

/// The state a Codex `notify` payload reports.
///
/// # Why this can only ever return `Idle` or `Unknown`
///
/// Codex's `notify` fires on exactly one event, `agent-turn-complete` — there is no
/// approval-request or turn-started notification the way Claude has `Notification`
/// and `UserPromptSubmit`. So there is no signal this function could map to
/// [`State::Blocked`] or [`State::Working`] even in principle: Codex does not tell
/// veld either of those things. That is a real, measured gap in what a Codex pane's
/// badge can say — not an oversight here — and [`State::Ready`], reported by the
/// wrapper itself before Codex even starts, is what a Codex pane shows for the
/// entire time it is genuinely working, same as before its first prompt.
///
/// Matched by value rather than defaulted to `Idle`, for the same reason
/// [`claude_state`] does not default to `Blocked`: an event type Codex adds later
/// must read as "we don't know" rather than silently claim to be the one event this
/// was written against.
#[must_use]
pub fn codex_state(payload: &HookPayload) -> State {
    match payload.event_type.as_deref() {
        Some("agent-turn-complete") => State::Idle,
        _ => State::Unknown,
    }
}

/// The ephemeral settings document handed to `claude --settings`.
///
/// # Why the session id is baked into the command
///
/// The hook has to say *which* terminal it is reporting for. The obvious mechanisms
/// are both unverified: whether Claude Code passes the shell's environment to a hook
/// subprocess is undocumented, and the `env` block in a settings file is documented
/// as setting variables for tools rather than as a guarantee about hooks. A literal
/// argument depends on neither. The file is per session, so there is nothing to
/// parameterise at run time.
///
/// # Why these four events
///
/// `Notification` and `SessionEnd` are fire-and-forget — Claude does not wait.
/// `UserPromptSubmit` and `Stop` **do** block, so each hook carries a short `timeout`
/// here *and* the CLI it calls bounds its own HTTP request. Two independent bounds,
/// because a badge is never worth stalling somebody's agent for. Both fire once per
/// **turn**, which is what makes being on that path affordable at all.
///
/// Together with the wrapper's [`State::Ready`] these cover the whole cycle: launched and
/// idle → working → blocked or finished → gone.
///
/// Two events are deliberately absent, and both absences were paid for:
///
/// - **`SessionStart`** was installed and is not any more. It is blocking, and the state
///   it produced was wrong: a session starting is not a session working, so a pane idle at
///   its prompt spun forever while a pane genuinely working looked identical. What it was
///   really trying to say — "an agent lives here now" — is [`State::Ready`], which the
///   wrapper reports for free before it `exec`s, off any blocking path at all.
/// - **`PostToolUse`** is the other way to learn "working", and it is worse: async, but
///   it fires on every tool call — hundreds per session, each one a process spawn — to
///   learn what `UserPromptSubmit` says once per turn. Reach for it only if per-tool
///   granularity is ever actually wanted, and measure the spawn cost first.
#[must_use]
pub fn claude_settings_doc(cli: &Path, session_id: &str) -> serde_json::Value {
    let command = format!(
        "{} agent-state --tool claude --session {}",
        sh_quote(cli.as_os_str().to_string_lossy().as_ref()),
        sh_quote(session_id),
    );
    let entry = serde_json::json!([{
        "hooks": [{
            "type": "command",
            "command": command,
            // Seconds. Claude's own ceiling for a blocking hook is 600s; this is
            // the promise that veld cannot use more than a moment of it.
            "timeout": HOOK_TIMEOUT_SECS,
        }],
    }]);
    serde_json::json!({
        "hooks": {
            "UserPromptSubmit": entry,
            "Notification": entry,
            "Stop": entry,
            "SessionEnd": entry,
        },
    })
}

/// The literal value handed to Codex's `-c` override — `notify=[...]`, never written
/// to `~/.codex/config.toml`.
///
/// # Why a config override and not a settings file
///
/// Codex has no `--settings`-shaped flag that merges into a settings hierarchy; what
/// it has is `-c key=value`, which overrides one config key for one invocation. That
/// is the same property `--settings` gives Claude — nothing of the user's is
/// touched — so [`AgentTool::injection`] treats it as [`Injection::ConfigOverride`]
/// rather than leaving Codex unsupported: `veld agent-settings` prints this value
/// directly instead of a file path, and the wrapper passes it straight through.
///
/// # Why the array elements are JSON-escaped, not TOML-escaped
///
/// The value Codex parses is TOML, but the string literal this builds — double
/// quotes, `\\`/`\"`/control-character escapes — is the same core grammar TOML's
/// basic strings use, and every element here is either an absolute path or one of a
/// handful of ASCII flag names this crate controls. `serde_json::to_string` on a
/// `&str` is infallible and gives that escaping for free, without a second
/// hand-rolled escaper to keep in sync with [`sh_quote`].
///
/// # Why there is no timeout here
///
/// Codex's `notify` is fire-and-forget — it spawns the program and does not await
/// it — so there is nothing here for [`HOOK_TIMEOUT_SECS`] to bound. The CLI's own
/// [`HOOK_REQUEST_TIMEOUT_MS`] still applies: Codex not waiting for the notifier
/// does not mean the notifier should wait forever on the daemon.
#[must_use]
pub fn codex_notify_config(cli: &Path, session_id: &str) -> String {
    let tokens = [
        cli.as_os_str().to_string_lossy().into_owned(),
        "agent-state".to_owned(),
        "--tool".to_owned(),
        "codex".to_owned(),
        "--session".to_owned(),
        session_id.to_owned(),
    ];
    let elements = tokens
        .iter()
        .map(|t| serde_json::to_string(t).expect("a String serializes to JSON infallibly"))
        .collect::<Vec<_>>()
        .join(",");
    format!("notify=[{elements}]")
}

/// What each generated hook is allowed to take, in seconds.
///
/// Two, not zero: the request itself is to `127.0.0.1` and answers in single-digit
/// milliseconds, but a daemon that accepts and then never replies would otherwise
/// hold a blocking `Stop` hook for Claude's own 600s ceiling. The CLI bounds the
/// request too ([`HOOK_REQUEST_TIMEOUT_MS`]); this is the outer belt.
pub const HOOK_TIMEOUT_SECS: u64 = 2;

/// How long the CLI waits for the daemon before giving up on reporting a state.
///
/// Short and silent. Nothing downstream of a missed report is broken — the badge
/// does not appear — and the alternative is an agent that pauses because a
/// notification could not be delivered.
pub const HOOK_REQUEST_TIMEOUT_MS: u64 = 1_000;

/// Where a session's ephemeral settings file lives, inside this daemon's shim
/// directory.
///
/// Named after the session rather than the launch, so relaunching an agent in the
/// same pane reuses one file instead of accumulating one per start.
#[must_use]
pub fn settings_path(shim_dir: &Path, tool: AgentTool, session_id: &str) -> PathBuf {
    shim_dir
        .join("agent")
        .join(format!("{}-{}.json", tool.as_str(), sanitize(session_id)))
}

/// How long an unused settings file is kept before the next write sweeps it.
///
/// Generous on purpose. These files are a few hundred bytes, and the cost of being
/// wrong is asymmetric: keeping one too long wastes nothing, while deleting one under
/// a live agent removes the hooks it is running with.
pub const SETTINGS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Single-quote a value for `sh`.
///
/// The session id is validated upstream and the CLI path is veld's own, so this is
/// belt-and-braces rather than the only defence — but the string ends up inside a
/// command Claude Code runs through a shell, and "it can't contain a quote" is a
/// claim that has to be enforced somewhere rather than assumed everywhere.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A session id reduced to something safe to put in a filename.
///
/// Session ids are already validated by the daemon, but this function's output is a
/// path and a path is not the place to find out that the validator changed.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(event: &str, notification: Option<&str>, tool: Option<&str>) -> HookPayload {
        HookPayload {
            hook_event_name: event.to_owned(),
            notification_type: notification.map(str::to_owned),
            tool_name: tool.map(str::to_owned),
            event_type: None,
        }
    }

    /// `idle_prompt` is the end of a turn, not a request for attention.
    ///
    /// This is the assertion the whole feature's credibility rests on. Claude sends
    /// `idle_prompt` after **every** turn; classifying it as `Blocked` is what makes
    /// a badge that says "needs you" on work that needs nobody, which is how a user
    /// learns to ignore it. It is a `finished` event and it belongs in the inbox —
    /// suppressing it would have thrown away the most common real signal there is.
    #[test]
    fn an_end_of_turn_notification_is_finished_and_never_attention() {
        assert_eq!(
            claude_state(&payload("Notification", Some("idle_prompt"), None)),
            State::Idle
        );
        assert_eq!(
            claude_state(&payload("Notification", Some("agent_completed"), None)),
            State::Idle
        );
        assert_eq!(claude_state(&payload("Stop", None, None)), State::Idle);
    }

    /// Every notification type that genuinely stops the session, and nothing else.
    #[test]
    fn only_a_real_prompt_for_the_user_is_blocked() {
        for kind in [
            "permission_prompt",
            "agent_needs_input",
            "elicitation_dialog",
            "elicitation_url_dialog",
        ] {
            assert_eq!(
                claude_state(&payload("Notification", Some(kind), None)),
                State::Blocked,
                "{kind}"
            );
        }
        // A question and a plan approval, through the tool name rather than a
        // notification.
        for tool in ["AskUserQuestion", "ExitPlanMode"] {
            assert_eq!(
                claude_state(&payload("PreToolUse", None, Some(tool))),
                State::Blocked,
                "{tool}"
            );
        }
    }

    /// A session starting is not a session working.
    ///
    /// The bug: `SessionStart` mapped to `Working`, nothing else fired until the first
    /// `Notification` or `Stop`, and so a pane sitting idle at Claude's prompt showed the
    /// activity spinner for the whole session — while a pane genuinely working looked
    /// exactly the same. It was also a *blocking* hook, so veld was buying that
    /// misinformation with the agent's own latency.
    /// The whole cycle a pane goes through, in order, with the producer of each step.
    ///
    /// Worth one test because the states only make sense as a sequence, and the gap this
    /// closes was invisible in any single one of them: with no `Ready`, a launched agent
    /// had *no* reported state at all until its first turn ended, so the shell's "a
    /// command is running here" spoke for it and an idle session spun.
    #[test]
    fn the_reported_cycle_covers_launch_to_exit() {
        // Launch — the wrapper, not a hook. No event, but it claims the pane.
        // (`State::Ready` is produced by `veld agent-state --launched`, so there is no
        // payload to map; this asserts the vocabulary round-trips it.)
        assert_eq!(State::parse("ready"), Some(State::Ready));
        // A turn begins.
        assert_eq!(
            claude_state(&payload("UserPromptSubmit", None, None)),
            State::Working
        );
        // …it needs you…
        assert_eq!(
            claude_state(&payload("Notification", Some("permission_prompt"), None)),
            State::Blocked
        );
        // …it finishes…
        assert_eq!(claude_state(&payload("Stop", None, None)), State::Idle);
        // …and the session ends.
        assert_eq!(
            claude_state(&payload("SessionEnd", None, None)),
            State::Done
        );
    }

    #[test]
    fn a_session_starting_reports_nothing() {
        assert_eq!(
            claude_state(&payload("SessionStart", None, None)),
            State::Unknown,
            "an agent that has started is waiting for a prompt, not working"
        );
        // And nothing installs it any more, which is the half that stops the latency
        // cost — asserted with the rest of the hook set in the settings-document test.
    }

    /// Anything unrecognised produces no event rather than a false positive.
    ///
    /// The failure mode being pinned: a `Notification` whose type veld has never
    /// heard of — `auth_success` today, whatever Claude adds tomorrow — must not
    /// default into `Blocked`. A badge on `auth_success` is a badge for nothing.
    #[test]
    fn an_unrecognised_signal_is_unknown_not_attention() {
        assert_eq!(
            claude_state(&payload("Notification", Some("auth_success"), None)),
            State::Unknown
        );
        assert_eq!(
            claude_state(&payload("Notification", Some("invented_later"), None)),
            State::Unknown
        );
        // No discriminator at all — an older or newer Claude. Still not attention.
        assert_eq!(
            claude_state(&payload("Notification", None, None)),
            State::Unknown
        );
        assert_eq!(
            claude_state(&payload("SomeFutureEvent", None, None)),
            State::Unknown
        );
        // And a payload that does not deserialise at all is the same silence: the
        // struct has no required field, so a schema that grew still parses.
        let grown: HookPayload =
            serde_json::from_str(r#"{"hook_event_name":"Stop","brand_new_field":42}"#).unwrap();
        assert_eq!(claude_state(&grown), State::Idle);
    }

    /// The generated document installs only events veld intends, carries the session
    /// id literally, and bounds every hook.
    #[test]
    fn the_settings_document_bakes_the_session_and_bounds_every_hook() {
        let doc = claude_settings_doc(Path::new("/opt/veld/bin/veld"), "pane-7");
        let hooks = doc["hooks"].as_object().expect("a hooks object");
        let mut installed: Vec<&str> = hooks.keys().map(String::as_str).collect();
        installed.sort_unstable();
        assert_eq!(
            installed,
            ["Notification", "SessionEnd", "Stop", "UserPromptSubmit"],
            "SessionStart is BLOCKING and its state was a lie — a session starting is not \
             a session working, and what it was reaching for is the wrapper's `Ready`; \
             PostToolUse fires per tool call to learn what UserPromptSubmit says once per \
             turn; PermissionRequest blocks with nothing to contribute"
        );
        for (event, entry) in hooks {
            let hook = &entry[0]["hooks"][0];
            assert_eq!(hook["type"], "command", "{event}");
            assert_eq!(
                hook["timeout"],
                serde_json::json!(HOOK_TIMEOUT_SECS),
                "{event}: an unbounded hook can hold a blocking event for 600s"
            );
            let command = hook["command"].as_str().unwrap();
            // The session travels as an argument, so nothing depends on Claude
            // passing the shell's environment through to a hook subprocess.
            assert!(command.contains("--session 'pane-7'"), "{event}: {command}");
            assert!(command.contains("--tool claude"), "{event}: {command}");
            assert!(
                command.starts_with("'/opt/veld/bin/veld' agent-state"),
                "{event}: the CLI is named by absolute path, so a dev daemon's \
                 terminals reach its own CLI: {command}"
            );
        }
        // Nothing else is written. In particular no `env`, no `permissions`, no
        // `model` — `--settings` merges, so anything here silently outranks the
        // user's own configuration.
        assert_eq!(
            doc.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["hooks"],
            "an ephemeral settings file must set nothing but the hooks it exists for"
        );
    }

    /// Codex's `notify` fires on exactly one event; everything else, including no
    /// event at all, is `Unknown` rather than a guess.
    #[test]
    fn codex_only_reports_turn_complete_as_idle() {
        let turn_complete = HookPayload {
            event_type: Some("agent-turn-complete".to_owned()),
            ..Default::default()
        };
        assert_eq!(codex_state(&turn_complete), State::Idle);

        for kind in [None, Some("session-configured"), Some("invented-later")] {
            let payload = HookPayload {
                event_type: kind.map(str::to_owned),
                ..Default::default()
            };
            assert_eq!(codex_state(&payload), State::Unknown, "{kind:?}");
        }
    }

    #[test]
    fn the_notify_config_bakes_the_cli_and_session_into_a_toml_array() {
        let value = codex_notify_config(Path::new("/opt/veld/bin/veld"), "pane-7");
        assert_eq!(
            value,
            r#"notify=["/opt/veld/bin/veld","agent-state","--tool","codex","--session","pane-7"]"#
        );
    }

    #[test]
    fn a_quote_in_a_session_id_cannot_break_the_notify_array_out_of_its_string() {
        // A double quote is what a TOML/JSON basic string escapes; the element stays
        // one array entry rather than closing early and adding a second.
        let value = codex_notify_config(Path::new("/a b/veld"), r#"it"s"#);
        assert_eq!(
            value,
            r#"notify=["/a b/veld","agent-state","--tool","codex","--session","it\"s"]"#
        );
    }

    #[test]
    fn each_tool_carries_its_own_injection_shape() {
        assert_eq!(
            AgentTool::Claude.injection(),
            ("--settings", Injection::SettingsFile)
        );
        assert_eq!(
            AgentTool::Codex.injection(),
            ("-c", Injection::ConfigOverride)
        );
    }

    #[test]
    fn a_quote_in_a_session_id_cannot_break_out_of_the_hook_command() {
        let doc = claude_settings_doc(Path::new("/a b/veld"), "it's");
        let command = doc["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(command.starts_with("'/a b/veld'"), "{command}");
        assert!(command.contains(r"'it'\''s'"), "{command}");
    }

    #[test]
    fn a_settings_path_is_per_session_and_never_escapes_its_directory() {
        let dir = Path::new("/tmp/shims");
        assert_eq!(
            settings_path(dir, AgentTool::Claude, "abc-1"),
            PathBuf::from("/tmp/shims/agent/claude-abc-1.json")
        );
        // The same session twice is the same file — a relaunch reuses it rather
        // than leaving one behind per start.
        assert_eq!(
            settings_path(dir, AgentTool::Claude, "abc-1"),
            settings_path(dir, AgentTool::Claude, "abc-1")
        );
        // Traversal cannot survive the name, even though the daemon validates
        // session ids upstream: this function's output is a path.
        let escaped = settings_path(dir, AgentTool::Claude, "../../etc/passwd");
        assert_eq!(
            escaped,
            PathBuf::from("/tmp/shims/agent/claude-______etc_passwd.json")
        );
        assert!(escaped.starts_with("/tmp/shims/agent"));
    }

    #[test]
    fn tools_and_states_round_trip_through_their_spelling() {
        for tool in AgentTool::ALL.iter().copied() {
            assert_eq!(AgentTool::parse(tool.as_str()), Some(tool));
        }
        assert_eq!(AgentTool::parse("codex"), Some(AgentTool::Codex));
        assert_eq!(AgentTool::parse("cursor"), None);
        for state in [
            State::Working,
            State::Blocked,
            State::Idle,
            State::Done,
            State::Unknown,
        ] {
            assert_eq!(State::parse(state.as_str()), Some(state));
        }
        assert_eq!(State::parse("busy"), None);
    }
}
