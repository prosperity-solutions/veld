mod commands;
mod engine_client;
mod hints;
mod output;
mod parts;

use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};

use parts::agent::{AgentCmd, AgentPromptArgs, AgentSettingsArgs, AgentStateArgs};
use parts::ide::{IdeCmd, OpenUrlArgs};
use parts::network::NetworkCmd;
use parts::orchestrator::OrcCmd;
use parts::self_::{DesktopCommand, SelfCmd, UpdateArgs};

/// Veld -- an operating system for agentic development.
#[derive(Parser)]
#[command(
    name = "veld",
    version = env!("CARGO_PKG_VERSION"),
    about = "An operating system for agentic development",
    after_help = "Every command is `veld <part> <verb>`. Bare `veld` shows what is up; \
                  `veld --help-all` lists every command.",
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// List every command of every part, one per line.
    #[arg(long)]
    help_all: bool,

    /// Bare `veld` only: the overview as JSON. Refused with a command, which takes
    /// its own `--json` after the verb: accepting it there and printing human text
    /// would hand a script the wrong format with exit 0.
    #[arg(long)]
    json: bool,

    /// Enable debug logging.
    #[arg(long, global = true)]
    debug: bool,
}

/// The parts of Veld. **No top-level verbs** (#423): a new command goes under the
/// part that owns what it acts on — see `docs/ship.md` → *Adding a command*.
#[derive(Subcommand)]
enum Command {
    /// The run graph: start, stop and watch environments; read their config.
    #[command(visible_alias = "orc")]
    Orchestrator {
        #[command(subcommand)]
        command: OrcCmd,
    },

    /// The Veld window: open files and pages beside a terminal, hand work to the
    /// human as a new worktree.
    Ide {
        #[command(subcommand)]
        command: IdeCmd,
    },

    /// The coding agents in Veld's terminals: their documentation, and the hooks
    /// that report their state.
    Agent {
        #[command(subcommand)]
        command: AgentCmd,
    },

    /// Shares and joins: a run on this machine opened from another.
    Network {
        #[command(subcommand)]
        command: NetworkCmd,
    },

    /// Veld itself: version, update, doctor, setup, settings, backups.
    #[command(name = "self")]
    Myself {
        #[command(subcommand)]
        command: SelfCmd,
    },

    /// Bidirectional feedback threads with the in-browser overlay.
    ///
    /// Left at the top level on purpose: #424 deletes the whole feedback system,
    /// so giving it a part for one release would only move it twice.
    Feedback {
        #[command(subcommand)]
        command: commands::feedback::FeedbackCommand,
    },

    // ---- Pre-#423 names that installed code still calls --------------------
    //
    // Not for people: a person typing an old name gets one line naming the new
    // one (`retired_command`). These are the names *code* calls, and that code
    // outlives the binary it was written against:
    //
    // - `agent-state`, `agent-settings`, `agent-prompt`, `open-url`: hooks and
    //   shims written into every terminal open across the update. The daemon
    //   rewrites them with the new names on its next start; until then they call
    //   these.
    // - `update`, `desktop`: Veld Desktop spawns `veld update --target-version …`
    //   and probes `veld desktop status --json` against whichever CLI is
    //   installed, and an app one release behind its CLI is the normal state
    //   halfway through an update. `install.sh` calls `veld desktop uninstall`.
    //
    // Hidden, and identical to the new spelling — same args struct, same handler.
    //
    // When they can go: `agent-*` and `open-url` once no terminal can still run
    // hooks written before this release — every update restarts the daemon, which
    // rewrites them, so one release later. `update` and `desktop` only together
    // with Veld Desktop's handoff: the app talks to whichever CLI is installed,
    // older or newer, so it keeps the spelling every CLI understands. The shims and
    // hooks themselves use the new spellings because the daemon that writes them
    // and the CLI they call ship as one release. The exception is a downgrade
    // (`VELD_VERSION=16.x`): hooks baked into an agent at its launch keep calling
    // `agent state`, which the older CLI does not have, until that agent restarts.
    // There is no alias to add in that direction; the badge goes quiet, nothing
    // breaks.
    #[command(name = "agent-state", hide = true)]
    LegacyAgentState(AgentStateArgs),

