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
//! # The second thing the wrapper carries: what the agent is told
//!
//! The same wrapper also hands the agent [`agent_context`] — a few lines, static per
//! daemon instance, saying it is in a Veld terminal and that `veld ide open` (spelled as
//! *this* daemon's CLI — [`context_cli_word`]) puts a file in front of the human.
//! It rides the tool's own *append* mechanism ([`ContextInjection`]): Claude's
//! `--append-system-prompt`, Codex's `-c developer_instructions=…`, and a second `pi -e`
//! extension that chains onto the assembled prompt. Gated on `VELD_AGENT_CONTEXT`
//! (`terminal.agentContext`), independent of the hooks' `VELD_AGENT_HOOKS` — and under
//! the same invariants: plain interactive launch only, the user's own colliding flag
//! wins, nothing of theirs is edited.
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
//!    `extra_interactive_first_words`, `context_injection`, `own_context_flag_patterns`
//!    today, and whatever this list has grown to by
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
//! 5. Docs: the two settings rows, README, the relevant `veld skills` topic
//!    (`crates/veld/src/commands/skills/`), `llms-full.txt`.
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

    /// How the context `text` ([`agent_context`]) reaches this tool's invocation. See
    /// [`ContextInjection`].
    #[must_use]
    pub fn context_injection(self, text: &str) -> ContextInjection {
        match self {
            // **Appends**, by the flag's own definition ("Append a system prompt to the
            // default system prompt", `claude --help`, 2.1.291), so the user's own
            // `CLAUDE.md` and output style are untouched. Not `--append-system-prompt-file`:
            // `--help` does not list it, and a temp file buys nothing for static text.
            Self::Claude => ContextInjection::Argv {
                flag: "--append-system-prompt",
                value: text.to_owned(),
                user_config_key: None,
            },
            // A `-c` override like `notify`, with the same property and the same cost:
            // nothing of the user's is edited, and a `developer_instructions` they set
            // themselves would be *replaced* for the launch — which, unlike a silenced
            // notifier, would quietly take their own instructions away. So the wrapper
            // reads their `config.toml` for the key and steps out if it is there; see
            // [`ContextInjection::Argv::user_config_key`]. The key was confirmed as a
            // top-level `ConfigToml` field in the codex-cli 0.160.1 binary; codex is not
            // installed where this was written, so the effect on a session was not driven.
            Self::Codex => ContextInjection::Argv {
                flag: "-c",
                value: codex_context_config(text),
                user_config_key: Some((
                    "${CODEX_HOME:-$HOME/.codex}/config.toml",
                    "developer_instructions",
                )),
            },
            // A second generated extension, static, beside the per-session reporter —
            // see [`pi_context_extension_doc`] for why it is its own file.
            Self::Pi => ContextInjection::ExtensionFile { flag: "-e" },
        }
    }

    /// Shell `case` patterns for a user flag that collides with the *context* injection
    /// specifically, on top of [`Self::own_injection_flag_patterns`] (which steps out of
    /// both). Empty when nothing beyond those collides.
    ///
    /// Separate because the collisions are separate: a user's own
    /// `--append-system-prompt` does not touch the hooks `--settings` carries, so it must
    /// not cost the badge — it only means veld's text stays out of a prompt the user is
    /// already shaping by hand. `--system-prompt` too: appending to a prompt somebody
    /// replaced on purpose is adding to something they took ownership of.
    #[must_use]
    pub fn own_context_flag_patterns(self) -> &'static str {
        match self {
            Self::Claude => {
                "--append-system-prompt | --append-system-prompt=* | --append-system-prompt-file \
                 | --append-system-prompt-file=* | --system-prompt | --system-prompt=* \
                 | --system-prompt-file | --system-prompt-file=*"
            }
            // Every `-c` already steps out of everything (see
            // `own_injection_flag_patterns`), which covers a user's own
            // `-c developer_instructions=…`.
            Self::Codex => "",
            // The extension *chains* onto whatever prompt Pi assembled, `--system-prompt`
            // and `--append-system-prompt` included, so there is nothing to collide with.
            Self::Pi => "",
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

    /// Whether this tool's wrapper hands it a launch prompt as its startup argument
    /// (see [`LAUNCH_PROMPT_ENV`]).
    ///
    /// **Claude only, because Claude is the one measured.** Claude Code 2.1.291 takes
    /// `claude -- "<prompt>"`, opens with that message already submitted, and reads an
    /// `@"<path>"` in it as an attachment — an image arrives as an image, with no tool
    /// call and no permission prompt, even outside the working directory. Codex takes a
    /// positional prompt too, but an image there has to travel as `-i <path>` rather
    /// than in the text, which is a different composition this wrapper does not do;
    /// Pi was not measured. For both, the prompt stays where it was — typed in by the
    /// window once the agent opens its input — which is exactly what an unclaimed
    /// launch prompt falls back to anyway.
    #[must_use]
    pub fn takes_launch_prompt(self) -> bool {
        matches!(self, Self::Claude)
    }

    /// The flags of this tool that always take one value, as a shell `case` pattern —
    /// how the wrapper tells a flag's value from a prompt the pane's own command
    /// already passes (see `agent_script` in `veld-daemon/src/pty/shims.rs`).
    ///
    /// **An allowlist, so a miss is safe.** A bare word after anything not listed —
    /// a boolean flag (`--dangerously-skip-permissions "Read AGENTS.md"`), a flag
    /// whose value is optional (`--resume`, `--debug`), a second value of a variadic
    /// one — is taken for a standing prompt, and the wrapper leaves the launch prompt
    /// for the window to type rather than append a second positional the agent would
    /// silently drop. From `claude --help` (2.1.291): every `<value>` flag, none of the
    /// `[value]` ones. Empty for a tool that does not take a launch prompt.
    #[must_use]
    pub fn launch_prompt_value_flags(self) -> &'static str {
        match self {
            Self::Claude => {
                "--add-dir | --agent | --agents | --allowedTools | --allowed-tools \
                 | --append-system-prompt | --append-system-prompt-file | --autocompact \
                 | --betas | --debug-file | --disallowedTools | --disallowed-tools \
                 | --effort | --environment | --fallback-model | --file | --input-format \
                 | --json-schema | --max-budget-usd | --mcp-config | --model | -n | --name \
                 | --output-format | --permission-mode | --permission-prompts | --plugin-dir \
                 | --plugin-url | --remote-control-session-name-prefix | --session-id \
                 | --setting-sources | --settings | --system-prompt | --system-prompt-file \
                 | --system-prompt-snapshot | --tools"
            }
            Self::Codex | Self::Pi => "",
        }
    }
}

