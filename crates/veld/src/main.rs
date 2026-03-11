mod commands;
mod output;

use clap::{Parser, Subcommand};

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

    /// Manage environment runs.
    Runs {
        #[command(subcommand)]
        action: Option<RunsAction>,

        /// Show all runs (including stopped).
        #[arg(long)]
        all: bool,

        /// Filter by run name.
        #[arg(long)]
        name: Option<String>,
    },

    /// Show status of a running environment.
    Status {
        /// Name of the run to inspect.
        #[arg(long)]
        name: Option<String>,

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

    /// Garbage-collect stale state and logs.
    Gc,

    /// Run the first-time setup sequence.
    Setup,

    /// Update Veld to the latest version.
    Update,

    /// Uninstall Veld and clean up.
    Uninstall,
}

#[derive(Subcommand)]
enum RunsAction {
    /// Purge a stopped run's state and logs.
    Purge {
        /// Name of the run to purge.
        #[arg(long)]
        name: String,
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

    // Handle bare `veld` with no subcommand -- print version.
    if cli.command.is_none() {
        commands::version::print_version();
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

    let exit_code = match command {
        Command::Start {
            selections,
            preset,
            name,
            debug,
        } => commands::start::run(selections, preset, name, debug).await,

        Command::Stop { name, all } => commands::stop::run(name, all).await,

        Command::Restart { name, debug } => commands::restart::run(name, debug).await,

        Command::Runs { action, all, name } => match action {
            Some(RunsAction::Purge { name }) => commands::runs::purge(&name).await,
            None => commands::runs::list(all, name.as_deref()).await,
        },

        Command::Status { name, json } => commands::status::run(name, json).await,

        Command::Urls { name, json } => commands::urls::run(name, json).await,

        Command::Logs {
            name,
            node,
            lines,
            since,
            json,
        } => commands::logs::run(name, node, lines, since, json).await,

        Command::Graph { selections } => commands::graph::run(selections).await,

        Command::Nodes { json } => commands::nodes::run(json).await,

        Command::Presets { json } => commands::presets::run(json).await,

        Command::Init => commands::init::run().await,

        Command::List { urls, json } => commands::list::run(urls, json).await,

        Command::Gc => commands::gc::run().await,

        Command::Setup => commands::setup::run().await,

        Command::Update => commands::update::run().await,

        Command::Uninstall => commands::uninstall::run().await,
    };

    std::process::exit(exit_code);
}
