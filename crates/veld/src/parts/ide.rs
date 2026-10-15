//! `veld ide` — the Veld window from a terminal: open things in it, hand work
//! to the human as a worktree, and the `$BROWSER` stand-in.

use clap::{Args, Subcommand};

use crate::commands;

#[derive(Subcommand)]
pub enum IdeCmd {
    #[command(flatten)]
    Window(commands::ide::IdeCommand),

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
    OpenUrl(OpenUrlArgs),

    /// Open the management dashboard in the browser.
    Dashboard,
}

#[derive(Args)]
pub struct OpenUrlArgs {
    /// Which system tool this is standing in for, so a passthrough reaches the
    /// right one. Set by the generated shims; you do not need it.
    #[arg(long, hide = true)]
    pub tool: Option<String>,

    /// Terminal session to open the page beside. Defaults to
    /// `$VELD_PTY_SESSION`, which a Veld terminal sets.
    #[arg(long)]
    pub session: Option<String>,

    /// The URL — or, for a shim, whatever the real tool was called with.
    #[arg(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub args: Vec<String>,
}

pub async fn run(command: IdeCmd) -> i32 {
    match command {
        IdeCmd::Window(command) => commands::ide::run(command).await,
        IdeCmd::OpenUrl(args) => open_url(args).await,
        IdeCmd::Dashboard => commands::ui::run().await,
    }
}

pub async fn open_url(args: OpenUrlArgs) -> i32 {
    commands::open_url::run(args.tool, args.session, args.args).await
}

/// This part's half of `crate::command_survives_an_update`, exhaustive so a new
/// verb is a compile error until somebody decides.
///
/// Every verb is blocked: each reaches the daemon (a pane, a worktree, the
/// dashboard it serves) that an update is restarting.
pub fn survives_an_update(command: &IdeCmd) -> bool {
    match command {
        IdeCmd::Window(_) | IdeCmd::OpenUrl(_) | IdeCmd::Dashboard => false,
    }
}
