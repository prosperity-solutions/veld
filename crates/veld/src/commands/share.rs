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
            if json {
                println!("{}", serde_json::to_string_pretty(&resp).unwrap_or_default());
            } else {
                output::print_success(&format!(
                    "Sharing {} node(s) over peer-to-peer.",
                    resp.nodes.len()
                ));
                println!();
                println!("  Send this ticket to your colleague:");
                println!();
                println!("    {}", output::cyan(&resp.ticket));
                println!();
                println!("  They run:  {}", output::dim("veld join <ticket>"));
                println!(
                    "  Stop with: {}",
                    output::dim(&format!("veld unshare {}", resp.share_id))
                );
                if approve_mode == ApprovalMode::Manual {
                    println!();
                    println!(
                        "  When they join, approve in the browser or run {}.",
                        output::dim("veld approve <id>")
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

/// `veld join <ticket> [--label ...] [--json]`
pub async fn join(ticket: String, label: Option<String>, json: bool) -> i32 {
    let req = JoinRequest { ticket, label };

    match DaemonClient::new().join(&req).await {
        Ok(resp) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&resp).unwrap_or_default());
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
                println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
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

/// `veld unshare <id> [--json]`
pub async fn unshare(id: String, json: bool) -> i32 {
    match DaemonClient::new().unshare(&id).await {
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

/// `veld leave <id> [--json]`
pub async fn leave(id: String, json: bool) -> i32 {
    match DaemonClient::new().leave(&id).await {
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
