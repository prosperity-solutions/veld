//! `veld share` / `join` / `shares` / `unshare` / `leave` — peer-to-peer
//! environment sharing. These are thin clients over the daemon's control API;
//! the daemon holds the iroh endpoint and does the real work.

use crate::output;
use veld_core::share::{ApprovalMode, DaemonClient, JoinRequest, StartShareRequest};

/// `veld share [run] [--node ...] [--ttl secs] [--approve MODE] [--json]`
pub async fn share(
    run: Option<String>,
    nodes: Vec<String>,
    ttl: Option<i64>,
    approve: Option<String>,
    json: bool,
) -> i32 {
    // Default: interactive humans approve each join (browser/CLI); agents and
    // scripts (`--json`) auto-approve the first joiner so they don't block.
    let approve_mode = match approve.as_deref() {
        Some("first") => ApprovalMode::First,
        Some("manual") => ApprovalMode::Manual,
        Some("auto") => ApprovalMode::Auto,
        Some(other) => {
            output::print_error(
                &format!("invalid --approve '{other}' (expected first|manual|auto)"),
                json,
            );
            return 2;
        }
        None if json => ApprovalMode::First,
        None => ApprovalMode::Manual,
    };

    let req = StartShareRequest {
        run,
        nodes: if nodes.is_empty() { None } else { Some(nodes) },
        ttl_secs: ttl,
        approve: Some(approve_mode),
    };

    match DaemonClient::new().start_share(&req).await {
        Ok(resp) => {
            let join_url = join_url(&resp.ticket);
            if json {
                let mut v = serde_json::to_value(&resp).unwrap_or_default();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("join_url".to_string(), serde_json::json!(join_url));
                }
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                output::print_success(&format!(
                    "Sharing {} node(s) over peer-to-peer.",
                    resp.nodes.len()
                ));
                println!();
                println!("  Send this link (opens in their browser):");
                println!("    {}", output::cyan(&join_url));
                println!();
                println!(
                    "  …or run:  {}",
                    output::dim(&format!("veld join {}", resp.ticket))
                );
                println!(
                    "  Stop:     {}",
                    output::dim(&format!("veld unshare {}", resp.share_id))
                );
                println!();
                println!(
                    "  {}",
                    output::dim("(the recipient needs veld installed and running)")
                );
                if approve_mode == ApprovalMode::Manual {
                    println!(
                        "  {}",
                        output::dim("when they join, approve in the browser or run `veld approve`")
                    );
                }
            }
            0
        }
        Err(e) => {
            output::print_error(&e.to_string(), json);
            1
        }
    }
}

/// Build the browser join URL for a ticket. The port matches this machine's
/// setup mode; both peers run the same mode, so it's correct on the recipient.
fn join_url(ticket: &str) -> String {
    let base = match super::read_setup_mode().as_deref() {
        Some("unprivileged") => "https://veld.localhost:18443",
        _ => "https://veld.localhost",
    };
    format!("{base}/join#{ticket}")
}

/// `veld join <ticket> [--label ...] [--json]`
pub async fn join(ticket: String, label: Option<String>, json: bool) -> i32 {
    let req = JoinRequest { ticket, label };

    match DaemonClient::new().join(&req).await {
        Ok(resp) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&resp).unwrap_or_default()
                );
            } else {
                output::print_success(&format!(
                    "Joined — {} URL(s) now reachable on this machine:",
                    resp.urls.len()
                ));
                println!();
                for url in &resp.urls {
                    println!("    {}", output::cyan(url));
                }
                for w in &resp.warnings {
                    println!("  {} {}", output::yellow("!"), w);
                }
                println!();
                println!(
                    "  Leave with: {}",
                    output::dim(&format!("veld leave {}", resp.join_id))
                );
            }
            0
        }
        Err(e) => {
            output::print_error(&e.to_string(), json);
            1
        }
    }
}

