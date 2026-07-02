//! `veld share` / `join` / `shares` / `unshare` / `leave` — peer-to-peer
//! environment sharing. These are thin clients over the daemon's control API;
//! the daemon holds the iroh endpoint and does the real work.

use crate::output;
use veld_core::share::{DaemonClient, JoinRequest, StartShareRequest};

/// `veld share [run] [--node ...] [--ttl secs] [--json]`
pub async fn share(run: Option<String>, nodes: Vec<String>, ttl: Option<i64>, json: bool) -> i32 {
    let req = StartShareRequest {
        run,
        nodes: if nodes.is_empty() { None } else { Some(nodes) },
        ttl_secs: ttl,
        approve: None,
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
