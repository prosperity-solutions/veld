mod commands;
mod engine_client;
mod hints;
mod output;

use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Subcommand)]
pub enum DesktopCommand {
    /// Download and install the Mac app matching this CLI version.
    Install,

    /// Update the installed app to match this CLI version.
    Update {
        /// Install this version instead of the CLI's own.
        ///
        /// The app passes the version it was offered. Without it, an app told
        /// "12.8.0 is available" by an older CLI would be reinstalled at the CLI's
        /// version, re-offered 12.8.0 on relaunch, and loop forever.
        #[arg(long)]
        version: Option<String>,

        /// Wait for this process to exit before replacing the bundle. Used by the
        /// app to update itself: it cannot be swapped while it is running.
        #[arg(long, hide = true)]
        wait_pid: Option<u32>,

        /// Reopen the app once it has been replaced.
        #[arg(long)]
        relaunch: bool,

        /// The running app's executable (`process.execPath`), so the bundle that
        /// gets replaced is the one the user launched.
        ///
        /// Without it the installer picks `/Applications`, and an app running from
        /// `~/Applications` or a second copy elsewhere gets a *new* install there
        /// while the one in the Dock stays stale.
        #[arg(long, hide = true)]
        app_path: Option<std::path::PathBuf>,
    },

    /// Remove the Mac app and stop veld installing it again.
    ///
    /// The opt-out half of an optional app: it deletes the bundle *and* records
    /// the answer, so `veld update` skips the app from then on. Works with no app
    /// installed too — that is how somebody who only uses veld as an orchestrator
    /// says "never fetch this" up front.
    Uninstall {
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Show where the app is installed and whether it matches this CLI.
    Status {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum SetupCommand {
    /// No-sudo setup: Caddy, daemon, helper on port 18443.
    Unprivileged,
    /// One-time sudo: system daemon, ports 80/443, clean URLs.
    Privileged {
        /// Path to veld-helper binary (resolved before sudo escalation).
        #[arg(long, hide = true)]
        helper_bin: Option<std::path::PathBuf>,

        /// Path to user socket (resolved before sudo escalation).
        #[arg(long, hide = true)]
        user_socket: Option<std::path::PathBuf>,

        /// Path to Caddy binary (resolved before sudo escalation).
        #[arg(long, hide = true)]
        caddy_bin: Option<std::path::PathBuf>,
    },
}

/// Veld -- local development environment orchestrator.
#[derive(Parser)]
#[command(
    name = "veld",
    version = env!("CARGO_PKG_VERSION"),
    about = "Local development environment orchestrator",
    after_help = "Management UI: https://veld.localhost (run `veld ui` to open)"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Enable debug logging.
    #[arg(long, global = true)]
    debug: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Start an environment.
    Start {
        /// Node selections in the form `node:variant`.
        #[arg(value_name = "NODE:VARIANT")]
        selections: Vec<String>,

        /// Use a preset instead of individual selections: its name, or the key
        /// shown by `veld presets`.
        #[arg(long)]
        preset: Option<String>,

        /// Give the run a custom name.
        #[arg(long)]
        name: Option<String>,

        /// From a terminal: follow the run's logs after it starts; Ctrl+C stops it.
        #[arg(long, short = 'a')]
        attach: bool,

        /// Run the selected command node as a one-off: bring up its
        /// dependencies, run it to completion streaming its output, then tear
        /// everything down and exit with the node's exit code. Ideal for
        /// end-to-end test runs. Requires exactly one command-type selection.
        #[arg(long)]
        oneshot: bool,

        /// With --oneshot, also stream the dependencies' logs (not just the
        /// terminal node's output).
        #[arg(long)]
        all_logs: bool,

        /// Answer a machine-overridable var for this run only, as `NAME=VALUE`.
        /// Repeatable. Never stored — use `veld config set` for that. Visible in
        /// the process table, so not the way to pass a secret.
        #[arg(long, value_name = "NAME=VALUE")]
        var: Vec<String>,

        /// Enable debug logging for the started environment.
        #[arg(long)]
        debug: bool,
    },

    /// Stop a running environment.
    Stop {
        /// Name of the run to stop.
        #[arg(long)]
        name: Option<String>,

        /// Stop all running environments.
        #[arg(long)]
        all: bool,
    },

    /// Restart a running environment.
    Restart {
        /// Name of the run to restart.
        #[arg(long)]
        name: Option<String>,

        /// Enable debug logging for the restarted environment.
        #[arg(long)]
        debug: bool,
    },

    /// Run history: list, inspect, or diff run instances.
    Runs {
        /// Filter by environment name.
        #[arg(long)]
        name: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        cmd: Option<RunsCmd>,
    },

    /// Show status of a running environment.
    Status {
        /// Name of the run to inspect.
        #[arg(long)]
        name: Option<String>,

        /// Show node outputs (environment variables, ports, etc.).
        #[arg(long)]
        outputs: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Show detailed CPU/memory stats, per subprocess and over time.
    Stats(commands::stats::StatsArgs),

    /// Work with the Veld IDE window from a terminal: `veld ide open`, and hand a
    /// task to the human as a new worktree with `veld ide worktree new`.
    Ide {
        #[command(subcommand)]
        command: commands::ide::IdeCommand,
    },

    /// Open a web page in the Veld window that owns this terminal.
    ///
    /// Falls back to the system browser whenever Veld is not the right place:
    /// outside a Veld terminal, for an origin on the exempt list
    /// (`browser.externalOrigins`, or `ide.externalOrigins` in the project's
    /// config), or when no window is attached to this terminal. Anything that is
    /// not a single http(s) URL is handed to the real system opener unchanged.
    ///
    /// A Veld terminal points `$BROWSER` at this command, so most CLIs reach it
    /// without being told to.
    #[command(name = "open-url")]
    OpenUrl {
        /// Which system tool this is standing in for, so a passthrough reaches the
        /// right one. Set by the generated shims; you do not need it.
        #[arg(long, hide = true)]
        tool: Option<String>,

        /// Terminal session to open the page beside. Defaults to
        /// `$VELD_PTY_SESSION`, which a Veld terminal sets.
        #[arg(long)]
        session: Option<String>,

        /// The URL — or, for a shim, whatever the real tool was called with.
        #[arg(
            value_name = "ARGS",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        args: Vec<String>,
    },

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
    #[command(name = "agent-settings", hide = true)]
    AgentSettings {
        /// Which agent: `claude`, `codex`, or `pi`.
        #[arg(long)]
        tool: Option<String>,

        /// Terminal session this is for. Defaults to `$VELD_PTY_SESSION`.
        #[arg(long)]
        session: Option<String>,
    },

    /// Report a coding agent's state, from a lifecycle hook.
    ///
    /// Reads the payload on stdin for a tool that pipes it there (Claude); a tool that
    /// appends it as the final argument instead (Codex's `notify`, Pi's generated
    /// extension) passes it as `PAYLOAD` here. Called by the hooks `veld
    /// agent-settings` installs, not by hand.
    #[command(name = "agent-state", hide = true)]
    AgentState {
        /// Which agent's payload this is: `claude`, `codex`, or `pi`.
        #[arg(long)]
        tool: Option<String>,

        /// Terminal session to attribute the state to. Defaults to
        /// `$VELD_PTY_SESSION`.
        #[arg(long)]
        session: Option<String>,

        /// Report "an agent just launched here and is idle", reading no payload.
        ///
        /// Sent by the generated wrapper before it execs the agent. It is what stops a
        /// pane running an agent looking like a pane running a long shell command.
        #[arg(long)]
        launched: bool,

        /// The event JSON, when the tool appends it as the final argv entry rather
        /// than piping it on stdin (Codex, Pi). Absent for Claude and for
        /// `--launched`.
        #[arg(value_name = "PAYLOAD")]
        payload: Option<String>,
    },

    /// Claim this pane's launch prompt and print it, once.
    ///
    /// Called by the generated `claude` wrapper, not by hand: the New worktree dialog's
    /// prompt waits in a one-shot file (`$VELD_LAUNCH_PROMPT`) so the agent can start
    /// with it already sent. Exits non-zero, printing nothing, when the prompt has
    /// already been claimed — by an earlier launch, or by the window typing it in.
    #[command(name = "agent-prompt", hide = true)]
    AgentPrompt {
        /// The launch-prompt file to claim. Removed by the claim.
        #[arg(long, value_name = "PATH")]
        take: std::path::PathBuf,
    },

    /// Show URLs of a running environment.
    Urls {
        /// Name of the run to inspect.
        #[arg(long)]
        name: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Run a node-defined action against a running environment.
    Action {
        /// Name of the action to run (see `veld actions`).
        #[arg(value_name = "ACTION")]
        action: String,

        /// Name of the run to target.
        #[arg(long)]
        name: Option<String>,

        /// Node to run the action against. Defaults to the matching node when
        /// only one qualifies.
        #[arg(long)]
        node: Option<String>,

        /// Print the resolved command instead of running it.
        #[arg(long)]
        print: bool,

        /// Output the resolved command as JSON (does not run it).
        #[arg(long)]
        json: bool,
    },

    /// List the actions defined across the project's nodes.
    Actions {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// View logs for a running environment.
    Logs {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Filter by node name.
        #[arg(long)]
        node: Option<String>,

        /// Number of lines to show per node/stream.
        #[arg(long, default_value = "50")]
        lines: usize,

        /// Only show logs since this duration (e.g. "5m", "1h").
        #[arg(long)]
        since: Option<String>,

        /// Stream logs continuously (like `tail -f`). Exits once the
        /// followed run ends.
        #[arg(long, short = 'f')]
        follow: bool,

        /// Show logs of a specific past run by id prefix (see `veld runs`).
        /// The run identifies its environment by itself, so this conflicts
        /// with --name.
        #[arg(long, value_name = "RUN_ID", conflicts_with_all = ["previous", "all_runs", "name"])]
        run: Option<String>,

        /// Show logs of the run before the latest one (after a restart:
        /// the previous generation).
        #[arg(long, short = 'p', conflicts_with = "all_runs")]
        previous: bool,

        /// Show logs of every run under this name interleaved (pre-v3
        /// behavior), including lines that predate run scoping.
        #[arg(long)]
        all_runs: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        /// Filter by log source: all, server (node output), client, setup
        /// (project setup/teardown steps), or internal (veld daemon
        /// liveness/recovery logs).
        #[arg(long, default_value = "all")]
        source: String,

        /// Filter log lines by search term (case-insensitive substring match).
        #[arg(long, short = 's')]
        search: Option<String>,

        /// Number of context lines to show around search matches.
        #[arg(long, short = 'C', default_value = "0")]
        context: usize,

        /// Print timestamps in UTC, exactly as stored, instead of your local
        /// time zone. Overrides the `logs.timeZone` setting for this run.
        ///
        /// Rejected alongside --json rather than silently ignored (the same
        /// posture `veld presets --pin` takes): `--json` always emits UTC, so
        /// accepting these there would answer a request for localised JSON
        /// with UTC and no word about it.
        #[arg(long, conflicts_with_all = ["local", "json"])]
        utc: bool,

        /// Print timestamps in your local time zone (the default), overriding
        /// the `logs.timeZone` setting for this run. Rejected alongside
        /// --json, which is always UTC.
        #[arg(long, conflicts_with = "json")]
        local: bool,
    },

    /// Print the dependency graph for the given selections.
    Graph {
        /// Node selections in the form `node:variant`.
        #[arg(value_name = "NODE:VARIANT")]
        selections: Vec<String>,
    },

    /// List all available nodes and their variants.
    Nodes {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// List all available presets.
    Presets {
        /// Output as JSON.
        #[arg(long)]
        json: bool,

        /// Print the current numbering as a paste-ready `presets` block, so
        /// auto-assigned keys can be pinned and stop moving.
        ///
        /// Rejected alongside `--json` rather than silently ignored: the output is
        /// JSONC for a human to paste, not a document for a program to parse.
        #[arg(long, conflicts_with = "json")]
        pin: bool,
    },

    /// Read and change Veld's own settings.
    ///
    /// These are machine-wide preferences — which shell terminals open, where new
    /// worktrees land, whether Veld may keep this machine awake — and they are
    /// the same ones the settings dialog in the IDE edits.
    ///
    /// Not to be confused with `veld config`, which is about a *project's*
    /// veld.json and the machine-overridable vars it declares.
    Settings {
        /// Show only settings under this key prefix, e.g. `keepAwake` or
        /// `terminal`. Matches whole segments, so `ui` finds every `ui.*`.
        prefix: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        cmd: Option<SettingsCmd>,
    },

    /// Print the project's veld.json configuration.
    Config {
        /// Print only the path to veld.json instead of its contents.
        #[arg(long)]
        path: bool,

        /// List each include glob, the files it matched, and the nodes each
        /// defines. The fastest way to find out why a node seems missing.
        #[arg(long)]
        files: bool,

        /// Explain one effective value and where it was defined, e.g.
        /// `nodes.api.variants.dev.env.DATABASE_URL`. A value declared `secret`
        /// is described, never printed.
        #[arg(long, value_name = "POINTER")]
        why: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        #[command(subcommand)]
        cmd: Option<ConfigCmd>,
    },

    /// Check veld.json for semantic problems (CI-friendly).
    Lint {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Initialise a new veld.json in the current directory.
    Init,

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

    /// List all Veld projects on this machine.
    List {
        /// Include URLs in the output.
        #[arg(long)]
        urls: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Bidirectional feedback threads with the in-browser overlay.
    Feedback {
        #[command(subcommand)]
        command: commands::feedback::FeedbackCommand,
    },

    /// Garbage-collect stale state and logs.
    Gc,

    /// Run first-time setup or manage setup configuration.
    Setup {
        #[command(subcommand)]
        command: Option<SetupCommand>,
    },

    /// Update Veld to the latest version.
    ///
    /// Moves both halves of the release: the CLI, daemon and helper, and — on
    /// macOS — Veld Desktop. When the app is running it is closed first, with the
    /// user's agreement, and reopened afterwards; its bundle cannot be replaced
    /// while it runs.
    Update {
        /// Wait for this process to exit before installing anything.
        ///
        /// Veld Desktop's own "Update" spawns this and quits: an Electron app
        /// reads from its own bundle while it runs, so the update has to outlive
        /// it. Nothing is installed until the pid is gone.
        #[arg(long, hide = true)]
        wait_pid: Option<u32>,

        /// Reopen Veld Desktop when the update is over, however it went.
        ///
        /// Hidden, and only meaningful with `--wait-pid`: on its own it would
        /// *launch* an app the user never had running, which is not what the word
        /// says and not something an update should do.
        #[arg(long, hide = true)]
        relaunch: bool,

        /// The running app's executable (`process.execPath`), so the bundle that
        /// gets replaced is the one the user launched rather than whichever copy
        /// the installer would have guessed.
        #[arg(long, hide = true)]
        app_path: Option<std::path::PathBuf>,

        /// Install this release instead of asking GitHub which is latest.
        ///
        /// The app passes the version it was offered, and that is not a
        /// convenience: the app learns about releases through electron-updater's
        /// feed while `check_update` asks `api.github.com/…/releases/latest`.
        /// Two sources, two failure modes. Unauthenticated api.github.com is rate
        /// limited per IP, so behind a shared NAT the handoff would abort with a
        /// 403 on a machine that had just been told an update exists; and in the
        /// minutes of skew after a release the API can still answer with the
        /// version the CLI already has, which would install nothing, report
        /// success, and re-offer the same update on the next launch — forever,
        /// since the app's "already offered" set is per session.
        #[arg(long, hide = true)]
        target_version: Option<String>,

        /// Re-run this update in a terminal window instead of here.
        ///
        /// Veld Desktop's handoff sets it. The app quits so its bundle can be
        /// replaced, which leaves the update with nothing to render on and — more
        /// seriously — no controlling terminal, so a privileged install's `sudo`
        /// can only ever try `sudo -n` and fail silently. A window fixes both at
        /// once. Falls back to running right here when no terminal can be opened.
        #[arg(long, hide = true)]
        console: bool,

        /// Report whether an update is running, and what it is doing.
        ///
        /// Reads the same lock file every other gate reads, so it is the honest
        /// answer rather than a second opinion. Installs nothing.
        #[arg(long)]
        status: bool,

        /// Machine-readable `--status`.
        #[arg(long, requires = "status")]
        json: bool,

        /// Start even though another update holds the lock.
        ///
        /// The escape hatch for a holder this one knows is dead but that the
        /// staleness rules have not written off yet — a run abandoned at a
        /// password prompt is only reclaimed after 30 minutes without this.
        #[arg(long)]
        force: bool,

        /// Show the install script's own output instead of veld's summary.
        ///
        /// The update normally narrates its own steps and runs the installer
        /// quietly, because the two of them talking at once produced three
        /// "installed successfully!" banners and a first-install footer in the
        /// middle of an update. This is the escape hatch for debugging one: the
        /// raw stream, every URL and checksum, exactly as the script emits it.
        #[arg(long)]
        verbose: bool,
    },

    /// Install, update or inspect Veld Desktop (macOS app).
    Desktop {
        #[command(subcommand)]
        command: Option<DesktopCommand>,
    },

    /// Uninstall Veld and clean up.
    Uninstall,

    /// Open the management dashboard in the browser.
    Ui,

    /// Diagnose installation and service health.
    Doctor {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Copies of Veld's own database: list them, take one, put one back.
    ///
    /// Everything Veld knows — repositories, worktrees, lanes, layouts, run
    /// history, settings — lives in one SQLite file, and the daemon keeps compact
    /// copies of it on the interval in `veld settings backup`.
    Backup {
        /// Where to look, overriding the `backup.dir` setting for this command.
        // `global` so it reads naturally after the verb — `veld backup now --dir
        // /Volumes/disk` — rather than only in front of it, which is where a
        // parent-only flag has to go and is not where anybody types it. A `//`
        // comment, not a doc comment: clap renders those as help text, and this is
        // a note to the next maintainer rather than to a user.
        #[arg(long, value_name = "DIR", global = true)]
        dir: Option<std::path::PathBuf>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<BackupCmd>,
    },

    /// Share a running environment with a colleague over peer-to-peer.
    Share {
        /// Run to share (defaults to the only active run).
        #[arg(value_name = "RUN")]
        run: Option<String>,
        /// Limit to specific nodes (default: all URL-bearing nodes).
        #[arg(long, value_name = "NODE")]
        node: Vec<String>,
        /// Share lifetime in seconds, overriding the default for this one share.
        /// No upper bound; anything under 60 is raised to 60, since a share that
        /// expires before its link can be opened is not a shorter share. The
        /// default comes from `sharing.peerTtlMinutes` / `sharing.webTtlMinutes`
        /// (`veld settings`, 240/120 minutes out of the box), or the project's
        /// `sharing.peer_ttl_minutes` / `web_ttl_minutes` in veld.json.
        #[arg(long)]
        ttl: Option<i64>,
        /// Approval mode: first | manual | auto (default: manual, or first
        /// with --json; auto for --web, where the gateway is the only joiner).
        #[arg(long, value_name = "MODE")]
        approve: Option<String>,
        /// Share to the public web via the configured gateway
        /// (`sharing.gateway`): only nodes with `web` in `share.expose`;
        /// prints real public URLs anyone can open in a browser.
        #[arg(long)]
        web: bool,
        /// Viewer access for --web nodes whose config is silent on
        /// `share.web.access`: password (default) | link. An explicit config
        /// value always wins over this flag.
        #[arg(long, value_name = "MODE", requires = "web")]
        access: Option<String>,
        /// Use this share password for --web instead of a generated one
        /// (min 8 chars). Note: CLI args land in shell history and process
        /// listings — prefer the generated default for anything sensitive.
        #[arg(long, value_name = "PASSWORD", requires = "web")]
        password: Option<String>,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Join a shared environment by ticket.
    Join {
        /// The `veldshare_…` ticket from the host.
        ticket: String,
        /// A label the host sees on approval (e.g. your name).
        #[arg(long)]
        label: Option<String>,
        /// Don't cache a relay auth token entered at the prompt (by default a
        /// working token is remembered per relay so future joins don't re-ask).
        #[arg(long)]
        no_remember: bool,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// List active shares and joins.
    Shares {
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Stop hosting a share.
    Unshare {
        /// Share id (from `veld shares`); optional when exactly one is active.
        id: Option<String>,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Leave a joined share.
    Leave {
        /// Join id (from `veld shares`); optional when exactly one is active.
        id: Option<String>,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Approve a pending join request (see `veld shares`).
    Approve {
        /// Request id (from `veld shares` or the approval prompt).
        id: String,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Deny a pending join request.
    Deny {
        /// Request id (from `veld shares`).
        id: String,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Print version information for all Veld binaries.
    Version,

    /// Internal: hand a downloaded veld-helper to the running root helper, which
    /// verifies its signature and version and installs it into its root-owned
    /// directory (#262). Called by `install.sh`; a no-op on any install whose
    /// helper is not served from that directory.
    #[command(name = "_helper-install", hide = true)]
    InternalHelperInstall {
        /// The downloaded `veld-helper`, with its `.sig` beside it.
        #[arg(long)]
        binary: std::path::PathBuf,
    },

    /// Internal (legacy): read stdin, prepend timestamps, append to a log
    /// file. Kept so detached pipelines started by a pre-SQLite veld keep
    /// working after an upgrade — if this subcommand disappeared, the running
    /// server would die of SIGPIPE on its next write. Note: those legacy
    /// pipelines keep writing FILES, which the DB-backed `veld logs` does not
    /// read — a pre-upgrade run stays visible and stoppable but its ongoing
    /// output is only in `.veld/logs/` until the run is restarted.
    #[command(name = "_timestamp", hide = true)]
    InternalTimestamp {
        /// Path to the log file to append to.
        #[arg(long)]
        log: std::path::PathBuf,
    },

    /// Internal: this clap tree as JSON — every node's flags (hidden ones
    /// included), whether it takes a positional, and whether it requires a
    /// subcommand. Consumed by `tests/validate-doc-commands.py`, which used to
    /// infer the same thing by parsing `--help` prose and got it wrong three
    /// ways (see `commands/cli_dump.rs`).
    #[command(name = "_cli-dump", hide = true)]
    InternalCliDump,
}

#[derive(Subcommand)]
enum RunsCmd {
    /// Show one run in full: outcome, node results, and the graph snapshot
    /// it was started with.
    Show {
        /// Run id prefix (see `veld runs`).
        run_id: String,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Diff two runs' graph snapshots — what changed in the config between
    /// them. With one id, diffs that run against its predecessor.
    Diff {
        /// Older run id prefix (or, with one argument, the run to compare
        /// against its predecessor).
        a: String,

        /// Newer run id prefix.
        b: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// This machine's answers for vars the config declared `machine`-overridable.
///
/// The declaration is committed and shared; the answer is not. Both scopes write
/// to veld's own database, never to veld.json — veld does not rewrite a user's
/// config, and the whole point is that the answer differs per machine.
#[derive(Subcommand)]
enum SettingsCmd {
    /// Print one setting's effective value and nothing else.
    ///
    /// Bare, so it drops into a shell substitution:
    /// `SHELL=$(veld settings get terminal.shell)`.
    Get {
        /// The setting's name, e.g. terminal.shell.
        key: String,

        /// Output as JSON, with where the value came from.
        #[arg(long)]
        json: bool,
    },
    /// Change one setting.
    ///
    /// The value is written bare — `true`, `60`, `/bin/bash` — not as JSON. A
    /// number outside its range is **clamped rather than refused**, and the
    /// clamp is reported; run `veld settings describe` first to see the range.
    Set {
        /// The setting's name.
        key: String,

        /// The new value. For a list setting, either a JSON array or a
        /// comma-separated list.
        ///
        /// `allow_hyphen_values` because real values start with one: `-1` for a
        /// number, and `-apple-system` heads most CSS font stacks. Without it
        /// clap refuses them as unknown flags before the catalog is consulted,
        /// which contradicts the documented promise that an out-of-range number
        /// is *clamped rather than refused* — and reports it as an argument error
        /// about a flag that is not a flag.
        #[arg(allow_hyphen_values = true)]
        value: String,

        /// Output as JSON, including whether the value was clamped.
        #[arg(long)]
        json: bool,
    },
    /// Put one setting back on its default.
    Unset {
        /// The setting's name.
        key: String,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Explain one setting: what it does, its type, its default, and what it
    /// accepts.
    ///
    /// "What it accepts" rather than "every value it accepts": for a closed set
    /// that is the list, but `terminal.shell` and the other runtime-offered keys
    /// have no enumerable one, and a number's answer is a range plus the fact
    /// that going outside it clamps rather than fails.
    Describe {
        /// The setting's name.
        key: String,

        /// Output as JSON — the setting's whole catalog entry.
        #[arg(long)]
        json: bool,
    },
}

/// `veld backup` and its verbs.
///
/// Every one of these has to work on a machine whose database will not open —
/// that is the only day anybody runs them — so none of them requires a readable
/// database, and the backup directory falls back to the derived default when the
/// `backup.dir` setting cannot be read.
#[derive(Subcommand)]
enum BackupCmd {
    /// Take a backup right now, outside the daemon's schedule.
    ///
    /// Retention still applies, so this cannot fill a disk that the scheduled
    /// backup keeps bounded.
    Now {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Put a backup back in place.
    ///
    /// With no path, the newest backup that is actually **usable** is chosen —
    /// which is not always the newest file, since a backup taken after the damage
    /// started is exactly the one to skip. The database being replaced is renamed,
    /// never deleted: it is the only evidence of what went wrong.
    Restore {
        /// A specific backup file. Defaults to the newest usable one.
        #[arg(value_name = "PATH")]
        path: Option<std::path::PathBuf>,

        /// Restore even though the daemon is running. Its work after this point
        /// goes to the database being replaced and is lost, so restart it
        /// afterwards.
        #[arg(long)]
        force: bool,

        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// List every machine-overridable var: its effective value, where that value
    /// came from, and what the config says about it.
    Vars {
        /// Output as JSON. Secret values are redacted in both forms.
        #[arg(long)]
        json: bool,
    },
    /// Set this machine's answer for one var.
    ///
    /// With no source flag the value is stored literally. The source flags store
    /// a *pointer* instead, which is how a `secret` var is answered without the
    /// secret itself landing in the database.
    Set {
        /// The var's name, as declared in `vars`.
        name: String,

        /// The literal value. Omit when using a source flag.
        value: Option<String>,

        /// Read the value from this environment variable at run start.
        #[arg(long, value_name = "NAME", conflicts_with_all = ["file", "shell"])]
        env: Option<String>,

        /// Read the value from this file at run start (relative to the project
        /// root).
        #[arg(long, value_name = "PATH", conflicts_with_all = ["env", "shell"])]
        file: Option<String>,

        /// Run this command at run start and use its stdout.
        #[arg(long, value_name = "COMMAND", conflicts_with_all = ["env", "file"])]
        shell: Option<String>,

        /// Apply to this checkout only, instead of every worktree of the
        /// project. Use for a value that is about this branch's work rather than
        /// about the machine.
        #[arg(long)]
        worktree: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Forget this machine's answer, falling back to the next scope down and
    /// then to the config's `default`.
    Unset {
        /// The var's name.
        name: String,

        /// Forget the checkout-scoped answer instead of the project-scoped one.
        #[arg(long)]
        worktree: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

fn init_tracing(debug: bool) {
    use tracing_subscriber::EnvFilter;

    let filter = if debug {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"))
    };

    // Diagnostic logs go to stderr (the conventional target), keeping stdout
    // clean for machine-readable output — notably the terminal node's own
    // output under `veld start --oneshot`.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
}

/// A plain `fn main`, not `#[tokio::main]`: the runtime is built only once a command
/// that needs one has been picked.
///
/// `veld agent-state` is the reason. Claude Code runs it from a blocking hook at least
/// twice per turn, and all it does is one POST to `127.0.0.1` (two, when an older
/// daemon refuses the naming turn) — so it is dispatched here, to a synchronous path,
/// before a multi-thread runtime (one worker per core) or a TLS stack exists. It was never behind the update gate or the version check
/// (see [`command_survives_an_update`] and `needs_version_check` in [`run`]), so
/// answering it before them changes nothing it used to do.
fn main() {
    let cli = Cli::parse();

    init_tracing(cli.debug);

    // Handle bare `veld` with no subcommand -- print help.
    let command = match cli.command {
        None => {
            let _ = Cli::command().print_help();
            println!();
            return;
        }
        Some(Command::AgentState {
            tool,
            session,
            launched,
            payload,
        }) => std::process::exit(commands::agent::state(tool, session, launched, payload)),
        Some(command) => command,
    };

    // Exactly what `#[tokio::main]` expanded to: a multi-thread runtime, every driver on.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the tokio runtime");
    runtime.block_on(run(command));
}

async fn run(command: Command) {
    // An update in flight is replacing veld, veld-daemon, veld-helper and caddy
    // and restarting both services. Nearly every command either execs one of
    // those binaries or expects a daemon that is currently down, so it gets a
    // straight answer here instead of an unexplained connection error thirty
    // seconds into a `veld start`. See `command_survives_an_update` for the
    // allow-list and why each entry is on it.
    if !command_survives_an_update(&command) {
        if let Some(state) = veld_core::update_lock::current() {
            output::print_error(&state.describe(chrono::Utc::now()), false);
            eprintln!(
                "  {}",
                output::dim(
                    "Commands that touch the daemon, the helper or the installed binaries are \
                     unavailable until it finishes. Watch it with `veld update --status`."
                )
            );
            // EX_TEMPFAIL. "Try again shortly" is exactly what this is, and a
            // coding agent driving veld needs to be able to tell it apart from
            // the failures that mean something is wrong.
            std::process::exit(75);
        }
    }

    // Check for version mismatches on commands that talk to the daemon/helper.
    let needs_version_check = matches!(
        command,
        Command::Start { .. }
            | Command::Stop { .. }
            | Command::Restart { .. }
            | Command::Status { .. }
            | Command::Stats(_)
            | Command::Urls { .. }
            | Command::Action { .. }
            | Command::Logs { .. }
    );

    // The version gate compares this CLI against the RUNNING helper in
    // privileged mode (the installed file when it does not answer, and always in
    // unprivileged and auto mode) and the installed daemon. A dev instance
    // (VELD_DAEMON_PORT set) shares those services deliberately, so a version gap with them is expected — enforce
    // alignment only for the installed instance. VELD_LIB_DIR is the older
    // escape hatch (points version discovery at a dev build dir) and still
    // skips too.
    let is_dev_instance =
        veld_core::instance::daemon_port() != veld_core::instance::DEFAULT_DAEMON_PORT;
    if needs_version_check && std::env::var("VELD_LIB_DIR").is_err() && !is_dev_instance {
        if let Err(msg) = commands::version::check_version_mismatch().await {
            output::print_error(&msg, false);
            std::process::exit(1);
        }
    }

    // Housekeeping on the commands a person or an agent runs most: one database
    // open shared by both steps (they used to open it separately, and the command
    // itself opens it a third time).
    //
    // - Auto-GC: trigger background GC if it hasn't run in >30 minutes.
    // - Update check: show a banner if a newer version is available (once per day).
    if needs_version_check {
        if let Ok(db) = veld_core::db::Db::open() {
            maybe_auto_gc(&db);
            if update_banner_wanted(&command) {
                maybe_show_update_banner(&db).await;
            }
        }
    }

    let exit_code = match command {
        Command::Start {
            selections,
            preset,
            name,
            attach,
            oneshot,
            all_logs,
            var,
            debug,
        } => {
            commands::start::run(
                selections, preset, name, attach, oneshot, all_logs, var, debug,
            )
            .await
        }

        Command::Stop { name, all } => commands::stop::run(name, all).await,

        Command::Restart { name, debug } => commands::restart::run(name, debug).await,

        Command::Runs { name, json, cmd } => match cmd {
            None => commands::runs::list(name.as_deref(), json).await,
            // OR the outer flag in: `veld runs --json show <id>` must not
            // silently fall back to human output (exit 0, unparseable) for
            // an agent that treats --json as global.
            Some(RunsCmd::Show { run_id, json: sub }) => {
                commands::runs::show(&run_id, json || sub).await
            }
            Some(RunsCmd::Diff { a, b, json: sub }) => {
                commands::runs::diff(&a, b.as_deref(), json || sub).await
            }
        },

        Command::Status {
            name,
            outputs,
            json,
        } => commands::status::run(name, outputs, json).await,

        Command::Stats(args) => commands::stats::run(args).await,

        Command::Urls { name, json } => commands::urls::run(name, json).await,

        Command::OpenUrl {
            tool,
            session,
            args,
        } => commands::open_url::run(tool, session, args).await,

        Command::Ide { command } => commands::ide::run(command).await,

        Command::AgentSettings { tool, session } => commands::agent::settings(tool, session),

        // Normally answered in `main`, before any runtime exists; kept so the match
        // stays exhaustive and a future caller of `run` gets the same behaviour.
        Command::AgentState {
            tool,
            session,
            launched,
            payload,
        } => commands::agent::state(tool, session, launched, payload),

        Command::AgentPrompt { take } => commands::agent::take_prompt(&take),

        Command::Action {
            action,
            name,
            node,
            print,
            json,
        } => commands::action::run(action, name, node, print, json).await,

        Command::Actions { json } => commands::action::list(json).await,

        Command::Logs {
            name,
            node,
            lines,
            since,
            follow,
            json,
            source,
            search,
            context,
            run,
            previous,
            all_runs,
            utc,
            local,
        } => {
            let source_filter =
                commands::logs::SourceFilter::from_str(&source).unwrap_or_else(|| {
                    output::print_error(
                        &format!(
                            "Invalid --source value '{source}'. Use: all, server, client, setup, internal"
                        ),
                        json,
                    );
                    std::process::exit(1);
                });
            commands::logs::run(commands::logs::LogsOptions {
                name,
                node,
                lines,
                since,
                follow,
                json,
                source: source_filter,
                search,
                context_lines: context,
                run,
                previous,
                all_runs,
                // `None` means "whatever `logs.timeZone` says", resolved once the
                // database is open. The flags are clap-exclusive, so at most one arm
                // can be taken.
                time_zone: if utc {
                    Some(veld_core::db::LogTimeZone::Utc)
                } else if local {
                    Some(veld_core::db::LogTimeZone::Local)
                } else {
                    None
                },
            })
            .await
        }

        Command::Graph { selections } => commands::graph::run(selections).await,

        Command::Nodes { json } => commands::nodes::run(json).await,

        Command::Presets { json, pin } => commands::presets::run(json, pin).await,

        // The outer `--json` is OR-ed into the inner one throughout, so both
        // `veld settings --json get x` and `veld settings get x --json` work —
        // the same shape `veld config` and `veld runs` already have.
        Command::Settings { prefix, json, cmd } => match cmd {
            Some(SettingsCmd::Get { key, json: inner }) => {
                commands::settings::get(key, json || inner).await
            }
            Some(SettingsCmd::Set {
                key,
                value,
                json: inner,
            }) => commands::settings::set(key, value, json || inner).await,
            Some(SettingsCmd::Unset { key, json: inner }) => {
                commands::settings::unset(key, json || inner).await
            }
            Some(SettingsCmd::Describe { key, json: inner }) => {
                commands::settings::describe(key, json || inner).await
            }
            None => commands::settings::list(prefix, json).await,
        },

        Command::Config {
            path,
            files,
            why,
            json,
            cmd,
        } => match cmd {
            // The outer `--json` is OR-ed in so both `veld config --json vars`
            // and `veld config vars --json` work, matching `veld runs`.
            Some(ConfigCmd::Vars { json: inner }) => {
                commands::config::list_vars(json || inner).await
            }
            Some(ConfigCmd::Set {
                name,
                value,
                env,
                file,
                shell,
                worktree,
                json: inner,
            }) => {
                commands::config::set_var(
                    &name,
                    value.as_deref(),
                    env.as_deref(),
                    file.as_deref(),
                    shell.as_deref(),
                    worktree,
                    json || inner,
                )
                .await
            }
            Some(ConfigCmd::Unset {
                name,
                worktree,
                json: inner,
            }) => commands::config::unset_var(&name, worktree, json || inner).await,
            None => commands::config::run(path, files, why, json).await,
        },

        Command::Lint { json } => commands::lint::run(json).await,

        Command::Init => commands::init::run().await,

        Command::Skills { topic, json } => commands::skills::run(topic, json),

        Command::InternalCliDump => commands::cli_dump::run(),

        Command::List { urls, json } => commands::list::run(urls, json).await,

        Command::Feedback { command } => commands::feedback::run(command).await,

        Command::Gc => commands::gc::run().await,

        Command::Setup { command } => commands::setup::run(command).await,

        Command::Update {
            wait_pid,
            relaunch,
            app_path,
            target_version,
            console,
            status,
            json,
            force,
            verbose,
        } => {
            if status {
                commands::update::status(json)
            } else {
                commands::update::run(
                    wait_pid,
                    relaunch,
                    app_path,
                    target_version,
                    console,
                    force,
                    verbose,
                )
                .await
            }
        }

        Command::Desktop { command } => match command {
            // Bare `veld desktop` reports rather than installs: a command that
            // puts an app in /Applications should be asked for by name.
            None | Some(DesktopCommand::Status { json: false }) => {
                commands::desktop::status(false).await
            }
            Some(DesktopCommand::Status { json: true }) => commands::desktop::status(true).await,
            Some(DesktopCommand::Install) => {
                commands::desktop::install(None, None, false, None).await
            }
            Some(DesktopCommand::Uninstall { yes }) => commands::desktop::uninstall(yes).await,
            Some(DesktopCommand::Update {
                version,
                wait_pid,
                relaunch,
                app_path,
            }) => commands::desktop::install(version, wait_pid, relaunch, app_path).await,
        },

        Command::Uninstall => commands::uninstall::run().await,
        Command::InternalHelperInstall { binary } => commands::helper_install::run(binary).await,

        Command::Ui => commands::ui::run().await,

        Command::Doctor { json } => commands::doctor::run(json).await,

        // Same outer/inner `--json` OR-ing as `settings`, `config` and `runs`, so
        // both `veld backup --json now` and `veld backup now --json` work.
        Command::Backup { dir, json, cmd } => match cmd {
            Some(BackupCmd::Now { json: inner }) => commands::backup::now(dir, json || inner),
            Some(BackupCmd::Restore {
                path,
                force,
                yes,
                json: inner,
            }) => commands::backup::restore(path, dir, force, yes, json || inner).await,
            None => commands::backup::list(dir, json),
        },

        Command::Share {
            run,
            node,
            ttl,
            approve,
            web,
            access,
            password,
            json,
        } => commands::share::share(run, node, ttl, approve, web, access, password, json).await,

        Command::Join {
            ticket,
            label,
            no_remember,
            json,
        } => commands::share::join(ticket, label, !no_remember, json).await,

        Command::Shares { json } => commands::share::list(json).await,

        Command::Unshare { id, json } => commands::share::unshare(id, json).await,

        Command::Leave { id, json } => commands::share::leave(id, json).await,

        Command::Approve { id, json } => commands::share::approve(id, json).await,

        Command::Deny { id, json } => commands::share::deny(id, json).await,

        Command::Version => {
            commands::version::print_version().await;
            0
        }

        Command::InternalTimestamp { log } => {
            // Fast path: no config loading, no network, just stdin → timestamped log file.
            // Used internally by detached server mode.
            use std::io::{BufRead, Write};
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            let mut buf = String::new();

            // Keep file handle open for performance; write per line so any
            // file tailer sees data immediately. (The DB-backed `veld logs`
            // does not read this file — see the subcommand doc above.)
            let mut file = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
            {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("veld _timestamp: failed to open log file: {e}");
                    std::process::exit(1);
                }
            };

            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let ts =
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
                        let formatted = format!("[{ts}] {trimmed}\n");
                        if let Err(e) = file.write_all(formatted.as_bytes()) {
                            eprintln!("veld _timestamp: write error: {e}");
                            break;
                        }
                    }
                    Err(_) => {
                        // Invalid UTF-8 line — skip it rather than terminating.
                        // This handles binary output from misbehaving processes.
                        continue;
                    }
                }
            }
            0
        }
    };

    std::process::exit(exit_code);
}

// ---------------------------------------------------------------------------
// Update gate
// ---------------------------------------------------------------------------

/// Which commands stay available while an update is running.
///
/// An **allow-list**, not a block-list, and that direction is the decision. The
/// unsafe set is "anything that execs a binary being replaced, or talks to a
/// service being restarted", which is most of this enum and grows every time a
/// subcommand is added — so a block-list would silently stop covering new
/// commands, and the symptom would be a corrupted update rather than a compile
/// error. Adding a command therefore means it is blocked during an update until
/// someone deliberately puts it here.
///
/// Each entry earns its place:
///
/// - `Update` arbitrates itself through the lock; `--status` and `--force` are
///   the two things a user reaches for *because* an update is running.
/// - `Doctor` is where a stuck user is sent, and it now reports the update.
/// - `Version` must answer during an update — it is how a human or a script
///   checks whether the swap has happened yet.
/// - `Config`, `Lint` and `Init` only read and write files in the project. They
///   are what a coding agent is likely doing in a worktree while an unrelated
///   update runs, and blocking them buys nothing.
/// - `InternalTimestamp` is **not** a convenience: it is spawned by the node
///   processes of environments that are still running, and an update
///   deliberately leaves those serving. Blocking it would break live
///   environments to protect an update that never touches them. (The log sink,
///   `veld-daemon --log-sink`, is not a CLI command and is not gated at all.)
/// - `Desktop { Status }` — read-only (it reads a plist), and blocking it
///   produces a *wrong* answer rather than a blocked one: Veld Desktop resolves
///   the CLI by running `veld desktop status --json`, and its caller catches any
///   failure as "there is no veld CLI here", which silently demotes
///   *Check for Updates…* to the download-from-GitHub route. The install and
///   update arms stay blocked — those move bytes — and so does `uninstall`, which
///   moves none but would delete the very bundle a running update is swapping.
fn command_survives_an_update(command: &Command) -> bool {
    matches!(
        command,
        Command::Update { .. }
            | Command::Doctor { .. }
            | Command::Version
            | Command::Config { .. }
            | Command::Lint { .. }
            | Command::Init
            // Nothing at all: the whole answer is a compile-time string, with no
            // config, database or daemon behind it. Blocking it would tell a coding
            // agent to come back later for its own instructions, which is a worse
            // outcome than any it could protect.
            | Command::Skills { .. }
            | Command::InternalTimestamp { .. }
            // **This one IS the update.** `install.sh` runs inside
            // `veld update`'s own lock (`perform_update` → `run_install_script`
            // with the guard still alive) and invokes `veld _helper-install` to
            // hand the new privileged helper to the root helper — the only way a
            // migrated install receives one at all (#262). Gating it makes the
            // lock refuse the update its own run is performing: the command
            // exits 75, install.sh reports the helper as not updated, and the
            // root-owned store keeps the old binary on **every** future
            // `veld update`, forever. That is the wedged updater #338's rule 2
            // exists to prevent, and the only repairs left would be a bare
            // `curl | bash` or a sudo `veld setup privileged`.
            | Command::InternalHelperInstall { .. }
            // Neither touches the daemon, the helper or the installed binaries: one
            // writes a small file in this instance's own shim directory, the other
            // POSTs to localhost and ignores the answer. Both already fail silently,
            // so gating them would buy nothing — and would cost something real,
            // because the gate's message goes to stderr and `agent-state`'s stderr is
            // read by a coding agent. Veld's update banner appearing inside somebody's
            // Claude Code transcript is worse than a badge that does not arrive.
            | Command::AgentSettings { .. }
            | Command::AgentState { .. }
            // Same reason again: it reads one file and prints it into an argv.
            | Command::AgentPrompt { .. }
            | Command::Desktop {
                command: None | Some(DesktopCommand::Status { .. })
            }
            // The **read** half of `veld settings`, for the same reason `Config`
            // is here: `describe` touches nothing at all (it prints a compile-time
            // catalog), while the listing and `get` open the database read-only,
            // exactly as `veld config vars` does. Blocking them produced a
            // *wrong* answer rather than a blocked one — `$(veld settings get
            // terminal.shell)`, a substitution the README advertises, yields an
            // empty string with the explanation on stderr where a script never
            // looks. `set` and `unset` stay blocked: they write, and the daemon
            // they would write through is being restarted.
            | Command::Settings {
                cmd: None | Some(SettingsCmd::Get { .. } | SettingsCmd::Describe { .. }),
                ..
            }
            // The **read** half of `veld backup`, and only that half. Listing reads
            // a directory and opens each artifact, none of which an update touches,
            // and "what can I go back to" is a question somebody asks precisely when
            // an update has gone wrong. `now` and `restore` stay blocked: one writes
            // a copy of a database that may be mid-migration, and the other would
            // swap the live file out from under the migration itself — which is the
            // one way to turn a stuck update into a lost database.
            | Command::Backup { cmd: None, .. }
    )
}

// ---------------------------------------------------------------------------
// Auto-GC
// ---------------------------------------------------------------------------

/// Minimum interval between auto-GC runs.
const AUTO_GC_INTERVAL: Duration = Duration::from_secs(30 * 60); // 30 minutes

/// Trigger a detached `veld gc` subprocess if the last run was more than
/// AUTO_GC_INTERVAL ago. The interval stamp lives in the database and is
/// claimed atomically, so concurrent CLI invocations don't all trigger GC.
/// Using a subprocess keeps GC off the foreground command's critical path
/// and survives `process::exit`.
fn maybe_auto_gc(db: &veld_core::db::Db) {
    if !matches!(
        db.kv_try_claim_interval("gc.last_auto_run", AUTO_GC_INTERVAL),
        Ok(true)
    ) {
        return; // Recent enough (or unreadable) — skip.
    }

    // Spawn a detached `veld gc` subprocess. It runs independently and
    // won't be killed when this process exits.
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .arg("gc")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

// ---------------------------------------------------------------------------
// Update check banner
// ---------------------------------------------------------------------------

/// Minimum interval between update checks.
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60); // 24 hours

const KV_UPDATE_LAST_CHECK: &str = "update.last_check";
const KV_UPDATE_LATEST: &str = "update.latest_version";

/// Whether this invocation should look for, and announce, a newer version.
///
/// The banner is written to **stderr** for a person, so it is skipped when stderr is
/// not a terminal — which is how a coding agent runs every command — and with `--json`.
/// Skipping matters for more than tidiness: the daily check is an inline network
/// request with a 5-second timeout on the command's critical path, and an agent
/// would otherwise pay it (and have the banner land in its transcript) on the first
/// command of each day. The check still happens, and is still stamped only after it
/// succeeds, the next time a human runs one of these in a terminal.
fn update_banner_wanted(command: &Command) -> bool {
    use std::io::IsTerminal;
    !command_wants_json(command) && std::io::stderr().is_terminal()
}

/// `--json` on the commands that run the update check. There is no global flag, so
/// each carries its own.
fn command_wants_json(command: &Command) -> bool {
    match command {
        Command::Status { json, .. }
        | Command::Urls { json, .. }
        | Command::Action { json, .. }
        | Command::Logs { json, .. } => *json,
        Command::Stats(args) => args.json,
        _ => false,
    }
}

/// Check for a new version and print a banner if one is available.
/// When a fetch is needed, it runs inline with the `check_update` timeout
/// (which is capped at a few seconds). Results are cached in the database so
/// subsequent invocations within UPDATE_CHECK_INTERVAL are instant.
async fn maybe_show_update_banner(db: &veld_core::db::Db) {
    let needs_fetch = match db.kv_updated_at(KV_UPDATE_LAST_CHECK) {
        Ok(Some(t)) => {
            let age = chrono::Utc::now() - t;
            age.to_std().unwrap_or_default() >= UPDATE_CHECK_INTERVAL
        }
        _ => true,
    };

    if needs_fetch {
        // Fetch inline — check_update has its own HTTP timeout (10s).
        // We wrap it in an additional 5s timeout to keep CLI snappy.
        let result =
            tokio::time::timeout(Duration::from_secs(5), veld_core::setup::check_update()).await;

        match result {
            Ok(Ok(Some(version))) => {
                let _ = db.kv_set(KV_UPDATE_LATEST, &version);
            }
            Ok(Ok(None)) => {
                // Up to date — clear stale cache.
                let _ = db.kv_delete(KV_UPDATE_LATEST);
            }
            _ => {
                // Timeout or error — leave cache as-is, don't update stamp
                // so we retry next time.
                return;
            }
        }

        // Only touch the stamp after a successful fetch.
        let _ = db.kv_set(KV_UPDATE_LAST_CHECK, "");
    }

    // Show banner from cache.
    if let Ok(Some(latest)) = db.kv_get(KV_UPDATE_LATEST) {
        let latest = latest.trim();
        let current = env!("CARGO_PKG_VERSION");
        if !latest.is_empty() && veld_core::setup::is_newer(latest, current) {
            eprintln!();
            eprintln!(
                "  {} {} → {}. Run {} to upgrade.",
                output::bold("Update available:"),
                output::dim(current),
                output::green(latest),
                output::bold("`veld update`"),
            );
            eprintln!();
        }
    }
}

#[cfg(test)]
mod update_gate_tests {
    use super::*;

    /// A value that begins with `-` reaches the validator instead of clap's
    /// unknown-flag error.
    ///
    /// Two of these are real, not contrived: `-1` is what somebody types to test
    /// the documented "out of range is clamped, not refused" promise, and
    /// `-apple-system` heads most CSS font stacks. Both used to die with
    /// "unexpected argument", which is an error about a flag for something that
    /// is not one.
    #[test]
    fn a_setting_value_may_begin_with_a_hyphen() {
        for value in ["-1", "-apple-system, system-ui", "--weird"] {
            let cli = Cli::try_parse_from(["veld", "settings", "set", "k", value])
                .unwrap_or_else(|e| panic!("clap refused {value:?}: {e}"));
            let Some(Command::Settings {
                cmd: Some(SettingsCmd::Set { value: got, .. }),
                ..
            }) = cli.command
            else {
                panic!("parsed into the wrong command for {value:?}")
            };
            assert_eq!(got, value);
        }
    }

    /// Reading a setting survives an update; writing one does not.
    ///
    /// The split matters because the gate's failure is a *wrong answer*, not a
    /// blocked one: `veld settings get` is advertised in README.md as a shell
    /// substitution, and a blocked run yields an empty string with the
    /// explanation on stderr, where no script looks. `describe` touches nothing
    /// at all — it prints a compile-time catalog — and the listing and `get` open
    /// the database read-only, which is why `Config` is already on the allow-list.
    ///
    /// **Both directions are asserted deliberately.** The allow-list is a
    /// `matches!`, so adding `Settings { .. }` unqualified would silently
    /// re-enable `set` and `unset` during an update — a write aimed at a daemon
    /// that is being restarted — and a test that only checked the reads would
    /// still pass.
    /// The privileged-helper handoff must not be blocked by the update lock,
    /// because it runs **inside** the update that holds it.
    ///
    /// `veld update` takes the lock, then runs `install.sh`, which invokes
    /// `veld _helper-install` — the only way a migrated privileged install ever
    /// receives a new root helper (#262). Gated, that call exits 75, install.sh
    /// records the helper as not updated, and the root-owned store keeps the old
    /// binary on **every** future `veld update`, forever: the wedged updater
    /// #338's rule 2 exists to prevent, repairable only by a bare `curl | bash`
    /// or a sudo `veld setup privileged`.
    ///
    /// Nothing else would notice. The command is invisible to users, install.sh
    /// swallows its status by design, and the message the user does see is the
    /// update-lock banner rather than anything about the helper.
    #[test]
    fn the_privileged_helper_handoff_is_not_blocked_by_the_update_lock() {
        assert!(command_survives_an_update(
            &Command::InternalHelperInstall {
                binary: std::path::PathBuf::from("/tmp/veld-helper"),
            }
        ));
    }

    /// An agent reading its own instructions must never be told to come back
    /// later, and `veld skills` costs nothing to serve — it opens no config, no
    /// database and no socket. The allowlist is one `matches!` arm and a
    /// reshuffle drops it silently, which is why every other entry here has a
    /// test of its own.
    #[test]
    fn reading_the_documentation_survives_an_update() {
        assert!(command_survives_an_update(&Command::Skills {
            topic: None,
            json: false,
        }));
        assert!(command_survives_an_update(&Command::Skills {
            topic: Some("basics".into()),
            json: true,
        }));
    }

    #[test]
    fn reading_a_setting_survives_an_update_but_writing_one_does_not() {
        let settings = |cmd| Command::Settings {
            prefix: None,
            json: false,
            cmd,
        };

        assert!(command_survives_an_update(&settings(None)));
        assert!(command_survives_an_update(&settings(Some(
            SettingsCmd::Get {
                key: "terminal.shell".into(),
                json: false,
            }
        ))));
        assert!(command_survives_an_update(&settings(Some(
            SettingsCmd::Describe {
                key: "terminal.shell".into(),
                json: false,
            }
        ))));

        // The writes reach a daemon the update is restarting.
        assert!(!command_survives_an_update(&settings(Some(
            SettingsCmd::Set {
                key: "terminal.shell".into(),
                value: "/bin/bash".into(),
                json: false,
            }
        ))));
        assert!(!command_survives_an_update(&settings(Some(
            SettingsCmd::Unset {
                key: "terminal.shell".into(),
                json: false,
            }
        ))));
    }

    /// Listing backups survives an update; taking or restoring one does not.
    ///
    /// The same shape as the settings split above and for a sharper reason: an
    /// update is migrating the database, and `restore` would swap the file out from
    /// under the migration — the one way to turn a stuck update into a lost
    /// database. Both directions are asserted because the allow-list is a
    /// `matches!`, so a `Backup { .. }` written unqualified would silently permit
    /// exactly that with the read half's test still passing.
    #[test]
    fn listing_backups_survives_an_update_but_writing_one_does_not() {
        let backup = |cmd| Command::Backup {
            dir: None,
            json: false,
            cmd,
        };

        assert!(command_survives_an_update(&backup(None)));
        assert!(!command_survives_an_update(&backup(Some(BackupCmd::Now {
            json: false
        }))));
        assert!(!command_survives_an_update(&backup(Some(
            BackupCmd::Restore {
                path: None,
                force: false,
                yes: false,
                json: false,
            }
        ))));
    }

    /// The spelling `veld`'s agent wrappers call (`pty/shims.rs` in the daemon): a
    /// rename here would leave every wrapper falling back to typing the prompt in,
    /// with nothing failing to say so.
    #[test]
    fn the_agent_prompt_claim_parses_as_the_wrappers_spell_it() {
        let cli = Cli::try_parse_from(["veld", "agent-prompt", "--take", "/p/x-launch-prompt.txt"])
            .expect("should parse");
        let Some(Command::AgentPrompt { take }) = cli.command else {
            panic!("parsed into the wrong command")
        };
        assert_eq!(take, std::path::Path::new("/p/x-launch-prompt.txt"));
    }

    /// `--dir` reads after the verb, which is where it is typed.
    ///
    /// It is declared on the parent, so without `global` clap accepts it only in
    /// front of the subcommand — and `veld backup now --dir /Volumes/disk` fails
    /// with "unexpected argument", which is what it did first.
    #[test]
    fn the_backup_directory_flag_reads_on_either_side_of_the_verb() {
        for argv in [
            vec!["veld", "backup", "--dir", "/tmp/b", "now"],
            vec!["veld", "backup", "now", "--dir", "/tmp/b"],
        ] {
            let cli = Cli::try_parse_from(&argv).expect("should parse");
            let Some(Command::Backup { dir, cmd, .. }) = cli.command else {
                panic!("parsed into the wrong command for {argv:?}")
            };
            assert_eq!(dir.as_deref(), Some(std::path::Path::new("/tmp/b")));
            assert!(matches!(cmd, Some(BackupCmd::Now { .. })));
        }
    }
}