/// The variable that tells an agent wrapper where its launch prompt is waiting.
///
/// Set by the daemon on a config pane's **fresh** launch when the New worktree dialog
/// sent a prompt with it, and only then. The value is a *path*, never the text: the
/// variable is inherited by everything the pane's shell starts, and a prompt in the
/// environment would be readable by all of it. Every wrapper unsets it before `exec`,
/// whether or not it took the prompt, so a nested agent cannot claim it later — but
/// only the wrapper does: the pane's shell, and a command that reaches the agent
/// without it (an absolute path, `npx`), keep the variable for the pane's lifetime.
/// Harmless, because it names a file that is gone once either side claims it.
pub const LAUNCH_PROMPT_ENV: &str = "VELD_LAUNCH_PROMPT";

/// The largest launch prompt handed over as an argument rather than typed.
///
/// A ceiling on `argv`, not on what a user may write: macOS's `ARG_MAX` is 1 MiB for
/// arguments *and* environment together, and a pane's environment is the user's
/// whole login environment. Anything longer is typed in, as before.
pub const MAX_LAUNCH_PROMPT_BYTES: usize = 64 * 1024;

/// How a launch prompt's file name ends. The rest is the paste directory's own
/// 32-hex-character random prefix, so that directory's reaper — which deletes only
/// names of that shape — sweeps a prompt nobody ever claimed.
pub const LAUNCH_PROMPT_SUFFIX: &str = "-launch-prompt.txt";

/// A fresh, unguessable launch-prompt file name.
#[must_use]
pub fn launch_prompt_file_name() -> String {
    format!("{}{LAUNCH_PROMPT_SUFFIX}", uuid::Uuid::new_v4().simple())
}