    #[command(name = "agent-settings", hide = true)]
    LegacyAgentSettings(AgentSettingsArgs),

    #[command(name = "agent-prompt", hide = true)]
    LegacyAgentPrompt(AgentPromptArgs),

    #[command(name = "open-url", hide = true)]
    LegacyOpenUrl(OpenUrlArgs),

    #[command(name = "update", hide = true)]
    LegacyUpdate(UpdateArgs),

    #[command(name = "desktop", hide = true)]
    LegacyDesktop {
        #[command(subcommand)]
        command: Option<DesktopCommand>,
    },

    // ---- Plumbing ----------------------------------------------------------
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
    /// pipelines keep writing FILES, which the DB-backed `veld orchestrator logs`
    /// does not read — a pre-upgrade run stays visible and stoppable but its
    /// ongoing output is only in `.veld/logs/` until the run is restarted.
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

/// What a pre-#423 top-level command is called now, or `None` when `args` does
/// not start with one.
///
/// `args` is argv without the program name. The new spelling depends on the
/// verb for the old commands whose bare form used to list (`runs`, `settings`,
/// `backup`), for the old `config`, whose var verbs moved to `veld orchestrator
/// vars`, and for the old `setup`, so the answer can be retyped as is.
///
/// The names installed code still calls — `update`, `desktop`, `agent-*`,
/// `open-url` — are not here: they still work (see `Command`).
fn retired_command(args: &[String]) -> Option<String> {
    // `--debug` is global and may come first; nothing else at the root takes a
    // value, so the first word that is not a flag is the command.
    let at = args.iter().position(|a| !a.starts_with('-'))?;
    let first = args[at].as_str();
    // The verb is looked for among *all* later words, not taken as the second
    // one: a flag's value can sit in between (the old `backup --dir D now`), and
    // reading `D` as the verb would name the wrong command. The values of the old
    // flags that took one are skipped for the same reason the other way round: an
    // environment called `show` (`runs --name show`) is not the verb `show`.
    const TAKES_A_VALUE: &[&str] = &["--name", "--dir", "--node", "--env", "--file", "--shell"];
    let words: Vec<&str> = args[at + 1..]
        .iter()
        .scan(false, |value_next, w| {
            let is_value = std::mem::replace(value_next, TAKES_A_VALUE.contains(&w.as_str()));
            Some((!is_value).then_some(w.as_str()))
        })
        .flatten()
        .collect();
    let verb = |verbs: &[&'static str]| {
        words
            .iter()
            .find_map(|w| verbs.iter().copied().find(|v| v == w))
    };
    let new = match first {
        "start" | "stop" | "restart" | "status" | "logs" | "urls" | "nodes" | "presets"
        | "graph" | "action" | "actions" | "stats" | "lint" | "init" => {
            format!("orchestrator {first}")
        }
        "runs" => match verb(&["show", "diff"]) {
            Some(verb) => format!("orchestrator runs {verb}"),
            None => "orchestrator runs list".into(),
        },
        "config" => match verb(&["vars", "set", "unset"]) {
            Some("vars") => "orchestrator vars list".into(),
            Some(verb) => format!("orchestrator vars {verb}"),
            None => "orchestrator config".into(),
        },
        "ui" => "ide dashboard".into(),
        "skills" => "agent skills".into(),
        "share" | "join" | "unshare" | "leave" | "approve" | "deny" => format!("network {first}"),
        "shares" => "network list".into(),
        "setup" => match verb(&["privileged", "unprivileged"]) {
            Some(verb) => format!("self setup {verb}"),
            None => "self setup".into(),
        },
        "doctor" | "version" | "gc" | "uninstall" => format!("self {first}"),
        "settings" => match verb(&["get", "set", "unset", "describe"]) {
            Some(verb) => format!("self settings {verb}"),
            None => "self settings list".into(),
        },
        "backup" => match verb(&["now", "restore"]) {
            Some(verb) => format!("self backup {verb}"),
            None => "self backup list".into(),
        },
        "list" => "self projects list".into(),
        _ => return None,
    };
    Some(format!("veld {new}"))
}

/// The root `--json` is bare `veld`'s. clap cannot say "conflicts with any
/// subcommand" for one flag without also refusing the global `--debug` there, so
/// it is checked here.
fn misplaced_root_json(cli: &Cli) -> Option<&'static str> {
    (cli.json && cli.command.is_some())
        .then_some("veld: --json goes after the command (`veld <part> <verb> --json`); before it, it is only for bare `veld`")
}

