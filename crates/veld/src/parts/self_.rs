//! `veld self` — Veld itself: its install, its settings, its database.

use clap::{Args, Subcommand};

use crate::commands;

#[derive(Subcommand)]
pub enum SelfCmd {
    /// Print version information for all Veld binaries.
    Version,

    /// Update Veld to the latest version.
    ///
    /// Moves both halves of the release: the CLI, daemon and helper, and — on
    /// macOS — Veld Desktop. When the app is running it is closed first, with the
    /// user's agreement, and reopened afterwards; its bundle cannot be replaced
    /// while it runs.
    Update(UpdateArgs),

    /// Diagnose installation and service health.
    Doctor {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Run first-time setup or manage setup configuration.
    Setup {
        #[command(subcommand)]
        command: Option<SetupCommand>,
    },

    /// Read and change Veld's own settings.
    ///
    /// These are machine-wide preferences — which shell terminals open, where new
    /// worktrees land, whether Veld may keep this machine awake — and they are
    /// the same ones the settings dialog in the IDE edits.
    ///
    /// Not to be confused with `veld orchestrator config`, which is about a *project's*
    /// veld.json and the machine-overridable vars it declares.
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },

    /// Copies of Veld's own database: list them, take one, put one back.
    ///
    /// Everything Veld knows — repositories, worktrees, lanes, layouts, run
    /// history, settings — lives in one SQLite file, and the daemon keeps compact
    /// copies of it on the interval in `veld self settings list backup`.
    Backup {
        /// Where to look, overriding the `backup.dir` setting for this command.
        // `global` so it reads naturally after the verb — `veld self backup now --dir
        // /Volumes/disk` — rather than only in front of it, which is where a
        // parent-only flag has to go and is not where anybody types it. A `//`
        // comment, not a doc comment: clap renders those as help text, and this is
        // a note to the next maintainer rather than to a user.
        #[arg(long, value_name = "DIR", global = true)]
        dir: Option<std::path::PathBuf>,
        #[command(subcommand)]
        cmd: BackupCmd,
    },

    /// The Veld projects on this machine.
    Projects {
        #[command(subcommand)]
        cmd: ProjectsCmd,
    },

    /// Garbage-collect stale state and logs.
    Gc,

    /// Install, update or inspect Veld Desktop (macOS app).
    Desktop {
        #[command(subcommand)]
        command: Option<DesktopCommand>,
    },

    /// Uninstall Veld and clean up.
    Uninstall,
}

#[derive(Args)]
pub struct UpdateArgs {
    /// Wait for this process to exit before installing anything.
    ///
    /// Veld Desktop's own "Update" spawns this and quits: an Electron app
    /// reads from its own bundle while it runs, so the update has to outlive
    /// it. Nothing is installed until the pid is gone.
    #[arg(long, hide = true)]
    pub wait_pid: Option<u32>,

    /// Reopen Veld Desktop when the update is over, however it went.
    ///
    /// Hidden, and only meaningful with `--wait-pid`: on its own it would
    /// *launch* an app the user never had running, which is not what the word
    /// says and not something an update should do.
    #[arg(long, hide = true)]
    pub relaunch: bool,

    /// The running app's executable (`process.execPath`), so the bundle that
    /// gets replaced is the one the user launched rather than whichever copy
    /// the installer would have guessed.
    #[arg(long, hide = true)]
    pub app_path: Option<std::path::PathBuf>,

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
    pub target_version: Option<String>,

    /// Re-run this update in a terminal window instead of here.
    ///
    /// Veld Desktop's handoff sets it. The app quits so its bundle can be
    /// replaced, which leaves the update with nothing to render on and — more
    /// seriously — no controlling terminal, so a privileged install's `sudo`
    /// can only ever try `sudo -n` and fail silently. A window fixes both at
    /// once. Falls back to running right here when no terminal can be opened.
    #[arg(long, hide = true)]
    pub console: bool,

    /// Report whether an update is running, and what it is doing.
    ///
    /// Reads the same lock file every other gate reads, so it is the honest
    /// answer rather than a second opinion. Installs nothing.
    #[arg(long)]
    pub status: bool,

    /// Machine-readable `--status`.
    #[arg(long, requires = "status")]
    pub json: bool,

    /// Start even though another update holds the lock.
    ///
    /// The escape hatch for a holder this one knows is dead but that the
    /// staleness rules have not written off yet — a run abandoned at a
    /// password prompt is only reclaimed after 30 minutes without this.
    #[arg(long)]
    pub force: bool,

    /// Show the install script's own output instead of veld's summary.
    ///
    /// The update normally narrates its own steps and runs the installer
    /// quietly, because the two of them talking at once produced three
    /// "installed successfully!" banners and a first-install footer in the
    /// middle of an update. This is the escape hatch for debugging one: the
    /// raw stream, every URL and checksum, exactly as the script emits it.
    #[arg(long)]
    pub verbose: bool,
}

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
    /// the answer, so `veld self update` skips the app from then on. Works with no app
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