/// Whether `name` is one [`launch_prompt_file_name`] could have produced.
#[must_use]
pub fn is_launch_prompt_name(name: &str) -> bool {
    name.strip_suffix(LAUNCH_PROMPT_SUFFIX)
        .is_some_and(|hex| hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Claim a launch prompt, at most once across every process that tries.
///
/// **One prompt, two claimants, and exactly one of them may win.** The agent wrapper
/// claims it at launch (through `veld agent-prompt --take`); the window claims it once
/// the agent has opened its input, and types it in if it got it — which it only can
/// if no wrapper took it first. A prompt neither sent nor typed is lost, and one both
/// sent and typed is a second turn the user never asked for, so the claim is a
/// `rename`: atomic on one filesystem, and the loser sees `NotFound`.
///
/// `Ok(None)` means somebody else got there first (or nothing was ever written).
/// Refuses a path whose name this module did not mint, because the CLI half takes
/// the path from its argv and then deletes what it names.
pub fn take_launch_prompt(path: &Path) -> std::io::Result<Option<String>> {
    deliver_launch_prompt(path, |_| Ok(()))
}

/// [`take_launch_prompt`], holding the file until `deliver` has the text.
///
/// Winning the claim is not the same as handing the prompt on: the wrapper's half
/// still has to write it to the pipe its `$(…)` reads, and a claim that deleted
/// first would lose the prompt on a failed write while telling the window the
/// wrapper took it. So the file is removed only once `deliver` succeeds, and put
/// back under its own name when it fails — where the window's later claim finds it
/// and types it in.
pub fn deliver_launch_prompt(
    path: &Path,
    deliver: impl FnOnce(&str) -> std::io::Result<()>,
) -> std::io::Result<Option<String>> {
    use std::io::Read as _;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| is_launch_prompt_name(n))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a launch prompt file")
        })?;
    // Still a paste-shaped name, so a claimed file whose removal fails below is
    // swept like an unclaimed one.
    let claimed = path.with_file_name(format!("{name}.taken-{}", std::process::id()));
    match std::fs::rename(path, &claimed) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut text = String::new();
    let read = std::fs::File::open(&claimed).and_then(|f| {
        f.take(MAX_LAUNCH_PROMPT_BYTES as u64 + 1)
            .read_to_string(&mut text)
    });
    // A file that cannot be read, or is too large to pass, is no use to the
    // window either: gone, not put back.
    let readable = read.and_then(|_| {
        if text.len() > MAX_LAUNCH_PROMPT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "launch prompt is larger than an argument may be",
            ));
        }
        Ok(())
    });
    if let Err(e) = readable {
        let _ = std::fs::remove_file(&claimed);
        return Err(e);
    }
    if let Err(e) = deliver(&text) {
        // If even this fails, the claimed name is still paste-shaped and the
        // reaper sweeps it.
        let _ = std::fs::rename(&claimed, path);
        return Err(e);
    }
    let _ = std::fs::remove_file(&claimed);
    Ok(Some(text))
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
    /// A turn ended while the session still has background agents running, which will
    /// wake it again without the user — so it is still working on the user's behalf, and
    /// the end of this turn is not news. See [`waits_on_background_agent`].
    ///
    /// Not [`Self::Working`]: that also *retracts* an unread "waiting for you", which is
    /// right when the session moved on and wrong here — one background agent asking for
    /// permission must stay amber while the session is woken by another one finishing.
    /// And not [`Self::Unknown`] (nothing at all): the inbox has to *remember* it, because
    /// Claude's `idle_prompt` reminder ([`Self::Settled`]) arrives a minute into the wait
    /// whether or not agents are running, and would stop the spinner for the rest of it.
    Delegated,
    /// The agent is sitting at its prompt and has nothing new to say — it stopped
    /// working, but this is not the news that a turn finished.
    ///
    /// Clears the pane's *working* flag — unless the last turn was [`Self::Delegated`],
    /// whose wait it does not end — and files nothing. Exists because the end of a
    /// turn has two reporters with different reliability: `Stop` is the news ([`Self::Idle`]),
    /// but Claude does not send it for a turn the user **interrupted** (Esc), so an
    /// interrupted turn left the spinner running with nothing to ever stop it. Claude's
    /// later `idle_prompt` reminder is what still arrives, and it maps here: it can stop a
    /// spinner, but as `Idle` it re-filed a "finished" the user had already read, and
    /// filed one for a session only waiting on its background agents.
    ///
    /// Deliberately not [`Self::Ready`], which does the same to the flag: `Ready` also
    /// means "an agent just launched here" and lets the inbox attribute an outstanding
    /// shell command to the agent — a late reminder arriving after the user quit the agent
    /// and started something else would have claimed *that* command.
    ///
    /// Not guaranteed to arrive. Claude skips the reminder for good once the user has
    /// interacted since the turn ended — pressed Esc, started typing a correction, then
    /// walked away without sending it — and a user can raise its threshold arbitrarily.
    /// Then an interrupted turn's spinner runs until the next prompt. Accepted: it is
    /// what this signal can do, not a bug in reading it.
    Settled,
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
            Self::Delegated => "delegated",
            Self::Settled => "settled",
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
            Self::Delegated,
            Self::Settled,
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
    /// `Stop`'s list of the session's background tasks that are still running — see
    /// [`waits_on_background_agent`]. Kept as raw JSON rather than a typed list: it is
    /// the one field here with structure, and a shape this code did not expect must cost
    /// that one field, not turn the whole `Stop` into an unparseable payload.
    #[serde(default)]
    pub background_tasks: Option<serde_json::Value>,
}

