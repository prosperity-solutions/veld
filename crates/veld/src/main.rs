mod commands;
mod hints;
mod output;

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Subcommand)]
pub enum SetupCommand {
    /// No-sudo setup: Caddy, daemon, helper on port 8443.
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
    about = "Local development environment orchestrator"
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

    /// Read or wait for feedback submitted via the in-browser overlay.
    Feedback {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Block until new feedback is submitted.
        #[arg(long)]
        wait: bool,

        /// Show all submitted feedback batches.
        #[arg(long)]
        history: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
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

    /// Print version information for all Veld binaries.
    Version,
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
            | Command::Logs { .. }
    );

    if needs_version_check {
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

        Command::Logs {
            name,
            node,
            lines,
            since,
            follow,
            json,
        } => commands::logs::run(name, node, lines, since, follow, json).await,

        Command::Graph { selections } => commands::graph::run(selections).await,

        Command::Nodes { json } => commands::nodes::run(json).await,

        Command::Presets { json } => commands::presets::run(json).await,

        Command::Init => commands::init::run().await,

        Command::List { urls, json } => commands::list::run(urls, json).await,

        Command::Feedback {
            name,
            wait,
            history,
            json,
        } => commands::feedback::run(name, wait, history, json).await,

        Command::Gc => commands::gc::run().await,

        Command::Setup { command } => commands::setup::run(command).await,

        Command::Update => commands::update::run().await,

        Command::Uninstall => commands::uninstall::run().await,

        Command::Version => {
            commands::version::print_version();
            0
        }
    };

    std::process::exit(exit_code);
}

// ---------------------------------------------------------------------------
// Auto-GC
// ---------------------------------------------------------------------------

/// Path to the timestamp file that records the last auto-GC run.
fn auto_gc_stamp_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("veld").join(".last-gc"))
}

/// Minimum interval between auto-GC runs.
const AUTO_GC_INTERVAL: Duration = Duration::from_secs(30 * 60); // 30 minutes

/// Trigger a detached `veld gc` subprocess if the last run was more than
/// AUTO_GC_INTERVAL ago. Using a subprocess avoids race conditions with
/// the foreground command on state files and survives `process::exit`.
fn maybe_auto_gc() {
    let stamp = match auto_gc_stamp_path() {
        Some(p) => p,
        None => return,
    };

    if let Ok(meta) = std::fs::metadata(&stamp) {
        if let Ok(modified) = meta.modified() {
            if SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
                < AUTO_GC_INTERVAL
            {
                return; // Recent enough, skip.
            }
        }
    }

    // Touch the stamp so concurrent CLI invocations don't all trigger GC.
    if let Some(parent) = stamp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&stamp, "");

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

/// Path to the timestamp file that records the last update check.
fn update_check_stamp_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("veld").join(".last-update-check"))
}

/// Path to a file caching the latest known version.
fn update_cache_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("veld").join(".latest-version"))
}

/// Minimum interval between update checks.
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60); // 24 hours

/// Check for a new version and print a banner if one is available.
/// When a fetch is needed, it runs inline with the `check_update` timeout
/// (which is capped at a few seconds). Results are cached to disk so
/// subsequent invocations within UPDATE_CHECK_INTERVAL are instant.
async fn maybe_show_update_banner() {
    let stamp = match update_check_stamp_path() {
        Some(p) => p,
        None => return,
    };
    let cache = match update_cache_path() {
        Some(p) => p,
        None => return,
    };

    let needs_fetch = match std::fs::metadata(&stamp) {
        Ok(meta) => match meta.modified() {
            Ok(modified) => {
                SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or_default()
                    >= UPDATE_CHECK_INTERVAL
            }
            Err(_) => true,
        },
        Err(_) => true,
    };

    if needs_fetch {
        // Fetch inline — check_update has its own HTTP timeout (10s).
        // We wrap it in an additional 5s timeout to keep CLI snappy.
        let result =
            tokio::time::timeout(Duration::from_secs(5), veld_core::setup::check_update()).await;

        // Ensure parent directory exists for stamp and cache files.
        if let Some(parent) = stamp.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match result {
            Ok(Ok(Some(version))) => {
                let _ = std::fs::write(&cache, &version);
            }
            Ok(Ok(None)) => {
                // Up to date — clear stale cache.
                let _ = std::fs::remove_file(&cache);
            }
            _ => {
                // Timeout or error — leave cache as-is, don't update stamp
                // so we retry next time.
                return;
            }
        }

        // Only touch stamp after successful fetch.
        let _ = std::fs::write(&stamp, "");
    }

    // Show banner from cache.
    if let Ok(latest) = std::fs::read_to_string(&cache) {
        let latest = latest.trim();
        let current = env!("CARGO_PKG_VERSION");
        if !latest.is_empty() && latest != current {
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
