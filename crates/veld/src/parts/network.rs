//! `veld network` — shares and joins: one machine's runs reachable from another.

use clap::Subcommand;

use crate::commands;

#[derive(Subcommand)]
pub enum NetworkCmd {
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
        /// (`veld self settings list`, 240/120 minutes out of the box), or the project's
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
    List {
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Stop hosting a share.
    Unshare {
        /// Share id (from `veld network list`); optional when exactly one is active.
        id: Option<String>,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Leave a joined share.
    Leave {
        /// Join id (from `veld network list`); optional when exactly one is active.
        id: Option<String>,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Approve a pending join request (see `veld network list`).
    Approve {
        /// Request id (from `veld network list` or the approval prompt).
        id: String,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },

    /// Deny a pending join request.
    Deny {
        /// Request id (from `veld network list`).
        id: String,
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(command: NetworkCmd) -> i32 {
    match command {
        NetworkCmd::Share {
            run,
            node,
            ttl,
            approve,
            web,
            access,
            password,
            json,
        } => commands::share::share(run, node, ttl, approve, web, access, password, json).await,
        NetworkCmd::Join {
            ticket,
            label,
            no_remember,
            json,
        } => commands::share::join(ticket, label, !no_remember, json).await,
        NetworkCmd::List { json } => commands::share::list(json).await,
        NetworkCmd::Unshare { id, json } => commands::share::unshare(id, json).await,
        NetworkCmd::Leave { id, json } => commands::share::leave(id, json).await,
        NetworkCmd::Approve { id, json } => commands::share::approve(id, json).await,
        NetworkCmd::Deny { id, json } => commands::share::deny(id, json).await,
    }
}

/// This part's half of `crate::command_survives_an_update`, exhaustive so a new
/// verb is a compile error until somebody decides.
///
/// Every verb is blocked: shares and joins live in the daemon an update is
/// restarting.
pub fn survives_an_update(command: &NetworkCmd) -> bool {
    match command {
        NetworkCmd::Share { .. }
        | NetworkCmd::Join { .. }
        | NetworkCmd::List { .. }
        | NetworkCmd::Unshare { .. }
        | NetworkCmd::Leave { .. }
        | NetworkCmd::Approve { .. }
        | NetworkCmd::Deny { .. } => false,
    }
}