/// Whether a `Stop` was sent while the session still has a background agent running —
/// one it will be woken by, without the user, when that agent ends.
///
/// # Why this turns a `Stop` into nothing
///
/// A session that launches background agents usually ends its turn straight away and
/// waits. Claude fires `Stop` for that, and again for every turn that one of the agents'
/// results wakes it into — no `UserPromptSubmit` in between, because nobody typed
/// anything. Each of those was filed as "Agent finished", with a banner: one per agent
/// plus one, for a single request. Reported from real use, and reproduced with two
/// background agents.
///
/// Claude says so itself. Its `Stop` payload carries `background_tasks`, the session's
/// tasks whose `status` is `running` or `pending` and which are backgrounded, each with
/// a `type` — already filtered by Claude, so a foreground agent is never on it; the
/// `status` check here is belt and braces. While one of them is an agent, the turn that just ended is not the session
/// finishing — it is the session waiting on its own work. The `Stop` after the last
/// agent has come back has an empty list, and that one is the real "finished". No state
/// to keep and nothing to pair up: every `Stop` carries the whole current answer.
///
/// It reports [`State::Delegated`], which keeps the "finished" out of the inbox and the
/// spinner on — see that variant for why neither `Working` nor `Unknown` would do.
///
/// # What it costs
///
/// The answer is Claude's own view of its tasks, so a subagent Claude still lists as
/// running — wedged, or killed in a way that never updates its status — keeps every
/// later `Stop` from reading as finished. That is accepted: Claude's own prompt then
/// says it is still waiting for that agent, so veld agrees with the tool rather than
/// guessing past it, and the pane keeps spinning until the next prompt — the same
/// "still waiting" Claude itself shows. A final `Stop` lost on the way (a hook timeout,
/// a daemon restart) costs that turn's "finished" and leaves the spinner on until the
/// next prompt or `SessionEnd`, as a lost `Stop` always did.
///
/// # Which tasks count
///
/// Only `subagent` and `workflow` — work that ends on its own and wakes the session when
/// it does. **Not `shell`**: a background shell is as often a dev server or a watcher as
/// a test run, and one that never ends would swallow every "finished" in the pane for as
/// long as it lives. Not `monitor` or `teammate` either — both are built to stay alive
/// for the session, so they have the shell's problem by design — and not Claude's own
/// housekeeping (`dream`, `auto-mode scan`, `memory import`), which never wakes it with
/// a result. `MCP task` and `cloud session` could join the list; they are left out until
/// someone measures that they end. The known residual: a turn woken by one of those
/// still files its own "finished", as every turn did before. A type this list has not heard of does not count, so the failure mode of
/// a new one is the old behaviour — a `Stop` that badges — rather than a pane that never
/// finishes.
///
/// # What this was not, and why
///
/// `SubagentStart`/`SubagentStop` look like the obvious source, and Claude's hook
/// reference documents a `run_in_background` field on both. **Claude Code 2.1.286 does
/// not send it**: its `SubagentStart` carries `agent_id` and `agent_type` only, and its
/// `SubagentStop` adds the same `background_tasks` list. A first version of this fix
/// built on the documented field, tracked nothing, and was caught only by driving it.
///
/// Measured by reading the payload construction in **Claude Code 2.1.286**: the `type`
/// spellings come from its own table (`local_agent` → `subagent`, `local_workflow` →
/// `workflow`, `local_bash` → `shell`, …), and the filter is `running`/`pending` and not
/// explicitly un-backgrounded. Recheck against the current version if this stops working.
#[must_use]
pub fn waits_on_background_agent(payload: &HookPayload) -> bool {
    let Some(serde_json::Value::Array(tasks)) = &payload.background_tasks else {
        return false;
    };
    tasks.iter().any(|task| {
        let field = |name: &str| task.get(name).and_then(serde_json::Value::as_str);
        matches!(field("type"), Some("subagent" | "workflow"))
            && matches!(field("status"), Some("running" | "pending"))
    })
}

