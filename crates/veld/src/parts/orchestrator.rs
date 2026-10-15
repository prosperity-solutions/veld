//! `veld orchestrator` (alias `orc`) — the run graph: start it, watch it, read
//! its config.

use clap::Subcommand;

use crate::{commands, output};

#[derive(Subcommand)]
pub enum OrcCmd {
    /// Start an environment.
    Start {
        /// Node selections in the form `node:variant`.
        #[arg(value_name = "NODE:VARIANT")]
        selections: Vec<String>,

        /// Use a preset instead of individual selections: its name, or the key
        /// shown by `veld orchestrator presets`.
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
        /// Repeatable. Never stored — use `veld orchestrator vars set` for that. Visible in
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

        /// Show logs of a specific past run by id prefix (see `veld orchestrator runs list`).
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
        /// posture `veld orchestrator presets --pin` takes): `--json` always emits UTC, so
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

    /// Show URLs of a running environment.
    Urls {
        /// Name of the run to inspect.
        #[arg(long)]
        name: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Run history: list, inspect, or diff run instances.
    Runs {
        #[command(subcommand)]
        cmd: RunsCmd,
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

    /// Print the dependency graph for the given selections.
    Graph {
        /// Node selections in the form `node:variant`.
        #[arg(value_name = "NODE:VARIANT")]
        selections: Vec<String>,
    },

    /// Run a node-defined action against a running environment.
    Action {
        /// Name of the action to run (see `veld orchestrator actions`).
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

    /// Show detailed CPU/memory stats, per subprocess and over time.
    Stats(commands::stats::StatsArgs),

    /// Check veld.json for semantic problems (CI-friendly).
    Lint {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Initialise a new veld.json in the current directory.
    Init,

    /// Print the project's veld.json configuration.
    ///
    /// Read-only. This machine's answers for the vars it declares are
    /// `veld orchestrator vars`.
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
    },

    /// This machine's answers for the vars veld.json declares `machine`-overridable.
    Vars {
        #[command(subcommand)]
        cmd: VarsCmd,
    },
}

#[derive(Subcommand)]
pub enum RunsCmd {
    /// List past and current runs.
    List {
        /// Filter by environment name.
        #[arg(long)]
        name: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one run in full: outcome, node results, and the graph snapshot
    /// it was started with.
    Show {
        /// Run id prefix (see `veld orchestrator runs list`).
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
pub enum VarsCmd {
    /// List every machine-overridable var: its effective value, where that value
    /// came from, and what the config says about it.
    List {
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

pub async fn run(command: OrcCmd) -> i32 {
    match command {
        OrcCmd::Start {
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

        OrcCmd::Stop { name, all } => commands::stop::run(name, all).await,

        OrcCmd::Restart { name, debug } => commands::restart::run(name, debug).await,

        OrcCmd::Runs { cmd } => match cmd {
            RunsCmd::List { name, json } => commands::runs::list(name.as_deref(), json).await,
            RunsCmd::Show { run_id, json } => commands::runs::show(&run_id, json).await,
            RunsCmd::Diff { a, b, json } => commands::runs::diff(&a, b.as_deref(), json).await,
        },

        OrcCmd::Status {
            name,
            outputs,
            json,
        } => commands::status::run(name, outputs, json).await,

        OrcCmd::Stats(args) => commands::stats::run(args).await,

        OrcCmd::Urls { name, json } => commands::urls::run(name, json).await,

        OrcCmd::Action {
            action,
            name,
            node,
            print,
            json,
        } => commands::action::run(action, name, node, print, json).await,

        OrcCmd::Actions { json } => commands::action::list(json).await,

        OrcCmd::Logs {
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

        OrcCmd::Graph { selections } => commands::graph::run(selections).await,

        OrcCmd::Nodes { json } => commands::nodes::run(json).await,

        OrcCmd::Presets { json, pin } => commands::presets::run(json, pin).await,

        OrcCmd::Config {
            path,
            files,
            why,
            json,
        } => commands::config::run(path, files, why, json).await,

        OrcCmd::Vars { cmd } => match cmd {
            VarsCmd::List { json } => commands::config::list_vars(json).await,
            VarsCmd::Set {
                name,
                value,
                env,
                file,
                shell,
                worktree,
                json,
            } => {
                commands::config::set_var(
                    &name,
                    value.as_deref(),
                    env.as_deref(),
                    file.as_deref(),
                    shell.as_deref(),
                    worktree,
                    json,
                )
                .await
            }
            VarsCmd::Unset {
                name,
                worktree,
                json,
            } => commands::config::unset_var(&name, worktree, json).await,
        },

        OrcCmd::Lint { json } => commands::lint::run(json).await,

        OrcCmd::Init => commands::init::run().await,
    }
}

/// The commands that talk to the daemon or the helper, and so get the CLI/daemon
/// version check, auto-GC and the update banner. Exhaustive on purpose, like the
/// two below: a new verb does not compile until somebody decides.
pub fn talks_to_the_daemon(command: &OrcCmd) -> bool {
    match command {
        OrcCmd::Start { .. }
        | OrcCmd::Stop { .. }
        | OrcCmd::Restart { .. }
        | OrcCmd::Status { .. }
        | OrcCmd::Stats(_)
        | OrcCmd::Urls { .. }
        | OrcCmd::Action { .. }
        | OrcCmd::Logs { .. } => true,
        OrcCmd::Runs { .. }
        | OrcCmd::Nodes { .. }
        | OrcCmd::Presets { .. }
        | OrcCmd::Graph { .. }
        | OrcCmd::Actions { .. }
        | OrcCmd::Lint { .. }
        | OrcCmd::Init
        | OrcCmd::Config { .. }
        | OrcCmd::Vars { .. } => false,
    }
}

/// `--json` on the commands [`talks_to_the_daemon`] covers, so the update banner
/// stays off a machine-readable run. There is no global flag, so each carries its
/// own; the rest never show the banner, so their answer does not matter.
pub fn wants_json(command: &OrcCmd) -> bool {
    match command {
        OrcCmd::Status { json, .. }
        | OrcCmd::Urls { json, .. }
        | OrcCmd::Action { json, .. }
        | OrcCmd::Logs { json, .. } => *json,
        OrcCmd::Stats(args) => args.json,
        OrcCmd::Start { .. } | OrcCmd::Stop { .. } | OrcCmd::Restart { .. } => false,
        OrcCmd::Runs { .. }
        | OrcCmd::Nodes { .. }
        | OrcCmd::Presets { .. }
        | OrcCmd::Graph { .. }
        | OrcCmd::Actions { .. }
        | OrcCmd::Lint { .. }
        | OrcCmd::Init
        | OrcCmd::Config { .. }
        | OrcCmd::Vars { .. } => false,
    }
}

/// This part's half of `crate::command_survives_an_update`.
///
/// `config`, `vars list`, `lint` and `init` only read and write files in the
/// project (or read the database, for `vars list`). They are what a coding agent
/// is likely doing in a worktree while an unrelated update runs, and blocking
/// them buys nothing. `vars set` and `vars unset` were allowed too, as part of
/// the old `config`, and stay allowed: they write one database row, not through
/// the daemon.
pub fn survives_an_update(command: &OrcCmd) -> bool {
    match command {
        OrcCmd::Config { .. } | OrcCmd::Vars { .. } | OrcCmd::Lint { .. } | OrcCmd::Init => true,
        OrcCmd::Start { .. }
        | OrcCmd::Stop { .. }
        | OrcCmd::Restart { .. }
        | OrcCmd::Status { .. }
        | OrcCmd::Stats(_)
        | OrcCmd::Urls { .. }
        | OrcCmd::Action { .. }
        | OrcCmd::Logs { .. }
        | OrcCmd::Runs { .. }
        | OrcCmd::Nodes { .. }
        | OrcCmd::Presets { .. }
        | OrcCmd::Graph { .. }
        | OrcCmd::Actions { .. } => false,
    }
}