/// `veld --help-all`: every visible command of every part, one per line, with
/// its one-line description. `--help` stays at the parts so it fits a screen;
/// this is the page an agent greps.
fn print_help_all() {
    fn walk(cmd: &clap::Command, path: &str, rows: &mut Vec<(String, String)>) {
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
            let path = format!("{path} {}", sub.get_name());
            let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
            if sub.has_subcommands() {
                // A parent that also runs on its own (`veld self desktop`) is a
                // command too, so it gets a row before its verbs.
                if !sub.is_subcommand_required_set() {
                    rows.push((path.clone(), about));
                }
                walk(sub, &path, rows);
            } else {
                rows.push((path, about));
            }
        }
    }
    let mut rows = Vec::new();
    walk(&Cli::command(), "veld", &mut rows);
    let width = rows.iter().map(|(p, _)| p.len()).max().unwrap_or(0);
    for (path, about) in rows {
        println!("{path:<width$}  {about}");
    }
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
    // output under `veld orchestrator start --oneshot`.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
}

/// A plain `fn main`, not `#[tokio::main]`: the runtime is built only once a command
/// that needs one has been picked.
///
/// `veld agent state` is the reason. Claude Code runs it from a blocking hook at least
/// twice per turn, and all it does is one POST to `127.0.0.1` (two, when an older
/// daemon refuses the naming turn) — so it is dispatched here, to a synchronous path,
/// before a multi-thread runtime (one worker per core) or a TLS stack exists. It was never behind the update gate or the version check
/// (see [`command_survives_an_update`] and `needs_version_check` in [`run`]), so
/// answering it before them changes nothing it used to do.
fn main() {
    // Before clap: a retired name would otherwise get clap's "unrecognized
    // subcommand" and a usage dump. One line naming the new spelling is the only
    // crumb (#423), and it exits non-zero so a script that used it fails loudly.
    // Lossy, not `std::env::args`, which panics on an argument that is not UTF-8
    // (a path, say) — clap still reads the real `OsString`s below.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(new) = retired_command(&args) {
        // Asked for help on an old name: point at the new name's help, which is
        // the thing they were after.
        let help = if args.iter().any(|a| a == "--help" || a == "-h") {
            " --help"
        } else {
            ""
        };
        eprintln!("that is `{new}{help}` now");
        std::process::exit(2);
    }
    // `veld help [PART [VERB]]` — clap's `help` subcommand is off so `--help` lists
    // parts only, but the spelling is a habit, and refusing it with "did you mean
    // `_helper-install`" pointed agents at a hidden installer.
    // Found as the first word that is not a flag, as `retired_command` finds the
    // command, so `veld --debug help` works too; an old name after it gets its crumb.
    if let Some(at) = args.iter().position(|a| !a.starts_with('-'))
        && args[at] == "help"
    {
        let rest = &args[at + 1..];
        if rest.iter().any(|a| a == "--help-all") {
            print_help_all();
            std::process::exit(0);
        }
        if let Some(new) = retired_command(rest) {
            eprintln!("that is `{new} --help` now");
            std::process::exit(2);
        }
        let mut argv: Vec<String> = vec!["veld".into()];
        argv.extend(rest.iter().cloned());
        argv.push("--help".into());
        if let Err(e) = Cli::command().try_get_matches_from(argv) {
            e.exit();
        }
    }

    let cli = Cli::parse();
    if let Some(message) = misplaced_root_json(&cli) {
        eprintln!("{message}");
        std::process::exit(2);
    }

    init_tracing(cli.debug);

    if cli.help_all {
        print_help_all();
        return;
    }

    let command = match cli.command {
        None => None,
        Some(Command::LegacyAgentState(args))
        | Some(Command::Agent {
            command: AgentCmd::State(args),
        }) => std::process::exit(parts::agent::state(args)),
        Some(command) => Some(command),
    };

    // Exactly what `#[tokio::main]` expanded to: a multi-thread runtime, every driver on.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the tokio runtime");
    match command {
        // Bare `veld`: the OS at a glance.
        None => std::process::exit(runtime.block_on(commands::overview::run(cli.json))),
        Some(command) => runtime.block_on(run(command)),
    }
}