/// The state a Claude Code hook payload reports.
///
/// # The `idle_prompt` question, which the brief got backwards — twice
///
/// `idle_prompt` is not a request for attention: it is "Claude is waiting for your
/// input", sent once the session has sat at its prompt for a while. Treating it as
/// `Blocked` is what produced the notorious attention-after-every-turn badge. Only a
/// real permission prompt, a question, or an elicitation dialog means the user is being
/// *waited on*.
///
/// It was then mapped to [`State::Idle`], on the theory that it is the end of a turn.
/// It is not one — it is a *reminder* about a turn that already ended, and `Stop`
/// already reported that turn, on every Claude this integration supports. So it could
/// only ever repeat news, and it repeated it wrongly in two ways: a "finished" the user
/// had already read came back a minute later, and a session waiting on its background
/// agents — which `Stop` correctly reports as still working, see
/// [`waits_on_background_agent`] — was filed as finished anyway, because this
/// notification carries no task list to tell the two apart. So it is now
/// [`State::Settled`]: the news that a turn ended is `Stop`'s to report, and only
/// `Stop`'s, while this keeps the one job nothing else does — Claude sends no `Stop` for
/// a turn the user interrupted, and this reminder is what stops that turn's spinner.
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
            // A reminder about a turn `Stop` already reported — or the only word on one
            // the user interrupted, which gets no `Stop`. See the note above.
            Some("idle_prompt") => State::Settled,
            // **A subagent, not the session** — see this function's "A subagent's turn is
            // not the session's turn". Deliberately `Unknown`, which sends nothing at all.
            Some("agent_completed") => State::Unknown,
            _ => State::Unknown,
        },
        // The turn ended — the news of it (see the note above on `idle_prompt`). Unless
        // the session is only waiting on its own background agents, which will wake it
        // again without the user: then this is not news, and the pane is still busy.
        //
        // `Stop` is the *session's* turn ending. Its subagent counterparts are matched
        // below and answer `Unknown`, so this arm cannot widen by accident.
        "Stop" => {
            if waits_on_background_agent(payload) {
                State::Delegated
            } else {
                State::Idle
            }
        }
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

/// What an agent in a Veld terminal is told about the terminal it is in, naming the
/// command to run as `cli` — see [`context_cli_word`] for which spelling that is.
///
/// # Static per daemon instance, on purpose
///
/// **Byte-identical in every session one daemon spawns** — no session id, no worktree,
/// no cwd. Three reasons, each enough: a system prompt is the head of every request, so
/// a per-session byte here defeats prompt caching for the whole conversation; Claude
/// records the prompt once per conversation and replays the record on resume
/// (`--system-prompt-snapshot`, default on), so anything dynamic would be stale by the
/// second launch anyway; and everything an agent needs to act on (which terminal, which
/// worktree) `veld ide open` already resolves from `VELD_PTY_SESSION` at run time.
///
/// The one thing that is *not* the same everywhere is the CLI it names, and that is
/// not optional. The text first said a bare `veld`, and in a dev stack's terminal that
/// is the **installed** release on `PATH` — which talks to the installed daemon and,
/// in the report that found this, had no `ide` command at all. The hooks never had the
/// problem because they name the daemon's own CLI by absolute path; this now does the
/// same. It varies by instance only, so the cache argument above still holds.
///
/// Short, and it points at `skills ide` rather than explaining: this is paid for on
/// every turn by every session, including the ones that never open a file.
#[must_use]
pub fn agent_context(cli: &str) -> String {
    format!(
        "You are running inside a Veld IDE terminal. To show the human a file (markdown, CSV, \
         code) or a web page next to this terminal, run `{cli} ide open <path-or-url>[:line] \
         --quiet`, or `--notify` when they should stop and read it. Use it for deliverables, \
         not for every file you touch. When you come across work that is separate from \
         the task at hand and deserves its own branch, offer to hand it off instead of doing \
         it or noting it for later: once the human agrees, `{cli} worktree new` creates a \
         worktree where an agent starts on a prompt you write, for them to take over. \
         Details: `{cli} skills ide`."
    )
}

/// How [`agent_context`] spells the CLI: the bare word `veld` when this daemon is the
/// installed one, else the absolute path of the CLI that belongs to it, shell-quoted
/// when it needs to be (a home directory with a space in it).
///
/// `installed` is [`crate::db::Db::uses_installed_database`] — the existing answer to
/// "is this process the real one, not a cargo build or a `VELD_DB_PATH` sandbox", and
/// the same question the CLI asks before trusting the daemon on the default port. Not a
/// `PATH` lookup: what `PATH` says in the daemon's environment is not what it says in a
/// user's shell. The bare word is preferred where it is right because it is what a
/// human reading the transcript would type, and the installed `veld` is the one on
/// their `PATH` by construction of the installer.
#[must_use]
pub fn context_cli_word(cli: &Path, installed: bool) -> String {
    if installed {
        return "veld".to_owned();
    }
    let path = cli.to_string_lossy();
    let plain = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@%:,=".contains(c));
    if plain {
        path.into_owned()
    } else {
        sh_quote(&path)
    }
}