/// `veld shares [--json]`
pub async fn list(json: bool) -> i32 {
    match DaemonClient::new().list().await {
        Ok(list) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&list).unwrap_or_default()
                );
                return 0;
            }
            if list.shares.is_empty() && list.joins.is_empty() {
                output::print_info("No active shares or joins.");
                return 0;
            }
            if !list.shares.is_empty() {
                println!("{}", output::bold("Hosting:"));
                let rows: Vec<Vec<String>> = list
                    .shares
                    .iter()
                    .map(|s| vec![s.id.clone(), s.nodes.join(", "), s.urls.join(" ")])
                    .collect();
                output::print_table(&["SHARE", "NODES", "URLS"], &rows);
            }
            if !list.joins.is_empty() {
                if !list.shares.is_empty() {
                    println!();
                }
                println!("{}", output::bold("Joined:"));
                let rows: Vec<Vec<String>> = list
                    .joins
                    .iter()
                    .map(|j| vec![j.id.clone(), j.nodes.join(", "), j.urls.join(" ")])
                    .collect();
                output::print_table(&["JOIN", "NODES", "URLS"], &rows);
            }
            0
        }
        Err(e) => {
            output::print_error(&e.to_string(), json);
            1
        }
    }
}

/// Resolve an id argument: use it if given, otherwise pick the sole share/join.
/// `joins = true` resolves against joins, else against hosted shares.
async fn resolve_id(
    client: &DaemonClient,
    id: Option<String>,
    joins: bool,
    json: bool,
) -> Option<String> {
    if let Some(id) = id {
        return Some(id);
    }
    let what = if joins { "join" } else { "share" };
    match client.list().await {
        Ok(list) => {
            let items = if joins { list.joins } else { list.shares };
            match items.len() {
                1 => Some(items[0].id.clone()),
                0 => {
                    output::print_error(&format!("no active {what}s"), json);
                    None
                }
                _ => {
                    output::print_error(
                        &format!("multiple {what}s — specify an id (see `veld shares`)"),
                        json,
                    );
                    None
                }
            }
        }
        Err(e) => {
            output::print_error(&e.to_string(), json);
            None
        }
    }
}

/// `veld unshare [id] [--json]` — id optional when exactly one share is active.
pub async fn unshare(id: Option<String>, json: bool) -> i32 {
    let client = DaemonClient::new();
    let Some(id) = resolve_id(&client, id, false, json).await else {
        return 1;
    };
    match client.unshare(&id).await {
        Ok(()) => {
            if json {
                println!("{}", serde_json::json!({ "stopped": id }));
            } else {
                output::print_success(&format!("Stopped share {id}."));
            }
            0
        }
        Err(e) => {
            output::print_error(&e.to_string(), json);
            1
        }
    }
}

/// `veld approve <req-id> [--json]`
pub async fn approve(id: String, json: bool) -> i32 {
    match DaemonClient::new().approve(&id).await {
        Ok(()) => {
            if json {
                println!("{}", serde_json::json!({ "approved": id }));
            } else {
                output::print_success(&format!("Approved join request {id}."));
            }
            0
        }
        Err(e) => {
            output::print_error(&e.to_string(), json);
            1
        }
    }
}

/// `veld deny <req-id> [--json]`
pub async fn deny(id: String, json: bool) -> i32 {
    match DaemonClient::new().deny(&id).await {
        Ok(()) => {
            if json {
                println!("{}", serde_json::json!({ "denied": id }));
            } else {
                output::print_success(&format!("Denied join request {id}."));
            }
            0
        }
        Err(e) => {
            output::print_error(&e.to_string(), json);
            1
        }
    }
}

/// `veld leave [id] [--json]` — id optional when exactly one join is active.
pub async fn leave(id: Option<String>, json: bool) -> i32 {
    let client = DaemonClient::new();
    let Some(id) = resolve_id(&client, id, true, json).await else {
        return 1;
    };
    match client.leave(&id).await {
        Ok(()) => {
            if json {
                println!("{}", serde_json::json!({ "left": id }));
            } else {
                output::print_success(&format!("Left share {id}."));
            }
            0
        }
        Err(e) => {
            output::print_error(&e.to_string(), json);
            1
        }
    }
}
