//! `veld agent` — the coding agents that live in Veld's terminals: their docs,
//! and the plumbing their hooks and wrappers call.
//!
//! `state`, `settings` and `prompt` are hidden because nobody types them: the
//! generated wrappers and hooks do. Each also answers at its pre-#423 top-level
//! name (`veld agent state`, …), because a terminal that was open across the
//! update still runs the hooks written before it. See `crate::Command`.

use clap::{Args, Subcommand};

use crate::commands;

#[derive(Subcommand)]
pub enum AgentCmd {
    /// Print veld's agent documentation, carried by this binary.
    ///
    /// With no topic, the index. The consumer skills in `skills/` are pointers
    /// here rather than copies of this text: a skill file installed into
    /// somebody's project describes whichever veld they had that day, while
    /// this describes the one they are running.
    Skills {
        /// The topic to print. Omit for the index.
        #[arg(value_name = "TOPIC")]
        topic: Option<String>,

        /// Output as JSON — the index as `[{name, summary, bytes}]`, a topic
        /// as `{name, summary, body}`. `bytes` is the document's size, so a
        /// caller with a context budget can choose before it spends one.
        #[arg(long)]
        json: bool,
    },

    /// Restart the coding agent this runs in, from inside it, in the same
    /// conversation — what `veld ide pane restart` does to this agent's pane.
    ///
    /// For an agent whose instructions changed under it — a new outfit profile, an
    /// edited `CLAUDE.md`, a skill installed — and that needs a fresh start to read
    /// them. The Veld window restarts the pane a moment after this returns, with the
    /// pane's `resume` command so the conversation carries on, and tells the agent
    /// it was restarted. Only a pane from the project's `ide.panes` can be restarted.
    ///
    /// Exit status: 0 the restart is on its way, 1 failed, 3 not inside a Veld
    /// terminal, 4 no Veld window is showing this worktree, 5 not a pane Veld can
    /// restart, or one that cannot resume (pass `--fresh`).
    Restart(commands::ide::RestartArgs),

    /// Report a coding agent's state, from a lifecycle hook.
    ///
    /// Reads the payload on stdin for a tool that pipes it there (Claude); a tool that
    /// appends it as the final argument instead (Codex's `notify`, Pi's generated
    /// extension) passes it as `PAYLOAD` here. Called by the hooks `veld
    /// agent settings` installs, not by hand.
    #[command(name = "state", hide = true)]
    State(AgentStateArgs),

    /// Write this terminal's ephemeral coding-agent hook configuration and print it.
    ///
    /// Called by the generated `claude`/`codex`/`pi` wrapper, not by hand. **Nothing
    /// of yours is edited** — no `~/.claude/settings.json`, no `.claude/` in your
    /// project, no `~/.codex/config.toml`, no `~/.pi/agent/settings.json` or
    /// `~/.pi/agent/extensions/`. For Claude this writes a per-session settings file
    /// and prints its path; `--settings` merges, so your own configuration still
    /// applies. For Codex there is no file: this prints a literal `-c notify=[...]`
    /// value, which overrides that one key for one invocation. For Pi this writes a
    /// per-session extension module and prints its path, loaded once with `-e`.
    ///
    /// Turned off by `terminal.agentIntegration` (Settings → Activity), and never
    /// reached for Claude on `terminal.claudeIntegration = mod`, whose wrapper passes
    /// straight through.
    #[command(name = "settings", hide = true)]
    Settings(AgentSettingsArgs),

    /// Claim this pane's launch prompt and print it, once.
    ///
    /// Called by the generated `claude` wrapper, not by hand: the New worktree dialog's
    /// prompt waits in a one-shot file (`$VELD_LAUNCH_PROMPT`) so the agent can start
    /// with it already sent. Exits non-zero, printing nothing, when the prompt has
    /// already been claimed — by an earlier launch, or by the window typing it in.
    #[command(name = "prompt", hide = true)]
    Prompt(AgentPromptArgs),
}

#[derive(Args)]
pub struct AgentStateArgs {
    /// Which agent's payload this is: `claude`, `codex`, or `pi`.
    #[arg(long)]
    pub tool: Option<String>,

    /// Terminal session to attribute the state to. Defaults to
    /// `$VELD_PTY_SESSION`.
    #[arg(long)]
    pub session: Option<String>,

    /// Report "an agent just launched here and is idle", reading no payload.
    ///
    /// Sent by the generated wrapper before it execs the agent. It is what stops a
    /// pane running an agent looking like a pane running a long shell command.
    #[arg(long)]
    pub launched: bool,

    /// The event JSON, when the tool appends it as the final argv entry rather
    /// than piping it on stdin (Codex, Pi). Absent for Claude and for
    /// `--launched`.
    #[arg(value_name = "PAYLOAD")]
    pub payload: Option<String>,
}

#[derive(Args)]
pub struct AgentSettingsArgs {
    /// Which agent: `claude`, `codex`, or `pi`.
    #[arg(long)]
    pub tool: Option<String>,

    /// Terminal session this is for. Defaults to `$VELD_PTY_SESSION`.
    #[arg(long)]
    pub session: Option<String>,
}

#[derive(Args)]
pub struct AgentPromptArgs {
    /// The launch-prompt file to claim. Removed by the claim.
    #[arg(long, value_name = "PATH")]
    pub take: std::path::PathBuf,
}

/// `state` is answered before a runtime exists (see `main`); the rest run on it.
pub async fn run(command: AgentCmd) -> i32 {
    match command {
        AgentCmd::Skills { topic, json } => commands::skills::run(topic, json),
        AgentCmd::Restart(args) => commands::ide::restart(args).await,
        AgentCmd::State(args) => state(args),
        AgentCmd::Settings(args) => commands::agent::settings(args.tool, args.session),
        AgentCmd::Prompt(args) => commands::agent::take_prompt(&args.take),
    }
}

/// This part's half of `crate::command_survives_an_update`, verb by verb so a new
/// verb is blocked until somebody decides otherwise.
///
/// `restart` is blocked: it asks the daemon, and a window, that an update is
/// restarting. The other four survive. `skills` is a compile-time string with no config, database or
/// daemon behind it; blocking it would tell a coding agent to come back later for
/// its own instructions. `state`, `settings` and `prompt` touch neither the daemon
/// nor the binaries — one POSTs to localhost and ignores the answer, one writes a
/// small file in this instance's shim directory, one reads a file — and the gate's
/// message goes to stderr, which a coding agent reads: Veld's update banner inside
/// somebody's Claude Code transcript is worse than a badge that does not arrive.
pub fn survives_an_update(command: &AgentCmd) -> bool {
    match command {
        AgentCmd::Skills { .. }
        | AgentCmd::State(_)
        | AgentCmd::Settings(_)
        | AgentCmd::Prompt(_) => true,
        AgentCmd::Restart(_) => false,
    }
}

pub fn state(args: AgentStateArgs) -> i32 {
    commands::agent::state(args.tool, args.session, args.launched, args.payload)
}