async fn run(command: Command) {
    // An update in flight is replacing veld, veld-daemon, veld-helper and caddy
    // and restarting both services. Nearly every command either execs one of
    // those binaries or expects a daemon that is currently down, so it gets a
    // straight answer here instead of an unexplained connection error thirty
    // seconds into a `veld orchestrator start`. See `command_survives_an_update`
    // for the allow-list and why each entry is on it.
    if !command_survives_an_update(&command) {
        if let Some(state) = veld_core::update_lock::current() {
            output::print_error(&state.describe(chrono::Utc::now()), false);
            eprintln!(
                "  {}",
                output::dim(
                    "Commands that touch the daemon, the helper or the installed binaries are \
                     unavailable until it finishes. Watch it with `veld self update --status`."
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
        &command,
        Command::Orchestrator { command } if parts::orchestrator::talks_to_the_daemon(command)
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
        Command::Orchestrator { command } => parts::orchestrator::run(command).await,
        Command::Ide { command } => parts::ide::run(command).await,
        Command::Agent { command } => parts::agent::run(command),
        Command::Network { command } => parts::network::run(command).await,
        Command::Myself { command } => parts::self_::run(command).await,
        Command::Feedback { command } => commands::feedback::run(command).await,

        // Normally answered in `main`, before any runtime exists; kept so the match
        // stays exhaustive and a future caller of `run` gets the same behaviour.
        Command::LegacyAgentState(args) => parts::agent::state(args),
        Command::LegacyAgentSettings(args) => parts::agent::run(AgentCmd::Settings(args)),
        Command::LegacyAgentPrompt(args) => parts::agent::run(AgentCmd::Prompt(args)),
        Command::LegacyOpenUrl(args) => parts::ide::open_url(args).await,
        Command::LegacyUpdate(args) => parts::self_::update(args).await,
        Command::LegacyDesktop { command } => parts::self_::desktop(command).await,

        Command::InternalHelperInstall { binary } => commands::helper_install::run(binary).await,
        Command::InternalCliDump => commands::cli_dump::run(),

        Command::InternalTimestamp { log } => {
            // Fast path: no config loading, no network, just stdin → timestamped log file.
            // Used internally by detached server mode.
            use std::io::{BufRead, Write};
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            let mut buf = String::new();

            // Keep file handle open for performance; write per line so any
            // file tailer sees data immediately. (The DB-backed `veld orchestrator logs`
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
// ---------------------------------------------------------------------------
// Update gate
// ---------------------------------------------------------------------------

/// Which commands stay available while an update is running.
///
/// An **allow-list**, not a block-list, and that direction is the decision. The
/// unsafe set is "anything that execs a binary being replaced, or talks to a
/// service being restarted", which is most of the CLI and grows every time a
/// subcommand is added — so a block-list would silently stop covering new
/// commands, and the symptom would be a corrupted update rather than a compile
/// error. Adding a command therefore means it is blocked during an update until
/// someone deliberately puts it here.
///
/// Each part answers for its own commands, exhaustively, with the reasons beside
/// them (`survives_an_update` in each of `parts::orchestrator`, `ide`, `agent`,
/// `network` and `self_`). What is decided here:
///
/// - `agent` answers verb by verb in `parts::agent::survives_an_update`; the
///   hidden `agent-*` names answer as their new spellings do.
/// - `InternalTimestamp` is **not** a convenience: it is spawned by the node
///   processes of environments that are still running, and an update
///   deliberately leaves those serving. Blocking it would break live
///   environments to protect an update that never touches them.
/// - `InternalHelperInstall` **is the update.** `install.sh` runs inside
///   `veld self update`'s own lock and invokes `veld _helper-install` to hand the
///   new privileged helper to the root helper — the only way a migrated install
///   receives one at all (#262). Gating it makes the lock refuse the update its
///   own run is performing, and the root-owned store keeps the old binary on
///   **every** future update, forever (#338's rule 2).
/// - The legacy names answer exactly as their new spellings do.
fn command_survives_an_update(command: &Command) -> bool {
    match command {
        Command::Orchestrator { command } => parts::orchestrator::survives_an_update(command),
        Command::Myself { command } => parts::self_::survives_an_update(command),
        Command::Agent { command } => parts::agent::survives_an_update(command),
        Command::LegacyAgentState(_)
        | Command::LegacyAgentSettings(_)
        | Command::LegacyAgentPrompt(_)
        | Command::LegacyUpdate(_)
        | Command::InternalTimestamp { .. }
        | Command::InternalHelperInstall { .. } => true,
        Command::LegacyDesktop { command } => parts::self_::desktop_survives_an_update(command),
        Command::Ide { command } => parts::ide::survives_an_update(command),
        Command::Network { command } => parts::network::survives_an_update(command),
        Command::Feedback { .. } | Command::LegacyOpenUrl(_) | Command::InternalCliDump => false,
    }
}

// ---------------------------------------------------------------------------
// Auto-GC
// ---------------------------------------------------------------------------

/// Minimum interval between auto-GC runs.
const AUTO_GC_INTERVAL: Duration = Duration::from_secs(30 * 60); // 30 minutes

/// Trigger a detached `veld self gc` subprocess if the last run was more than
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

    // Spawn a detached `veld self gc` subprocess. It runs independently and
    // won't be killed when this process exits.
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .args(["self", "gc"])
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
        Command::Orchestrator { command } => parts::orchestrator::wants_json(command),
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
                output::bold("`veld self update`"),
            );
            eprintln!();
        }
    }
}

#[cfg(test)]
mod update_gate_tests {
    use super::*;
    use parts::self_::{BackupCmd, SettingsCmd};

    fn parse(argv: &[&str]) -> Command {
        Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("clap refused {argv:?}: {e}"))
            .command
            .unwrap_or_else(|| panic!("no command for {argv:?}"))
    }

    fn survives(argv: &[&str]) -> bool {
        command_survives_an_update(&parse(argv))
    }

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
            let Command::Myself {
                command:
                    SelfCmd::Settings {
                        cmd: SettingsCmd::Set { value: got, .. },
                    },
            } = parse(&["veld", "self", "settings", "set", "k", value])
            else {
                panic!("parsed into the wrong command for {value:?}")
            };
            assert_eq!(got, value);
        }
    }

    /// The privileged-helper handoff must not be blocked by the update lock,
    /// because it runs **inside** the update that holds it.
    ///
    /// `veld self update` takes the lock, then runs `install.sh`, which invokes
    /// `veld _helper-install` — the only way a migrated privileged install ever
    /// receives a new root helper (#262). Gated, that call exits 75, install.sh
    /// records the helper as not updated, and the root-owned store keeps the old
    /// binary on **every** future update, forever: the wedged updater #338's
    /// rule 2 exists to prevent.
    #[test]
    fn the_privileged_helper_handoff_is_not_blocked_by_the_update_lock() {
        assert!(survives(&[
            "veld",
            "_helper-install",
            "--binary",
            "/tmp/veld-helper"
        ]));
    }

    /// An agent reading its own instructions must never be told to come back
    /// later, and `veld agent skills` costs nothing to serve.
    #[test]
    fn reading_the_documentation_survives_an_update() {
        assert!(survives(&["veld", "agent", "skills"]));
        assert!(survives(&["veld", "agent", "skills", "basics", "--json"]));
    }

    /// Reading a setting survives an update; writing one does not.
    ///
    /// The split matters because the gate's failure is a *wrong answer*, not a
    /// blocked one: `veld self settings get` is advertised in README.md as a
    /// shell substitution, and a blocked run yields an empty string with the
    /// explanation on stderr, where no script looks.
    ///
    /// **Both directions are asserted deliberately**: a `Settings { .. }` written
    /// unqualified would silently re-enable `set` and `unset` during an update,
    /// and a test that only checked the reads would still pass.
    #[test]
    fn reading_a_setting_survives_an_update_but_writing_one_does_not() {
        assert!(survives(&["veld", "self", "settings", "list"]));
        assert!(survives(&[
            "veld",
            "self",
            "settings",
            "get",
            "terminal.shell"
        ]));
        assert!(survives(&[
            "veld",
            "self",
            "settings",
            "describe",
            "terminal.shell"
        ]));
        assert!(!survives(&[
            "veld",
            "self",
            "settings",
            "set",
            "terminal.shell",
            "/bin/bash"
        ]));
        assert!(!survives(&[
            "veld",
            "self",
            "settings",
            "unset",
            "terminal.shell"
        ]));
    }

    /// Listing backups survives an update; taking or restoring one does not —
    /// `restore` would swap the file out from under the migration, the one way
    /// to turn a stuck update into a lost database. Both directions, as above.
    #[test]
    fn listing_backups_survives_an_update_but_writing_one_does_not() {
        assert!(survives(&["veld", "self", "backup", "list"]));
        assert!(!survives(&["veld", "self", "backup", "now"]));
        assert!(!survives(&["veld", "self", "backup", "restore"]));
    }

    /// Veld Desktop probes the CLI with `veld desktop status --json`; a blocked
    /// probe reads as "no CLI here". Installing and uninstalling stay blocked.
    #[test]
    fn the_desktop_probe_survives_an_update_under_both_spellings() {
        for prefix in [&["veld", "desktop"][..], &["veld", "self", "desktop"][..]] {
            let with = |rest: &[&'static str]| [prefix, rest].concat();
            assert!(survives(&with(&["status", "--json"])));
            assert!(survives(&with(&[])));
            assert!(!survives(&with(&["install"])));
            assert!(!survives(&with(&["uninstall", "--yes"])));
        }
    }

    /// The project-file commands an agent runs in a worktree are not blocked by
    /// an unrelated update; the ones that drive the daemon are.
    #[test]
    fn project_file_commands_survive_an_update_and_run_commands_do_not() {
        for argv in [
            &["veld", "orchestrator", "config"][..],
            &["veld", "orc", "vars", "list"],
            &["veld", "orc", "lint"],
            &["veld", "orc", "init"],
        ] {
            assert!(survives(argv), "{argv:?} should survive");
        }
        for argv in [
            &["veld", "orc", "start"][..],
            &["veld", "orc", "stop"],
            &["veld", "ide", "dashboard"],
            &["veld", "network", "list"],
        ] {
            assert!(!survives(argv), "{argv:?} should be blocked");
        }
    }

    /// The spellings installed code calls (`pty/shims.rs` and `agent.rs`'s hook
    /// documents in this release and every earlier one; Veld Desktop; install.sh)
    /// parse, and parse to the same thing as their new names. A terminal open
    /// across the update runs hooks written before it, and a rename here leaves
    /// every one of them failing quietly.
    #[test]
    fn the_names_installed_code_calls_still_parse() {
        for argv in [
            &["veld", "agent-state", "--tool", "claude", "--session", "s"][..],
            &[
                "veld",
                "agent-state",
                "--tool",
                "codex",
                "--session",
                "s",
                "{}",
            ],
            &["veld", "agent-state", "--tool", "claude", "--launched"],
            &["veld", "agent-settings", "--tool", "claude"],
            &["veld", "agent-prompt", "--take", "/p/x-launch-prompt.txt"],
            &["veld", "open-url", "--tool", "open", "--", "https://x.test"],
            &[
                "veld",
                "update",
                "--target-version",
                "1.2.3",
                "--wait-pid",
                "1",
                "--relaunch",
                "--app-path",
                "/A",
                "--console",
            ],
            &["veld", "update", "--status", "--json"],
            &[
                "veld",
                "desktop",
                "update",
                "--version",
                "1.2.3",
                "--wait-pid",
                "1",
                "--relaunch",
                "--app-path",
                "/A",
            ],
            &["veld", "desktop", "status", "--json"],
            &["veld", "desktop", "uninstall", "--yes"],
            &["veld", "_helper-install", "--binary", "/tmp/h"],
            &["veld", "_timestamp", "--log", "/tmp/l"],
        ] {
            parse(argv);
            assert!(
                retired_command(&argv[1..].iter().map(|s| s.to_string()).collect::<Vec<_>>())
                    .is_none(),
                "{argv:?} must run, not be retired"
            );
        }
        let Command::LegacyAgentPrompt(args) =
            parse(&["veld", "agent-prompt", "--take", "/p/x-launch-prompt.txt"])
        else {
            panic!("parsed into the wrong command")
        };
        assert_eq!(args.take, std::path::Path::new("/p/x-launch-prompt.txt"));
    }

    /// A hidden pre-#423 name is gated exactly as its new spelling is. The legacy
    /// arms of `command_survives_an_update` answer directly rather than through
    /// the part, so this is what keeps them from drifting when a part's answer
    /// changes.
    #[test]
    fn a_legacy_name_survives_an_update_exactly_as_its_new_spelling_does() {
        for (old, new) in [
            (
                &["veld", "agent-state", "--launched"][..],
                &["veld", "agent", "state", "--launched"][..],
            ),
            (&["veld", "agent-settings"], &["veld", "agent", "settings"]),
            (
                &["veld", "agent-prompt", "--take", "/p"],
                &["veld", "agent", "prompt", "--take", "/p"],
            ),
            (
                &["veld", "open-url", "https://x.test"],
                &["veld", "ide", "open-url", "https://x.test"],
            ),
            (&["veld", "update"], &["veld", "self", "update"]),
            (
                &["veld", "update", "--status"],
                &["veld", "self", "update", "--status"],
            ),
            (&["veld", "desktop"], &["veld", "self", "desktop"]),
            (
                &["veld", "desktop", "status"],
                &["veld", "self", "desktop", "status"],
            ),
            (
                &["veld", "desktop", "install"],
                &["veld", "self", "desktop", "install"],
            ),
            (
                &["veld", "desktop", "uninstall", "--yes"],
                &["veld", "self", "desktop", "uninstall", "--yes"],
            ),
        ] {
            assert_eq!(survives(old), survives(new), "{old:?} vs {new:?}");
        }
    }

    /// Every old top-level command answers with its new name — the only crumb
    /// #423 leaves — and the name it gives is one clap actually accepts.
    #[test]
    fn every_retired_name_points_at_a_command_that_exists() {
        let cases: &[(&[&str], &str)] = &[
            (&["start", "web:local"], "veld orchestrator start"),
            (&["--debug", "stop"], "veld orchestrator stop"),
            (&["restart"], "veld orchestrator restart"),
            (&["status"], "veld orchestrator status"),
            (&["logs", "-f"], "veld orchestrator logs"),
            (&["urls"], "veld orchestrator urls"),
            (&["nodes"], "veld orchestrator nodes"),
            (&["presets"], "veld orchestrator presets"),
            (&["graph"], "veld orchestrator graph"),
            (&["action", "seed"], "veld orchestrator action"),
            (&["actions"], "veld orchestrator actions"),
            (&["stats"], "veld orchestrator stats"),
            (&["lint"], "veld orchestrator lint"),
            (&["init"], "veld orchestrator init"),
            (&["runs"], "veld orchestrator runs list"),
            (&["runs", "show", "ab12"], "veld orchestrator runs show"),
            (&["runs", "diff", "ab12"], "veld orchestrator runs diff"),
            (&["config"], "veld orchestrator config"),
            (&["config", "--files"], "veld orchestrator config"),
            (&["config", "vars"], "veld orchestrator vars list"),
            (&["config", "set", "X", "1"], "veld orchestrator vars set"),
            (&["config", "unset", "X"], "veld orchestrator vars unset"),
            (&["ui"], "veld ide dashboard"),
            (&["skills", "ide"], "veld agent skills"),
            (&["share"], "veld network share"),
            (&["join", "t"], "veld network join"),
            (&["shares"], "veld network list"),
            (&["unshare"], "veld network unshare"),
            (&["leave"], "veld network leave"),
            (&["approve", "i"], "veld network approve"),
            (&["deny", "i"], "veld network deny"),
            (&["doctor"], "veld self doctor"),
            (&["setup"], "veld self setup"),
            (&["setup", "privileged"], "veld self setup privileged"),
            (&["version"], "veld self version"),
            (&["gc"], "veld self gc"),
            (&["uninstall"], "veld self uninstall"),
            (&["settings"], "veld self settings list"),
            (&["settings", "get", "k"], "veld self settings get"),
            (
                &["settings", "describe", "k"],
                "veld self settings describe",
            ),
            (&["backup"], "veld self backup list"),
            (&["backup", "now"], "veld self backup now"),
            (&["backup", "restore"], "veld self backup restore"),
            (
                &["backup", "--dir", "/tmp/b", "now"],
                "veld self backup now",
            ),
            (
                &["runs", "--name", "x", "show", "ab"],
                "veld orchestrator runs show",
            ),
            (&["runs", "--name", "show"], "veld orchestrator runs list"),
            (
                &["settings", "--json", "get", "k"],
                "veld self settings get",
            ),
            (&["config", "--json", "vars"], "veld orchestrator vars list"),
            (&["list"], "veld self projects list"),
        ];
        for (old, new) in cases {
            let args: Vec<String> = old.iter().map(|s| s.to_string()).collect();
            assert_eq!(retired_command(&args).as_deref(), Some(*new), "for {old:?}");
            // The new spelling must be a real command: walk it in the clap tree.
            let mut cmd = Cli::command();
            for word in new.split(' ').skip(1) {
                cmd = cmd
                    .find_subcommand(word)
                    .unwrap_or_else(|| panic!("{new} names no command ({word})"))
                    .clone();
            }
        }
        assert!(retired_command(&[]).is_none());
        assert!(retired_command(&["orchestrator".into(), "start".into()]).is_none());
    }

    /// The root `--json` belongs to bare `veld`; with a command it is refused, not
    /// silently dropped.
    #[test]
    fn the_root_json_flag_is_refused_with_a_command() {
        let parse = |argv: &[&str]| Cli::try_parse_from(argv).expect("parses");
        assert!(misplaced_root_json(&parse(&["veld", "--json"])).is_none());
        assert!(misplaced_root_json(&parse(&["veld", "--json", "self", "version"])).is_some());
        assert!(misplaced_root_json(&parse(&["veld", "--debug", "self", "version"])).is_none());
    }

    /// `--help` lists parts only (#423): every visible top-level command is a
    /// part, plus `feedback` until #424 deletes it.
    #[test]
    fn help_lists_parts_only() {
        let mut visible: Vec<String> = Cli::command()
            .get_subcommands()
            .filter(|s| !s.is_hide_set())
            .map(|s| s.get_name().to_string())
            .collect();
        visible.sort();
        let mut expected: Vec<String> = commands::overview::PARTS
            .iter()
            .map(|s| s.to_string())
            .chain(["feedback".to_string()])
            .collect();
        expected.sort();
        assert_eq!(visible, expected);
    }

    /// `--dir` reads after the verb, which is where it is typed.
    ///
    /// It is declared on the parent, so without `global` clap accepts it only in
    /// front of the subcommand — and `veld self backup now --dir /Volumes/disk`
    /// fails with "unexpected argument", which is what it did first.
    #[test]
    fn the_backup_directory_flag_reads_on_either_side_of_the_verb() {
        for argv in [
            &["veld", "self", "backup", "--dir", "/tmp/b", "now"][..],
            &["veld", "self", "backup", "now", "--dir", "/tmp/b"],
        ] {
            let Command::Myself {
                command: SelfCmd::Backup { dir, cmd },
            } = parse(argv)
            else {
                panic!("parsed into the wrong command for {argv:?}")
            };
            assert_eq!(dir.as_deref(), Some(std::path::Path::new("/tmp/b")));
            assert!(matches!(cmd, BackupCmd::Now { .. }));
        }
    }
}