#[derive(Subcommand)]
pub enum SettingsCmd {
    /// List every setting with its effective value.
    List {
        /// Show only settings under this key prefix, e.g. `keepAwake` or
        /// `terminal`. Matches whole segments, so `ui` finds every `ui.*`.
        prefix: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print one setting's effective value and nothing else.
    ///
    /// Bare, so it drops into a shell substitution:
    /// `SHELL=$(veld self settings get terminal.shell)`.
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
    /// clamp is reported; run `veld self settings describe` first to see the range.
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

/// `veld self backup list` and its verbs.
///
/// Every one of these has to work on a machine whose database will not open —
/// that is the only day anybody runs them — so none of them requires a readable
/// database, and the backup directory falls back to the derived default when the
/// `backup.dir` setting cannot be read.
#[derive(Subcommand)]
pub enum BackupCmd {
    /// List the backups in the backup directory, newest first.
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
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
pub enum ProjectsCmd {
    /// List all Veld projects on this machine.
    List {
        /// Include URLs in the output.
        #[arg(long)]
        urls: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(command: SelfCmd) -> i32 {
    match command {
        SelfCmd::Version => {
            commands::version::print_version().await;
            0
        }
        SelfCmd::Update(args) => update(args).await,
        SelfCmd::Doctor { json } => commands::doctor::run(json).await,
        SelfCmd::Setup { command } => commands::setup::run(command).await,
        SelfCmd::Settings { cmd } => match cmd {
            SettingsCmd::List { prefix, json } => commands::settings::list(prefix, json).await,
            SettingsCmd::Get { key, json } => commands::settings::get(key, json).await,
            SettingsCmd::Set { key, value, json } => {
                commands::settings::set(key, value, json).await
            }
            SettingsCmd::Unset { key, json } => commands::settings::unset(key, json).await,
            SettingsCmd::Describe { key, json } => commands::settings::describe(key, json).await,
        },
        SelfCmd::Backup { dir, cmd } => match cmd {
            BackupCmd::List { json } => commands::backup::list(dir, json),
            BackupCmd::Now { json } => commands::backup::now(dir, json),
            BackupCmd::Restore {
                path,
                force,
                yes,
                json,
            } => commands::backup::restore(path, dir, force, yes, json).await,
        },
        SelfCmd::Projects {
            cmd: ProjectsCmd::List { urls, json },
        } => commands::list::run(urls, json).await,
        SelfCmd::Gc => commands::gc::run().await,
        SelfCmd::Desktop { command } => desktop(command).await,
        SelfCmd::Uninstall => commands::uninstall::run().await,
    }
}

pub async fn update(args: UpdateArgs) -> i32 {
    if args.status {
        return commands::update::status(args.json);
    }
    commands::update::run(
        args.wait_pid,
        args.relaunch,
        args.app_path,
        args.target_version,
        args.console,
        args.force,
        args.verbose,
    )
    .await
}

pub async fn desktop(command: Option<DesktopCommand>) -> i32 {
    match command {
        // Bare `veld self desktop` reports rather than installs: a command that
        // puts an app in /Applications should be asked for by name.
        None => commands::desktop::status(false).await,
        Some(DesktopCommand::Status { json }) => commands::desktop::status(json).await,
        Some(DesktopCommand::Install) => commands::desktop::install(None, None, false, None).await,
        Some(DesktopCommand::Uninstall { yes }) => commands::desktop::uninstall(yes).await,
        Some(DesktopCommand::Update {
            version,
            wait_pid,
            relaunch,
            app_path,
        }) => commands::desktop::install(version, wait_pid, relaunch, app_path).await,
    }
}

/// Whether `veld self desktop …` survives an update. Shared with the hidden
/// pre-#423 `veld desktop`, which Veld Desktop itself still calls.
///
/// `status` is read-only (it reads a plist), and blocking it produces a *wrong*
/// answer rather than a blocked one: Veld Desktop resolves the CLI by running
/// `veld desktop status --json`, and its caller catches any failure as "there is
/// no veld CLI here", which silently demotes *Check for Updates…* to the
/// download-from-GitHub route. The install and update arms stay blocked — those
/// move bytes — and so does `uninstall`, which moves none but would delete the
/// very bundle a running update is swapping.
pub fn desktop_survives_an_update(command: &Option<DesktopCommand>) -> bool {
    matches!(command, None | Some(DesktopCommand::Status { .. }))
}

/// This part's half of `crate::command_survives_an_update`.
///
/// - `update` arbitrates itself through the lock; `--status` and `--force` are
///   the two things a user reaches for *because* an update is running.
/// - `doctor` is where a stuck user is sent, and it now reports the update.
/// - `version` must answer during an update — it is how a human or a script
///   checks whether the swap has happened yet.
/// - The **read** half of `settings`: `describe` touches nothing at all (it
///   prints a compile-time catalog), while `list` and `get` open the database
///   read-only. Blocking them produced a *wrong* answer rather than a blocked
///   one — `$(veld self settings get terminal.shell)`, a substitution the README
///   advertises, yields an empty string with the explanation on stderr where a
///   script never looks. `set` and `unset` stay blocked: they write, and the
///   daemon they would write through is being restarted.
/// - The **read** half of `backup`, and only that half. Listing reads a
///   directory and opens each artifact, none of which an update touches, and
///   "what can I go back to" is a question somebody asks precisely when an
///   update has gone wrong. `now` and `restore` stay blocked: one writes a copy
///   of a database that may be mid-migration, and the other would swap the live
///   file out from under the migration itself — which is the one way to turn a
///   stuck update into a lost database.
/// - `desktop status`: see [`desktop_survives_an_update`].
pub fn survives_an_update(command: &SelfCmd) -> bool {
    match command {
        SelfCmd::Update(_) | SelfCmd::Doctor { .. } | SelfCmd::Version => true,
        SelfCmd::Settings { cmd } => matches!(
            cmd,
            SettingsCmd::List { .. } | SettingsCmd::Get { .. } | SettingsCmd::Describe { .. }
        ),
        SelfCmd::Backup { cmd, .. } => matches!(cmd, BackupCmd::List { .. }),
        SelfCmd::Desktop { command } => desktop_survives_an_update(command),
        SelfCmd::Setup { .. } | SelfCmd::Projects { .. } | SelfCmd::Gc | SelfCmd::Uninstall => {
            false
        }
    }
}
