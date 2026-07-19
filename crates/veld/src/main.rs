mod commands;
mod hints;
mod output;

use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};

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
    /// Install Hammerspoon menu bar widget (macOS only).
    Hammerspoon,
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

        /// Use a named preset instead of individual selections.
        #[arg(long)]
        preset: Option<String>,

        /// Give the run a custom name.
        #[arg(long)]
        name: Option<String>,

        /// Stay in the foreground and stream logs (default is detached).
        #[arg(long, short = 'a')]
        attach: bool,

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

    /// List environment runs.
    Runs {
        /// Filter by run name.
        #[arg(long)]
        name: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
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

        /// Number of lines to show.
        #[arg(long, default_value = "50")]
        lines: usize,

        /// Only show logs since this duration (e.g. "5m", "1h").
        #[arg(long)]
        since: Option<String>,

        /// Stream logs continuously (like `tail -f`).
        #[arg(long, short = 'f')]
        follow: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        /// Filter by log source: all, server, client, or internal (veld daemon liveness/recovery logs).
        #[arg(long, default_value = "all")]
        source: String,

        /// Filter log lines by search term (case-insensitive substring match).
        #[arg(long, short = 's')]
        search: Option<String>,

        /// Number of context lines to show around search matches.
        #[arg(long, short = 'C', default_value = "0")]
        context: usize,
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
    },

    /// Print the project's veld.json configuration.
    Config {
        /// Print only the path to veld.json instead of its contents.
        #[arg(long)]
        path: bool,

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
    Update,

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
    /// server would die of SIGPIPE on its next write.
    #[command(name = "_timestamp", hide = true)]
    InternalTimestamp {
        /// Path to the log file to append to.
        #[arg(long)]
        log: std::path::PathBuf,
    },
}

fn init_tracing(debug: bool) {
    use tracing_subscriber::EnvFilter;

    let filter = if debug {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"))
    };

    tracing_subscriber::fmt().with_env_filter(filter).init();
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

    // Check for version mismatches on commands that talk to the daemon/helper.
    let needs_version_check = matches!(
        command,
        Command::Start { .. }
            | Command::Stop { .. }
            | Command::Restart { .. }
            | Command::Status { .. }
            | Command::Urls { .. }
            | Command::Action { .. }
            | Command::Logs { .. }
    );

    if needs_version_check && std::env::var("VELD_LIB_DIR").is_err() {
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
            debug,
        } => commands::start::run(selections, preset, name, attach, debug).await,

        Command::Stop { name, all } => commands::stop::run(name, all).await,

        Command::Restart { name, debug } => commands::restart::run(name, debug).await,

        Command::Runs { name, json } => commands::runs::list(name.as_deref(), json).await,

        Command::Status {
            name,
            outputs,
            json,
        } => commands::status::run(name, outputs, json).await,

        Command::Urls { name, json } => commands::urls::run(name, json).await,

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
        } => {
            let source_filter =
                commands::logs::SourceFilter::from_str(&source).unwrap_or_else(|| {
                    output::print_error(
                        &format!(
                            "Invalid --source value '{source}'. Use: all, server, client, internal"
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
            })
            .await
        }

        Command::Graph { selections } => commands::graph::run(selections).await,

        Command::Nodes { json } => commands::nodes::run(json).await,

        Command::Presets { json } => commands::presets::run(json).await,

        Command::Config { path, json } => commands::config::run(path, json).await,

        Command::Init => commands::init::run().await,

        Command::List { urls, json } => commands::list::run(urls, json).await,

        Command::Feedback { command } => commands::feedback::run(command).await,

        Command::Gc => commands::gc::run().await,

        Command::Setup { command } => commands::setup::run(command).await,

        Command::Update => commands::update::run().await,

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
            node,
            variant,
        } => {
            // Fast path: no config loading, no network — stdin → database.
            // Used internally by detached server mode; this process outlives
            // the CLI and keeps writing as long as the server produces output.
            use std::io::BufRead;
            let db = match veld_core::db::Db::open() {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("veld _log: failed to open database: {e}");
                    std::process::exit(1);
                }
            };
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            let mut buf = String::new();

            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
                        let _ = db.append_log(
                            &project_root,
                            &run,
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

            // Keep file handle open for performance; flush after each line
            // so `veld logs -f` can see data immediately.
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
