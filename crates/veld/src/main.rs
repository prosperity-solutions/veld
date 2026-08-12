mod commands;
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
struct Cli {
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

        /// Stay in the foreground and stream logs (default is detached).
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
    /// Called by the generated `claude`/`codex` wrapper, not by hand. **Nothing of
    /// yours is edited** — no `~/.claude/settings.json`, no `.claude/` in your project,
    /// no `~/.codex/config.toml`. For Claude this writes a per-session settings file
    /// and prints its path; `--settings` merges, so your own configuration still
    /// applies. For Codex there is no file: this prints a literal `-c notify=[...]`
    /// value, which overrides that one key for one invocation.
    ///
    /// Turned off by `terminal.agentIntegration` (Settings → Terminal).
    #[command(name = "agent-settings", hide = true)]
    AgentSettings {
        /// Which agent: `claude` or `codex`.
        #[arg(long)]
        tool: Option<String>,

        /// Terminal session this is for. Defaults to `$VELD_PTY_SESSION`.
        #[arg(long)]
        session: Option<String>,
    },

    /// Report a coding agent's state, from a lifecycle hook.
    ///
    /// Reads the payload on stdin for a tool that pipes it there (Claude); a tool that
    /// appends it as the final argument instead (Codex's `notify`) passes it as
    /// `PAYLOAD` here. Called by the hooks `veld agent-settings` installs, not by hand.
    #[command(name = "agent-state", hide = true)]
    AgentState {
        /// Which agent's payload this is: `claude` or `codex`.
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
        /// than piping it on stdin. Absent for Claude and for `--launched`.
        #[arg(value_name = "PAYLOAD")]
        payload: Option<String>,
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

    /// Share a running environment with a colleague over peer-to-peer.
    Share {
        /// Run to share (defaults to the only active run).
        #[arg(value_name = "RUN")]
        run: Option<String>,
        /// Limit to specific nodes (default: all URL-bearing nodes).
        #[arg(long, value_name = "NODE")]
        node: Vec<String>,
        /// Share lifetime in seconds (default 7200; 3600 for --web).
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

    /// Internal: read stdin, timestamp each line, store it in the central
    /// database. Used by detached server mode to capture process output.
    #[command(name = "_log", hide = true)]
    InternalLog {
        /// Project root the run belongs to.
        #[arg(long)]
        project_root: std::path::PathBuf,
        /// Run name.
        #[arg(long)]
        run: String,
        /// Run instance id (UUID). Optional: detached pipelines started by a
        /// pre-v3 veld invoke this without it.
        #[arg(long)]
        run_id: Option<String>,
        /// Node name.
        #[arg(long)]
        node: String,
        /// Variant name.
        #[arg(long)]
        variant: String,
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    init_tracing(cli.debug);

    // Handle bare `veld` with no subcommand -- print help.
    if cli.command.is_none() {
        let _ = Cli::command().print_help();
        println!();
        return;
    }

    let command = cli.command.unwrap();

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

    // The version gate compares this CLI against the INSTALLED helper/daemon
    // binaries. A dev instance (VELD_DAEMON_PORT set) shares those services
    // deliberately, so a version gap with them is expected — enforce
    // alignment only for the installed instance. VELD_LIB_DIR is the older
    // escape hatch (points version discovery at a dev build dir) and still
    // skips too.
    let is_dev_instance =
        veld_core::instance::daemon_port() != veld_core::instance::DEFAULT_DAEMON_PORT;
    if needs_version_check && std::env::var("VELD_LIB_DIR").is_err() && !is_dev_instance {
        if let Err(msg) = commands::version::check_version_mismatch() {
            output::print_error(&msg, false);
            std::process::exit(1);
        }
    }

    // Auto-GC: trigger background GC if it hasn't run in >30 minutes.
    if needs_version_check {
        maybe_auto_gc();
    }

    // Update check: show banner if a newer version is available (once per day).
    if needs_version_check {
        maybe_show_update_banner().await;
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

        Command::AgentSettings { tool, session } => commands::agent::settings(tool, session),

        Command::AgentState {
            tool,
            session,
            launched,
            payload,
        } => commands::agent::state(tool, session, launched, payload).await,

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
            Some(DesktopCommand::Update {
                version,
                wait_pid,
                relaunch,
                app_path,
            }) => commands::desktop::install(version, wait_pid, relaunch, app_path).await,
        },

        Command::Uninstall => commands::uninstall::run().await,

        Command::Ui => commands::ui::run().await,

        Command::Doctor { json } => commands::doctor::run(json).await,

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
            commands::version::print_version();
            0
        }

        Command::InternalLog {
            project_root,
            run,
            run_id,
            node,
            variant,
        } => {
            // Fast path: no config loading, no network — stdin → database.
            // Used internally by detached server mode; this process outlives
            // the CLI and keeps writing as long as the server produces output.
            //
            // This process is the read end of the server's stdout pipe: if it
            // ever exits while the server is running, the server takes
            // SIGPIPE on its next write and dies. So NOTHING here is fatal —
            // if the database can't be opened (downgrade, transient lock) we
            // keep draining stdin and drop the lines rather than kill the
            // environment.
            use std::io::BufRead;
            let db = veld_core::db::Db::open()
                .map_err(|e| eprintln!("veld _log: failed to open database, dropping logs: {e}"))
                .ok();
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            let mut buf = String::new();

            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let Some(ref db) = db else { continue };
                        let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
                        let _ = db.append_log(
                            &project_root,
                            &run,
                            run_id.as_deref(),
                            Some(&node),
                            Some(&variant),
                            veld_core::db::LogStream::Server,
                            chrono::Utc::now(),
                            trimmed,
                        );
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
/// - `InternalLog` / `InternalTimestamp` are **not** a convenience: they are
///   spawned by the node processes of environments that are still running, and
///   an update deliberately leaves those serving. Blocking them would break live
///   environments to protect an update that never touches them.
/// - `Desktop { Status }` — read-only (it reads a plist), and blocking it
///   produces a *wrong* answer rather than a blocked one: Veld Desktop resolves
///   the CLI by running `veld desktop status --json`, and its caller catches any
///   failure as "there is no veld CLI here", which silently demotes
///   *Check for Updates…* to the download-from-GitHub route. The install and
///   update arms stay blocked — those move bytes.
fn command_survives_an_update(command: &Command) -> bool {
    matches!(
        command,
        Command::Update { .. }
            | Command::Doctor { .. }
            | Command::Version
            | Command::Config { .. }
            | Command::Lint { .. }
            | Command::Init
            | Command::InternalLog { .. }
            | Command::InternalTimestamp { .. }
            // Neither touches the daemon, the helper or the installed binaries: one
            // writes a small file in this instance's own shim directory, the other
            // POSTs to localhost and ignores the answer. Both already fail silently,
            // so gating them would buy nothing — and would cost something real,
            // because the gate's message goes to stderr and `agent-state`'s stderr is
            // read by a coding agent. Veld's update banner appearing inside somebody's
            // Claude Code transcript is worse than a badge that does not arrive.
            | Command::AgentSettings { .. }
            | Command::AgentState { .. }
            | Command::Desktop {
                command: None | Some(DesktopCommand::Status { .. })
            }
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
fn maybe_auto_gc() {
    let Ok(db) = veld_core::db::Db::open() else {
        return;
    };
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

/// Check for a new version and print a banner if one is available.
/// When a fetch is needed, it runs inline with the `check_update` timeout
/// (which is capped at a few seconds). Results are cached in the database so
/// subsequent invocations within UPDATE_CHECK_INTERVAL are instant.
async fn maybe_show_update_banner() {
    let Ok(db) = veld_core::db::Db::open() else {
        return;
    };

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
