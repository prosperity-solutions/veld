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
//! It does not touch `~/.codex/config.toml` either. Codex's ephemeral configuration
//! is a `-c notify=[...]` override on the command line, not a file — see
//! [`codex_notify_config`]. **This one does not merge**: it replaces whatever
//! `notify` the user configured for the duration of the wrapped launch, which is a
//! real, user-visible behaviour change (their own notifier goes quiet in a veld
//! terminal) that `--settings`'s merge does not have. There is no cheap fix for
//! that asymmetry — chaining to the user's own notifier means resolving and parsing
//! their config, including profile layering, just to build an argv that also invokes
//! it — so it is a documented cost (README's "What it cannot do"), not a bug.
//!
//! It does not touch `~/.pi/agent/settings.json`, `.pi/settings.json`, or either of
//! Pi's auto-discovered extension directories (`~/.pi/agent/extensions/`,
//! `.pi/extensions/`) either. Pi's ephemeral configuration is a `-e <path>` flag
//! pointing at a generated extension **module** — code, not a settings document — in
//! this daemon's own shim directory. See [`pi_extension_doc`] for why an extension is
//! the right-shaped hook for a tool with no `hooks`/`notify` config key at all, and
//! [`pi_state`] for why it can report `Working`/`Idle` but never `Blocked`.
//!
//! # Adding another agent
//!
//! Everything downstream of this module is **already generic** — the daemon endpoint
//! takes a state rather than a vendor payload, the wire carries a tool name, and the
//! browser store, the rail glyph, the pane dot and the notification table all key on
//! [`State`] and never on which tool produced it. So a new tool is an installer plus a
//! mapping, in five edits, and nothing else has to move:
//!
//! 1. A variant on [`AgentTool`], and an arm in every `match self` on it — `ALL`,
//!    `shim_name`, `as_str`, `injection`, `own_injection_flag_patterns`,
//!    `extra_interactive_first_words` today, and whatever this list has grown to by
//!    the time you read it; the compiler enforces exhaustiveness, this comment does
//!    not. `shim_name` is the command the wrapper stands in front of.
//! 2. A `<tool>_state(&HookPayload) -> State` beside [`claude_state`]/[`codex_state`],
//!    and an arm in `veld agent-state`'s `match tool` (`crates/veld/src/commands/agent.rs`).
//! 3. A `<tool>_settings_doc`/`<tool>_notify_config` beside [`claude_settings_doc`]/
//!    [`codex_notify_config`], depending on [`Injection`] — see below. `prepare_in` in
//!    `veld-daemon/src/pty/shims.rs` already generates one wrapper per `AgentTool::ALL`,
//!    so the script itself comes for free either way. A new [`Injection::SettingsFile`]
//!    tool also needs an arm in [`settings_path`]'s extension match (the compiler
//!    refuses to build without one, since the match is exhaustive over
//!    [`AgentTool`]) — easy to miss since it lives well below [`Injection`] itself.
//! 4. Whatever [`HookPayload`] is missing for the new tool's schema — every field is
//!    optional and unknown fields are ignored, so adding one cannot break an existing tool.
//! 5. Docs: the two settings rows, README, `skills/veld/SKILL.md`, `llms-full.txt`.
//!
//! ## The five traps, each already paid for once
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
//!   this can take and why they need different wrapper logic. **"Override" is not
//!   "merge", and the difference is user-visible**: `--settings` merges, so a Claude
//!   user's own hooks still run alongside veld's; `-c notify=[...]` *replaces* whatever
//!   `notify` a Codex user configured in `~/.codex/config.toml`, silently, for the
//!   duration of the wrapped launch. There is no cheap fix — chaining to the user's own
//!   notifier means resolving and parsing their config (including profile layering) just
//!   to build an argv to also invoke — so this is a documented cost, not a bug, laid out
//!   in README's "What it cannot do".
//! - **A richer signal is not automatically the right one to use.** Codex's `notify` is
//!   not its only lifecycle mechanism — it also ships a `hooks` system whose event names
//!   echo Claude's (`pre_tool_use`, `permission_request`, `user_prompt_submit`,
//!   `session_end`, …) closely enough to look like deliberate compatibility. Using it
//!   would give Codex the same `Working`/`Blocked` fidelity Claude has. It was not
//!   chosen because it costs something `notify` does not: an interactive **hook-trust
//!   review** the first time Codex sees a hook, or `--dangerously-bypass-hook-trust`,
//!   which is not scoped to veld's own hook and disables trust review for every hook
//!   configured for that invocation. Neither is compatible with a wrapper that must stay
//!   invisible. See [`codex_state`] for the full reasoning and the version this was
//!   measured against — check it again before trading `notify` for `hooks`.
//! - **The wrapper must be unreachable for anything but a plain interactive launch.**
//!   See `agent_script` in `veld-daemon/src/pty/shims.rs` for the rule and for the
//!   upstream bug (`anthropics/claude-code#42485`) that makes it necessary. "Bare first
//!   word ⇒ subcommand, not interactive" is close to exhaustive for Claude but is not
//!   for every tool: Codex's `resume`/`fork` are bare first words that ARE interactive
//!   sessions, and [`AgentTool::extra_interactive_first_words`] is the per-tool escape
//!   hatch for exactly that — a short, stable list of a tool's own subcommand names,
//!   never content-guessed.
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
    Pi,
}