/// How [`agent_context`]'s text is handed to one tool. Independent of [`Injection`], and of
/// `VELD_AGENT_HOOKS`: either can be on without the other, so each has its own gate
/// (`VELD_AGENT_CONTEXT` for this one) and its own argv.
///
/// Unlike the hooks, the text is the same for every session of a daemon, so it is baked
/// into the generated wrapper at daemon start instead of fetched from `veld agent-settings` at
/// launch — one fewer process on the launch path and nothing that can print garbage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextInjection {
    /// `<flag> <value>` ahead of the user's argv.
    Argv {
        flag: &'static str,
        value: String,
        /// `(config file as a shell word, key)`: when the user's own config sets
        /// `key`, the override would replace theirs, so the wrapper leaves the context
        /// out. A plain line scan with shell builtins, not a TOML parse — any line
        /// mentioning the key counts, which errs toward injecting nothing. It covers the
        /// user's global config and its profiles, **not** a project's `.codex/config.toml`
        /// found by walking up from the cwd; that one is replaced silently, the same
        /// documented cost `notify` has.
        user_config_key: Option<(&'static str, &'static str)>,
    },
    /// `<flag> <path>` to [`context_extension_path`], checked for existence at launch.
    ExtensionFile { flag: &'static str },
}

/// The literal value handed to Codex's `-c` for the context `text`:
/// `developer_instructions="…"`.
///
/// Through [`json_string`], for the reason [`codex_notify_config`] gives — with the one
/// gap that function's docs name closed, since `text` now carries a path.
#[must_use]
pub fn codex_context_config(text: &str) -> String {
    format!("developer_instructions={}", json_string(text))
}

/// A string literal valid as JSON, as a TOML basic string, and as JS.
///
/// `serde_json` escapes `"`, `\` and every control character below U+0020, which TOML
/// and JS read identically. It leaves U+007F (DEL) raw, which TOML forbids in a basic
/// string — harmless while the input was a constant, not once it carries a filesystem
/// path. `\u007F` means the same thing in all three.
fn json_string(s: &str) -> String {
    serde_json::to_string(s)
        .expect("a str serializes to JSON infallibly")
        .replace('\u{7f}', "\\u007F")
}

/// The extension handed to `pi -e` that appends the context `text` to Pi's system
/// prompt — static per daemon, like the text.
///
/// # Why its own file, and not a handler in [`pi_extension_doc`]
///
/// The reporter is per session and only exists when hooks are on; this has to work with
/// hooks off, and has nothing per session in it. `-e` is repeatable and additive (the
/// same measurement [`AgentTool::own_injection_flag_patterns`] relies on), so two files
/// cost nothing — and one static file, rewritten every daemon start, is the shape the
/// text already has.
///
/// # Appended, never replaced
///
/// `before_agent_start` hands each handler the prompt as assembled so far and **chains**
/// whatever `systemPrompt` it returns into the next handler ("If multiple extensions
/// return this, they are chained" — `BeforeAgentStartEventResult`, read in
/// `@mariozechner/pi-coding-agent` 0.73.1's `extensions/types.d.ts` and `runner.js`). So
/// returning `event.systemPrompt` plus a section keeps Pi's own prompt, the user's
/// `--system-prompt`/`--append-system-prompt`, and every other extension's change.
/// Returning the text alone would replace all of it. The `includes` guard keeps a
/// reloaded extension from appending twice. Pi is not installed where this was written,
/// so this was read, not driven.
#[must_use]
pub fn pi_context_extension_doc(text: &str) -> String {
    let text = json_string(text);
    format!(
        r#"// pi-veld-context — generated by veld, rewritten on every daemon start. Never edit by hand.
// Appends Veld's terminal context to the system prompt; never replaces it.
const CONTEXT = {text};

export default function (pi) {{
  pi.on("before_agent_start", async (event) => {{
    if (typeof event.systemPrompt !== "string" || event.systemPrompt.includes(CONTEXT)) return;
    return {{ systemPrompt: event.systemPrompt + "\n\n" + CONTEXT }};
  }});
}}
"#
    )
}

/// Where [`pi_context_extension_doc`] is written, inside this daemon's shim directory.
///
/// **Not under `agent/`**: that directory is swept of files older than
/// [`SETTINGS_MAX_AGE`] by `veld agent-settings`, and this one is written once per
/// daemon start, so a daemon up for a week would lose it.
#[must_use]
pub fn context_extension_path(shim_dir: &Path) -> PathBuf {
    shim_dir.join("context").join("pi-veld-context.ts")
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

    /// The end of a turn is `Stop`'s to report, and it is never attention.
    ///
    /// This is the assertion the whole feature's credibility rests on. Classifying
    /// `idle_prompt` as `Blocked` is what makes a badge that says "needs you" on work
    /// that needs nobody, which is how a user learns to ignore it. It is not `Idle`
    /// either: it is a reminder about a turn `Stop` already reported, so as `Idle` it
    /// re-filed a "finished" the user had read, and filed one for a session that was
    /// only waiting on its background agents — it carries no task list to tell. It is
    /// `Settled`: the spinner stops, nothing is filed.
    #[test]
    fn a_turn_ends_once_through_stop_and_never_as_attention() {
        assert_eq!(claude_state(&payload("Stop", None, None)), State::Idle);
        // Still worth something: it is the only word on a turn the user interrupted,
        // which gets no `Stop`, so it stops the spinner — without filing anything.
        assert_eq!(
            claude_state(&payload("Notification", Some("idle_prompt"), None)),
            State::Settled
        );
        assert_eq!(State::parse("settled"), Some(State::Settled));
        assert_eq!(State::parse("delegated"), Some(State::Delegated));
    }

    /// A `Stop` while a background agent is still running is the session waiting on its
    /// own work, not finishing — the reported bug: two background agents filed three
    /// "Agent finished" banners for one request. `Delegated`, not `Working`, so it cannot
    /// retract another agent's unanswered "waiting for you".
    ///
    /// The payloads are the shape Claude Code 2.1.286 builds, trimmed to what matters.
    #[test]
    fn a_stop_waiting_on_a_background_agent_is_still_working() {
        let stop = |tasks: &str| -> HookPayload {
            serde_json::from_str(&format!(
                r#"{{"hook_event_name":"Stop","stop_hook_active":false,
                    "last_assistant_message":"waiting","background_tasks":{tasks}}}"#
            ))
            .unwrap()
        };
        for tasks in [
            r#"[{"id":"a1","type":"subagent","status":"running","description":"Sleep",
                "agent_type":"general-purpose"}]"#,
            r#"[{"id":"w1","type":"workflow","status":"pending","description":"x","name":"y"}]"#,
            // One agent among other things is enough.
            r#"[{"id":"b1","type":"shell","status":"running","description":"dev","command":"npm run dev"},
                {"id":"a1","type":"subagent","status":"running","description":"Sleep"}]"#,
        ] {
            assert_eq!(claude_state(&stop(tasks)), State::Delegated, "{tasks}");
        }
        for tasks in [
            // The turn after the last agent came back: the real "finished".
            "[]",
            // A background shell can be a server that never ends, so it never holds a
            // pane at working.
            r#"[{"id":"b1","type":"shell","status":"running","description":"dev","command":"npm run dev"}]"#,
            r#"[{"id":"m1","type":"monitor","status":"running","description":"m"}]"#,
            // An agent that is not running any more.
            r#"[{"id":"a1","type":"subagent","status":"completed","description":"Sleep"}]"#,
            // A type this code has never heard of: the old behaviour, not a stuck pane.
            r#"[{"id":"x","type":"invented_later","status":"running"}]"#,
            // A shape this code did not expect costs the field, not the `Stop`.
            r#"{"not":"a list"}"#,
            r#"[42, null, "subagent"]"#,
            "null",
        ] {
            assert_eq!(claude_state(&stop(tasks)), State::Idle, "{tasks}");
        }
        // And only `Stop` reads it: no other event is silenced because of it.
        let mut end = payload("SessionEnd", None, None);
        end.background_tasks = stop(r#"[{"type":"subagent","status":"running"}]"#).background_tasks;
        assert_eq!(claude_state(&end), State::Done);
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
            State::Delegated,
            State::Settled,
            State::Unknown,
        ] {
            assert_eq!(State::parse(state.as_str()), Some(state));
        }
        assert_eq!(State::parse("busy"), None);
    }

    #[test]
    fn a_launch_prompt_is_taken_once_and_then_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(launch_prompt_file_name());
        std::fs::write(&path, "fix it\n\n@\"/tmp/a b.png\"").unwrap();
        assert_eq!(
            take_launch_prompt(&path).unwrap().as_deref(),
            Some("fix it\n\n@\"/tmp/a b.png\"")
        );
        // The second claimant — the window, after the wrapper — gets nothing, and
        // nothing is left behind for the reaper to find.
        assert_eq!(take_launch_prompt(&path).unwrap(), None);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_launch_prompt_that_could_not_be_handed_on_is_put_back_for_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(launch_prompt_file_name());
        std::fs::write(&path, "fix it").unwrap();
        let failed = deliver_launch_prompt(&path, |_| {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fix it");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(
            take_launch_prompt(&path).unwrap().as_deref(),
            Some("fix it")
        );
    }

    #[test]
    fn take_refuses_a_file_this_module_did_not_name() {
        let dir = tempfile::tempdir().unwrap();
        // The CLI half deletes whatever its argv names, so the name is the guard.
        for name in ["id_rsa", "launch-prompt.txt", "0123-launch-prompt.txt"] {
            let path = dir.path().join(name);
            std::fs::write(&path, "x").unwrap();
            assert!(take_launch_prompt(&path).is_err(), "{name}");
            assert!(path.exists(), "{name} was touched");
        }
        assert!(is_launch_prompt_name(&launch_prompt_file_name()));
    }

    #[test]
    fn an_oversized_launch_prompt_is_refused_rather_than_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(launch_prompt_file_name());
        std::fs::write(&path, "x".repeat(MAX_LAUNCH_PROMPT_BYTES + 1)).unwrap();
        assert!(take_launch_prompt(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn only_claude_takes_a_launch_prompt() {
        assert!(AgentTool::Claude.takes_launch_prompt());
        assert!(!AgentTool::Codex.takes_launch_prompt());
        assert!(!AgentTool::Pi.takes_launch_prompt());
    }

    /// The context names the CLI of the daemon that wrote it — the bare `veld` only for
    /// the installed instance — in both places it names one, with the flag pair the CLI
    /// requires. The reported bug: a dev stack's agent was told `veld`, ran the installed
    /// release, and got "no ide command".
    #[test]
    fn the_agent_context_names_this_instances_cli() {
        let dev = Path::new("/Users/me/git/veld/target/debug/veld");
        assert_eq!(context_cli_word(dev, true), "veld");
        assert_eq!(context_cli_word(dev, false), dev.display().to_string());
        // A path with a space is one shell word, so the line still pastes.
        assert_eq!(
            context_cli_word(Path::new("/Users/Jo Doe/bin/veld"), false),
            "'/Users/Jo Doe/bin/veld'"
        );

        let text = agent_context(&context_cli_word(dev, false));
        assert!(text.contains(&format!("`{} ide open <path-or-url>", dev.display())));
        assert!(text.contains(&format!("`{} skills ide`", dev.display())));
        assert!(text.contains("--quiet") && text.contains("--notify"));
        assert!(text.contains(&format!("`{} worktree new`", dev.display())));
        assert!(
            !text.contains("`veld "),
            "no bare word left for a dev instance: {text}"
        );
        let installed = agent_context("veld");
        assert!(installed.contains("`veld ide open ") && installed.contains("`veld skills ide`"));
        // Static per instance: the same input is the same bytes, every time.
        assert_eq!(installed, agent_context(&context_cli_word(dev, true)));
    }

    /// The encoders survive a hostile path: quotes, a backslash, a newline and DEL. Each
    /// output decodes back to the exact text, and carries no raw DEL (TOML forbids it in
    /// a basic string; `serde_json` alone would leave it in).
    #[test]
    fn the_context_encodings_survive_any_path() {
        let text = agent_context(&context_cli_word(
            Path::new("/tmp/it's \"odd\"\\dir\n\u{7f}/veld"),
            false,
        ));
        let codex = codex_context_config(&text);
        let literal = codex
            .strip_prefix("developer_instructions=")
            .expect("the key prefix the wrapper's guard checks");
        assert!(
            !literal.contains('\u{7f}') && !literal.contains('\n'),
            "{literal}"
        );
        assert_eq!(serde_json::from_str::<String>(literal).unwrap(), text);

        let pi = pi_context_extension_doc(&text);
        let line = pi
            .lines()
            .find_map(|l| l.strip_prefix("const CONTEXT = "))
            .and_then(|l| l.strip_suffix(';'))
            .expect("one CONTEXT line");
        assert_eq!(serde_json::from_str::<String>(line).unwrap(), text);
        // Appended to the prompt it was handed — never a bare replacement.
        assert!(
            pi.contains("systemPrompt: event.systemPrompt + \"\\n\\n\" + CONTEXT"),
            "{pi}"
        );
        assert_eq!(pi, pi_context_extension_doc(&text));
        assert_eq!(
            context_extension_path(Path::new("/tmp/shims")),
            PathBuf::from("/tmp/shims/context/pi-veld-context.ts"),
            "outside `agent/`, whose sweep would delete a file written once per daemon"
        );
    }
}