impl AgentTool {
    /// Every tool a shim is generated for. Iterated by the generator and its tests
    /// rather than a hand-written list, for the reason `opener::Tool::ALL` exists.
    pub const ALL: &'static [AgentTool] = &[Self::Claude, Self::Codex, Self::Pi];

    /// The command name the shim stands in front of, and the name of the generated
    /// file.
    #[must_use]
    pub fn shim_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }

    /// The `--tool` spelling on the wire and on the CLI.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
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
            Self::Codex => (
                "-c",
                Injection::ConfigOverride {
                    key_prefix: "notify=",
                },
            ),
            // `-e`/`--extension <path>` loads one extension module for this invocation
            // only — documented as repeatable and additive, never touching
            // `~/.pi/agent/settings.json` or the auto-discovered
            // `~/.pi/agent/extensions/`/`.pi/extensions/` directories. That is the same
            // "nothing of the user's is touched" property `--settings` gives Claude, by
            // a different route: a file on disk rather than a merge into a settings
            // hierarchy. See [`pi_extension_doc`].
            Self::Pi => ("-e", Injection::SettingsFile),
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
            // `-c*` catches `-cnotify=...` the same way. Excluded on ANY `-c`/`--config`,
            // not only one that happens to set `notify`: telling the two apart from a
            // POSIX `case` pattern against a single token means parsing `-c`'s *value*,
            // which is either a second `-c<key>=<value>` token or a following separate
            // one — real parsing this script does not otherwise do anywhere. `--enable`/
            // `--disable` are overrides too (Codex's own docs: "equivalent to
            // `-c features.<name>=…`") but are deliberately NOT excluded here, because
            // they can never collide with the `notify` key this wrapper sets — the
            // conservative case is `-c`/`--config` specifically, not "any flag that is
            // secretly a config override". Cost: `codex -c model="o3"` gets no badge for
            // that session, silently — accepted in exchange for not hand-rolling a second
            // parser in shell.
            Self::Codex => "-c* | --config | --config=*",
            // `-e`/`--extension <path>` is documented as repeatable, so a second `-e`
            // from this wrapper would not silently displace the user's own — unlike
            // Claude's `--settings` or Codex's `-c`. Excluded anyway, belt-and-braces:
            // "repeatable" is measured at pi-coding-agent 0.84.1, and a version that
            // ever changes that guarantee must not find this wrapper adding a second
            // `-e` on top of the user's.
            //
            // `-p*`/`--print` too, for the same reason Claude's own pattern carries
            // them: `-p`/`--print` is Pi's non-interactive print-and-exit mode
            // (documented `pi -p "prompt"`, also reads piped stdin) — nobody is
            // waiting on it, so there is nothing to badge and no reason to add `-e`
            // to its argv.
            //
            // **Known, accepted gap**: `--mode json`/`--mode rpc` are equally
            // non-interactive (they replace the TUI with a scripted event stream) and
            // arguably deserve the same exclusion `-p`/`--print` gets, but are not
            // listed here. A `case` pattern against one argv token at a time cannot
            // tell `--mode json` (two tokens) from `--mode` followed by an unrelated
            // positional, and pi's own docs show the space-separated form as the
            // primary spelling — so only a glued `--mode=json`/`--mode=rpc` could ever
            // be matched this way, covering a spelling nobody's docs recommend. Same
            // shape as Codex's `-c` value-parsing limitation above: a real parser is
            // the only way to close this, and it is not worth hand-rolling one in
            // shell for an edge case (running `pi --mode rpc` inside a Veld terminal
            // pane at all) this narrow.
            Self::Pi => "-p* | --print | -e* | --extension | --extension=*",
        }
    }

    /// Bare first words that count as an interactive launch for this tool even though
    /// rule 1 in `agent_script` (`veld-daemon/src/pty/shims.rs`) would otherwise treat
    /// any bare first word as a subcommand.
    ///
    /// Empty for Claude: none of its subcommands are interactive-continuation entry
    /// points, so the plain rule already gets it right. Not empty for Codex: `resume`
    /// and `fork` are Codex's *own* stable subcommand names for continuing a past
    /// interactive session — this is the same shape as
    /// [`Self::own_injection_flag_patterns`], a short list of a tool's own vocabulary,
    /// never a guess about arbitrary prompt content (that guess is what rule 1's own
    /// doc comment calls out as the road to `anthropics/claude-code#42485`).
    ///
    /// This matters beyond correctness-in-principle: `README.md`'s own example
    /// `ide.panes` entry for Codex sets `resume: {argv: ["codex", "resume", "--last"]}`,
    /// so without this every resumed/auto-resumed Codex pane got zero hook injection —
    /// the feature silently off for the exact pattern the docs hold up as supported.
    /// Verified (codex-cli 0.146.0) that `-c key=value` parses identically whether it
    /// precedes or follows `resume`/`fork` on the command line, which is what makes
    /// prepending the injected flag ahead of `"$@"` — this wrapper's one strategy,
    /// unconditional on subcommand — safe for these two as well as for a bare launch.
    #[must_use]
    pub fn extra_interactive_first_words(self) -> &'static str {
        match self {
            Self::Claude => "",
            Self::Codex => "resume | fork",
            // Pi resumes a past session through flags (`-c`/`--continue`, `-r`/`--resume`,
            // `--session <path|id>`, `--fork <path|id>`), never through a bare subcommand
            // word — rule 1's plain heuristic already gets every one of those right, the
            // same as Claude.
            Self::Pi => "",
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
/// `agent-settings` prints the *value* directly and there is no file to check.
///
/// Injecting an empty string is not the only failure mode here, though it looked that
/// way at first: Codex parses a malformed `-c` value as TOML and, on a parse failure,
/// falls back to treating it as a **literal string** rather than rejecting it — so a
/// non-empty-but-broken value does not fail closed the way a missing file does. An
/// empty check catches "nothing printed"; it does not catch "printed garbage".
/// `ConfigOverride`'s `key_prefix` is the cheap guard for that second case: the
/// wrapper checks the printed value actually starts with the key this tool's
/// `agent-settings` arm is supposed to set, and drops it otherwise (belt-and-braces
/// alongside `agent-settings`'s own correctness, the same relationship `sh_quote` has
/// to its callers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Injection {
    /// `agent-settings` writes a file and prints its path; inject only once that
    /// file exists.
    SettingsFile,
    /// `agent-settings` prints a literal value with no file behind it; inject once
    /// it starts with `key_prefix` (e.g. `"notify="`), the wrapper's cheap check that
    /// what it is about to hand the real binary is shaped like the value this tool's
    /// `agent-settings` arm actually produces.
    ConfigOverride { key_prefix: &'static str },
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
    /// Told something we do not understand — **or something we understand and have
    /// chosen not to report**, which is the larger population. Produces no inbox event
    /// and touches no state: silence beats a badge the user cannot act on. See
    /// [`claude_state`] for both kinds, and `veld agent-state` for the early return
    /// that makes this reach nothing at all.
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
    /// Pi's discriminator. Unlike Claude's and Codex's fields, this is not somebody
    /// else's schema — [`pi_extension_doc`] is the only thing that ever writes this
    /// wire shape, so `event` is whichever `pi.on(...)` name veld's own generated
    /// extension chose to forward: `"agent_start"`, `"agent_settled"`, `"session_shutdown"`.
    #[serde(default)]
    pub event: Option<String>,
    /// `session_shutdown`'s reason (`"quit" | "reload" | "new" | "resume" | "fork"`),
    /// carried alongside `event` because only `"quit"` is the session actually
    /// ending — the other four mean a new session is about to start in this same
    /// pane, and reporting [`State::Done`] for those would badge a pane that is still
    /// live.
    #[serde(default)]
    pub reason: Option<String>,
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
///
/// # A subagent's turn is not the session's turn
///
/// Only the **session** reports state here. A subagent finishing produces no event and
/// no state, because it is not something the user acts on and it is not the pane's
/// state either — and the two failures compound: `agent_completed` used to map to
/// [`State::Idle`], so a subagent ending put an "Agent finished" in the inbox (a
/// notification for work nobody asked about) *and* cleared the pane's working flag, so
/// the spinner died while the session was still mid-turn with no further signal until it
/// really ended. One arm, both symptoms. Claude gates the notification — a subagent the
/// user stopped produces none — so this was *most* agent-tool calls rather than all of
/// them, which changes how loud the bug was and not whether the mapping was wrong.
///
/// It is also not only about *finishing*: Claude sends `agent_completed` for a subagent
/// that **failed** as well (its message is `"<label> failed"`), so the old mapping
/// reported a failure as the session having finished.
///
/// Measured on **Claude Code 2.1.228**, the same way [`codex_state`] names its version:
/// this is somebody else's schema, and the next person to doubt these strings needs to
/// know what they were checked against. Both `agent_completed` and `agent_needs_input`
/// come from one agent-session state machine, keyed on a subagent's own label and session
/// id; `notification_type` there is one of `permission_prompt`, `idle_prompt`,
/// `auth_success`, `elicitation_dialog`, `elicitation_complete`, `elicitation_response`,
/// `agent_needs_input`, `agent_completed`. A rename lands on the `_` arm and reports
/// nothing, which is the safe direction to fail.
///
/// `agent_needs_input` is the deliberate asymmetry and stays [`State::Blocked`]: a
/// subagent that needs an answer is a real claim on the user, wherever in the session
/// it came from, and dropping it would lose the one thing the badge exists for. The
/// test is *"would the user do something about it"*, not *"which agent produced it"* —
/// which is why the two halves of the same producer split.
///
/// The session's own turn boundaries are `UserPromptSubmit`/`Stop`, and the subagent
/// counterparts (`SubagentStart`, `SubagentStop`) get their own [`State::Unknown`] arm
/// rather than being left to the fall-through. That arm is a guard, not decoration:
/// adding `SubagentStop` to `Stop`'s arm — the natural mistake, since it reads like the
/// same event — makes the later pattern unreachable, and `unreachable_patterns` is denied
/// by CI's `-D warnings`. So the mistake fails the build instead of quietly reinstating
/// this bug.
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
            Some("idle_prompt") => State::Idle,
            // **A subagent, not the session** — see this function's "A subagent's turn is
            // not the session's turn". Deliberately `Unknown`, which sends nothing at all.
            Some("agent_completed") => State::Unknown,
            _ => State::Unknown,
        },
        // The turn ended without a notification. Redundant with `idle_prompt` on a
        // Claude that sends one, and the only signal on a Claude that does not —
        // the two collapse to the same state, so a duplicate costs nothing.
        //
        // `Stop` is the *session's* turn ending. Its subagent counterparts are matched
        // below and answer `Unknown`, so this arm cannot widen by accident.
        "Stop" => State::Idle,
        // A subagent's lifecycle, which is not this pane's state — see this function's
        // "A subagent's turn is not the session's turn". Neither is installed; matched
        // here for the same reason `SessionStart` is, so the receiving end is never
        // narrower than a future installer.
        //
        // **Not dead code, even though `_` answers `Unknown` too.** This arm is what
        // makes folding a subagent event into a session-state arm a *compile* failure
        // rather than a silent regression: `"Stop" | "SubagentStop" => State::Idle` above
        // makes this pattern unreachable, and `unreachable_patterns` is denied by CI's
        // `-D warnings`. That is the whole reason to spell it out — measured, not assumed.
        "SubagentStart" | "SubagentStop" => State::Unknown,
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
/// `notify` fires on exactly one event, `agent-turn-complete` — no approval-request,
/// no turn-started. **This is a choice of mechanism, not a fact about Codex**: Codex
/// also ships a newer `hooks` system (`pre_tool_use`, `permission_request`,
/// `user_prompt_submit`, `session_end`, …) that mirrors Claude's own event names
/// closely enough to suggest deliberate compatibility. Codex genuinely has the
/// richer signals `--enable`ing that system would need. What it does not have is a
/// way to use them for free: a hook installed via `-c hooks.*=…` needs interactive
/// **trust review** the first time Codex sees it ("New hook — review required"),
/// or `--dangerously-bypass-hook-trust`, which is not scoped to veld's own hook — it
/// disables trust review for *every* configured hook for that invocation, described
/// by Codex's own `--help` as "DANGEROUS… intended only for automation that already
/// vets hook sources". Neither is compatible with an ephemeral, invisible wrapper:
/// the first is a security prompt the user never asked for, appearing because veld
/// silently added a hook to their session; the second is a blanket bypass veld would
/// be injecting on every launch, for hooks that are not veld's to vouch for.
///
/// `notify`/`legacy_notify` has neither problem — it takes effect immediately, no
/// review, no bypass flag — at the cost of the one event it fires. veld takes that
/// trade deliberately: a narrower badge over a security prompt or a standing bypass.
/// If Codex ever offers a way to pre-trust a single named hook non-interactively,
/// this is the function to widen — measured at codex-cli 0.146.0, and worth
/// re-checking against whatever version is current before concluding it still holds.
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

/// The state a Pi extension event reports.
///
/// # Why there is no `Blocked`
///
/// Pi's own docs are explicit that it "intentionally does not include built-in …
/// permission popups, plan mode, to-dos" — there is no equivalent of Claude's
/// `permission_prompt`/`agent_needs_input` or Codex's approval flow to observe. An
/// extension *could* add a confirmation dialog (`ctx.ui.confirm`) around `tool_call`,
/// but that would be a behaviour change layered on top of a vanilla session, not a
/// signal [`pi_extension_doc`]'s generated extension can read — it has no visibility
/// into some *other* extension's UI state. So this can only ever return `Working`,
/// `Idle`, `Done`, or `Unknown`, the same shape [`codex_state`] settled on for the
/// same reason: a narrower badge over inventing a signal that is not really there.
///
/// There is also no [`claude_state`]-style sub-agent carve-out to get wrong here:
/// Pi's own docs say it has no sub-agent concept at all ("intentionally does not
/// include built-in MCP, sub-agents, …"), so there is no second lifecycle a future
/// event could conflate with the session's own turn.
///
/// # `agent_start`/`agent_settled`, not `turn_start`/`turn_end`
///
/// Pi's lifecycle has two nested levels. A **turn** is one LLM response plus the
/// tool calls it made (`turn_start` … `turn_end`); an **agent run** is the whole
/// processing of one user prompt (`agent_start` … `agent_end` … `agent_settled`), and
/// it contains as many turns as the run needs while it calls tools. `turn_start`/
/// `turn_end` therefore fire **once per step**, not once per prompt — which is exactly
/// the per-step "finished" spam this badge must not reproduce (a run that calls ten
/// tools filed ten "agent finished" events). The run-level pair is the right
/// granularity: `agent_start` is when the agent starts working, and `agent_settled` is
/// the documented signal that the run is "fully settled; no automatic retry, compaction
/// retry, or queued follow-up messages remain" — the one event that means the agent is
/// genuinely idle waiting for the next prompt, the same shape as Claude's
/// `UserPromptSubmit`/`Stop`. `agent_end` is deliberately not used for the same reason
/// `turn_end` is not: Pi may still auto-retry, auto-compact and retry, or continue with
/// queued follow-up messages after it, so it is not "done" either.
///
/// Neither event is on a blocking path veld has to bound: the generated extension
/// spawns its reporter and returns without awaiting it ([`pi_extension_doc`]), so a
/// hung `veld` binary cannot hold Pi's run open regardless of whether Pi itself
/// awaits the handler.
///
/// # `session_shutdown` only reports `Done` for `"quit"`
///
/// The event fires for `"quit"`, `"reload"`, `"new"`, `"resume"`, and `"fork"` — only
/// the first is the session actually ending. `/new`/`/resume`/`/fork` all shut down
/// the current session and immediately start a different one in the same pane, so
/// mapping every reason to `Done` would badge a pane that is still live between one
/// session ending and the next one's own `Ready` (the wrapper only fires that once,
/// at process launch — a session switch inside one long-running `pi` process gets no
/// second `Ready`). `"reload"` is `/reload`'s extension hot-reload, not a session
/// boundary at all.
///
/// Measured at **pi-coding-agent 0.84.1** — the version to recheck this against if
/// `agent_start`/`agent_settled`/`session_shutdown` are ever renamed or regrouped.
#[must_use]
pub fn pi_state(payload: &HookPayload) -> State {
    match payload.event.as_deref() {
        Some("agent_start") => State::Working,
        Some("agent_settled") => State::Idle,
        Some("session_shutdown") => match payload.reason.as_deref() {
            Some("quit") => State::Done,
            _ => State::Unknown,
        },
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
/// The value Codex parses is TOML, but every element here is either an absolute path
/// or one of a handful of ASCII flag names this crate controls — never arbitrary
/// user text — and for that controlled set, JSON's basic-string escaping
/// (`\\`/`\"`/control characters) and TOML's agree closely enough that
/// `serde_json::to_string` on a `&str`, infallible and free, does the job without a
/// second hand-rolled escaper to keep in sync with [`sh_quote`]. This is **not** a
/// claim that JSON and TOML string escaping are interchangeable in general — U+007F
/// (DEL) is the one character TOML forbids unescaped that JSON does not escape, and
/// it is excluded here only by the inputs being what they are, not by construction.
/// A future caller feeding this function less controlled input (raw user text, say)
/// should not lean on this comment as proof the encoding is safe.
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
    // The prefix comes from `AgentTool::Codex.injection()` rather than a second
    // `"notify="` literal: the wrapper's `value_guard` checks a value against that
    // same prefix before ever handing it to the real binary, and two independent
    // literals here would let one drift from the other with every test still green
    // — the fake `veld` those tests use to stand in for this function hardcodes the
    // same string, which would hide exactly that drift.
    let (_, Injection::ConfigOverride { key_prefix }) = AgentTool::Codex.injection() else {
        unreachable!("Codex is a ConfigOverride tool")
    };
    format!("{key_prefix}[{elements}]")
}

/// The ephemeral extension module handed to `pi -e`.
///
/// # Why an extension file and not a settings-file hook
///
/// Pi has no equivalent of Claude's `hooks` key or Codex's `notify`/`hooks` config —
/// its `settings.json` carries model, UI and tooling preferences, nothing that runs a
/// command on a lifecycle event. What it has instead is an extension API
/// (`pi.on(event, handler)`) reached by a JS/TS module, auto-discovered from
/// `~/.pi/agent/extensions/`/`.pi/extensions/` or loaded ad hoc with `-e`/`--extension
/// <path>` — documented for exactly this ("quick tests" without auto-discovery) and,
/// per Pi's own docs, participating in `project_trust` only as a non-deciding
/// bystander: a CLI `-e` extension never triggers the interactive trust prompt that
/// gated Codex's richer `hooks` system out of [`codex_state`]. So this is a file, the
/// same shape as [`Injection::SettingsFile`], carrying code instead of JSON.
///
/// # Why the reporting call is never awaited
///
/// The generated handler calls `child_process.execFile` and returns without awaiting
/// the callback, so the handler's own promise resolves immediately regardless of how
/// long (or whether) the spawned `veld agent-state` finishes — the same trade
/// [`codex_notify_config`] takes for the same reason, just enforced in JS rather than
/// by Codex's own `spawn()` never being awaited. That holds for every event here:
/// `agent_start`/`agent_settled` are run lifecycle events, and `session_shutdown` is
/// one Pi *does* await before actually shutting down — which is why the handler must
/// not await anything itself. [`HOOK_TIMEOUT_SECS`] still bounds the child process
/// itself (`execFile`'s own `timeout`), so a hung `veld` binary cannot hold Pi's
/// shutdown open either.
///
/// # Why the payload rides on `argv`, not `stdin`
///
/// Nothing here reads anybody else's schema — this module writes both the extension
/// and [`pi_state`]'s reader — so the payload shape is a free choice, made to match
/// Codex's rather than Claude's: a small JSON object as `agent-state`'s final
/// argument, never on stdin. `veld agent-state`'s stdin path stays Claude-only.
///
/// # Escaping
///
/// `cli` and `session_id` are embedded as JS string literals via
/// `serde_json::to_string`, the same trick [`codex_notify_config`] uses to get
/// JSON-safe escaping essentially for free; a JSON string literal is also a valid JS
/// string literal, so there is no second escaper to keep in sync with [`sh_quote`].
#[must_use]
pub fn pi_extension_doc(cli: &Path, session_id: &str) -> String {
    let cli_js = serde_json::to_string(&cli.to_string_lossy().into_owned())
        .expect("a String serializes to JSON infallibly");
    let session_js =
        serde_json::to_string(session_id).expect("a String serializes to JSON infallibly");
    format!(
        r#"// pi-veld-activity-reporter — generated by veld, rewritten on every launch.
// Reports this session's run/shutdown lifecycle to Veld's activity badge. Never edit by hand.
import {{ execFile }} from "node:child_process";

const CLI = {cli_js};
const SESSION = {session_js};

function report(event, reason) {{
  const body = JSON.stringify(reason === undefined ? {{ event }} : {{ event, reason }});
  execFile(
    CLI,
    ["agent-state", "--tool", "pi", "--session", SESSION, body],
    {{ timeout: {timeout_ms} }},
    () => {{}},
  );
}}

export default function (pi) {{
  pi.on("agent_start", async () => {{ report("agent_start"); }});
  pi.on("agent_settled", async () => {{ report("agent_settled"); }});
  pi.on("session_shutdown", async (event) => {{ report("session_shutdown", event.reason); }});
}}
"#,
        cli_js = cli_js,
        session_js = session_js,
        timeout_ms = HOOK_TIMEOUT_SECS * 1000,
    )
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
///
/// The extension is per tool, not one hardcoded `.json`, because [`AgentTool::Pi`]'s
/// [`Injection::SettingsFile`] file is JS/TS source Pi's loader (`jiti`) resolves by
/// extension — a `.json` file handed to `pi -e` would not load as an extension at
/// all. Never called for [`AgentTool::Codex`], whose [`Injection`] is
/// [`Injection::ConfigOverride`] and has no file — panics rather than returning a
/// plausible-looking `.json` path nothing would ever read, the same "loud failure
/// beats a silent wrong answer" choice [`codex_notify_config`]'s own `unreachable!`
/// makes for the same invariant.
///
/// Pi's file stem is `pi-veld-activity-reporter`, not just `tool.as_str()` — unlike
/// Claude's settings file and Codex's literal `-c` value, Pi's is a **module** that
/// can surface in Pi's own extension listing or an error message, so its name should
/// say what it is on sight rather than reading as an unlabelled `pi-<session>.ts`.
#[must_use]
pub fn settings_path(shim_dir: &Path, tool: AgentTool, session_id: &str) -> PathBuf {
    let (stem, ext) = match tool {
        AgentTool::Claude => (tool.as_str(), "json"),
        AgentTool::Pi => ("pi-veld-activity-reporter", "ts"),
        AgentTool::Codex => unreachable!("Codex is ConfigOverride and has no settings file"),
    };
    shim_dir
        .join("agent")
        .join(format!("{stem}-{}.{ext}", sanitize(session_id)))
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
            ..Default::default()
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
        assert_eq!(claude_state(&payload("Stop", None, None)), State::Idle);
    }

    /// A subagent ending is not the session ending and reports nothing at all — while a
    /// subagent *asking* still does, which is the one exception and is asserted here
    /// beside the rule rather than left to a reader to discover.
    ///
    /// Two bugs in one arm, both reported from real use. `agent_completed` mapped to
    /// [`State::Idle`], so a subagent ending (a) filed an "Agent finished" the user had
    /// no reason to act on and (b) cleared the pane's working flag — nothing reports a
    /// turn *starting* except `UserPromptSubmit`, so the spinner stayed dead for the rest
    /// of a turn that was still running.
    ///
    /// `Unknown` and not some new state on purpose: the CLI drops `Unknown` without
    /// contacting the daemon, so this is the only answer that touches neither the inbox
    /// nor the pane's working flag. Asserted as the *state* rather than as "no request"
    /// because that is where the decision lives; `agent-state`'s own early return is what
    /// turns it into silence.
    #[test]
    fn a_subagent_ending_reports_nothing_but_one_asking_still_does() {
        // One notification for both outcomes — Claude's message is `"<label> finished"`
        // or `"<label> failed"` — so the old mapping also reported a subagent's failure
        // as the session having finished.
        assert_eq!(
            claude_state(&payload("Notification", Some("agent_completed"), None)),
            State::Unknown,
            "a subagent finishing is neither an event for the user nor the pane's state"
        );
        for event in ["SubagentStart", "SubagentStop"] {
            assert_eq!(
                claude_state(&payload(event, None, None)),
                State::Unknown,
                "{event}: a subagent's lifecycle is not the session's"
            );
        }
        // The deliberate asymmetry: the *other* half of the same producer survives,
        // because a subagent waiting on an answer is still the user's to answer.
        assert_eq!(
            claude_state(&payload("Notification", Some("agent_needs_input"), None)),
            State::Blocked
        );
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
             turn; PermissionRequest blocks with nothing to contribute; SubagentStart and \
             SubagentStop report a subagent's turn, which is not this pane's state"
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

    /// A run starting and settling, and only `"quit"` ending the session.
    #[test]
    fn pi_reports_agent_runs_and_only_a_real_quit_as_done() {
        let event = |name: &str, reason: Option<&str>| HookPayload {
            event: Some(name.to_owned()),
            reason: reason.map(str::to_owned),
            ..Default::default()
        };
        assert_eq!(pi_state(&event("agent_start", None)), State::Working);
        assert_eq!(pi_state(&event("agent_settled", None)), State::Idle);
        assert_eq!(
            pi_state(&event("session_shutdown", Some("quit"))),
            State::Done
        );
        // `/new`, `/resume`, `/fork` and a dev `/reload` all fire the same event —
        // none of them means this pane's agent is gone.
        for reason in ["reload", "new", "resume", "fork"] {
            assert_eq!(
                pi_state(&event("session_shutdown", Some(reason))),
                State::Unknown,
                "{reason}"
            );
        }
        assert_eq!(pi_state(&event("session_shutdown", None)), State::Unknown);
    }

    /// A turn ending is not a run settling: a run that calls tools fires `turn_end`
    /// after **every** step, long before the agent is done. Mapping it to `Idle` is
    /// what filed an "agent finished" notification for each step of a run that was
    /// still working — the bug this pair was switched away from. The run-level
    /// `agent_settled` is the only end-of-run signal ("no retry, compaction retry, or
    /// queued follow-up remains"), and `agent_end` is not done either for the same
    /// reason.
    #[test]
    fn a_turn_end_is_not_a_finished_agent() {
        let event = |name: &str| HookPayload {
            event: Some(name.to_owned()),
            ..Default::default()
        };
        // The per-step events this integration deliberately does not listen to report
        // nothing rather than a premature "finished".
        assert_eq!(pi_state(&event("turn_start")), State::Unknown);
        assert_eq!(pi_state(&event("turn_end")), State::Unknown);
        assert_eq!(pi_state(&event("agent_end")), State::Unknown);
        // And the real pair still works.
        assert_eq!(pi_state(&event("agent_start")), State::Working);
        assert_eq!(pi_state(&event("agent_settled")), State::Idle);
    }

    /// Nothing Pi has not been told to send is a guess.
    #[test]
    fn an_unrecognised_pi_event_is_unknown_not_a_guess() {
        let event = HookPayload {
            event: Some("invented_later".to_owned()),
            ..Default::default()
        };
        assert_eq!(pi_state(&event), State::Unknown);
        assert_eq!(pi_state(&HookPayload::default()), State::Unknown);
    }

    /// The generated extension names the CLI and session literally, subscribes to
    /// exactly the three events `pi_state` understands, and never awaits the process
    /// it spawns to report them.
    #[test]
    fn the_extension_module_bakes_the_cli_and_session_and_never_awaits_the_report() {
        let doc = pi_extension_doc(Path::new("/opt/veld/bin/veld"), "pane-7");
        assert!(
            doc.contains(r#"const CLI = "/opt/veld/bin/veld";"#),
            "{doc}"
        );
        assert!(doc.contains(r#"const SESSION = "pane-7";"#), "{doc}");
        for event in ["agent_start", "agent_settled", "session_shutdown"] {
            assert!(
                doc.contains(&format!(r#"pi.on("{event}""#)),
                "{event} missing: {doc}"
            );
        }
        // The per-step events this integration does not listen to must not be wired.
        for event in ["turn_start", "turn_end", "agent_end"] {
            assert!(
                !doc.contains(&format!(r#"pi.on("{event}""#)),
                "{event} must not be subscribed: {doc}"
            );
        }
        assert!(
            doc.contains("agent-state"),
            "the generated command must call agent-state: {doc}"
        );
        assert!(
            doc.contains("--tool") && doc.contains("\"pi\""),
            "the payload must name the tool: {doc}"
        );
        // Fire-and-forget: a callback, not an `await`, on the spawn itself.
        assert!(
            !doc.contains("await execFile"),
            "awaiting the spawned process would make session_shutdown wait on veld: {doc}"
        );
        assert!(doc.contains("timeout"), "{doc}");
    }

    #[test]
    fn a_quote_in_a_pi_session_id_cannot_break_out_of_its_js_string_literal() {
        let doc = pi_extension_doc(Path::new("/a b/veld"), r#"it"s"#);
        assert!(doc.contains(r#"const SESSION = "it\"s";"#), "{doc}");
    }

    #[test]
    fn each_tool_carries_its_own_injection_shape() {
        assert_eq!(
            AgentTool::Claude.injection(),
            ("--settings", Injection::SettingsFile)
        );
        assert_eq!(
            AgentTool::Codex.injection(),
            (
                "-c",
                Injection::ConfigOverride {
                    key_prefix: "notify="
                }
            )
        );
        assert_eq!(AgentTool::Pi.injection(), ("-e", Injection::SettingsFile));
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
        // Pi's file is `.ts`, not `.json` — Pi's loader resolves an extension module
        // by extension, and a `.json` file handed to `pi -e` would not load as one.
        // Its stem also names what it is, not just the tool — this file can surface
        // in Pi's own extension listing or an error message.
        assert_eq!(
            settings_path(dir, AgentTool::Pi, "abc-1"),
            PathBuf::from("/tmp/shims/agent/pi-veld-activity-reporter-abc-1.ts")
        );
    }

    #[test]
    fn tools_and_states_round_trip_through_their_spelling() {
        for tool in AgentTool::ALL.iter().copied() {
            assert_eq!(AgentTool::parse(tool.as_str()), Some(tool));
        }
        assert_eq!(AgentTool::parse("codex"), Some(AgentTool::Codex));
        assert_eq!(AgentTool::parse("pi"), Some(AgentTool::Pi));
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
