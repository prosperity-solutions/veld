//! `veld share` / `join` / `shares` / `unshare` / `leave` — peer-to-peer
//! environment sharing. These are thin clients over the daemon's control API;
//! the daemon holds the iroh endpoint and does the real work.

use crate::output;
use veld_core::config::WebAccessMode;
use veld_core::share::{ApprovalMode, DaemonClient, JoinRequest, StartShareRequest};

/// `veld share [run] [--node ...] [--ttl secs] [--approve MODE] [--web]
/// [--access MODE] [--password PW] [--json]`
#[allow(clippy::too_many_arguments)]
pub async fn share(
    run: Option<String>,
    nodes: Vec<String>,
    ttl: Option<i64>,
    approve: Option<String>,
    web: bool,
    access: Option<String>,
    password: Option<String>,
    json: bool,
) -> i32 {
    // Default: interactive humans approve each join (browser/CLI); agents and
    // scripts (`--json`) auto-approve the first joiner so they don't block.
    // Web shares default to auto — the gateway (which the user just pointed
    // this share at) is the only joiner, so there is nobody else to vet.
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
        None if web => ApprovalMode::Auto,
        None if json => ApprovalMode::First,
        None => ApprovalMode::Manual,
    };

    // --access sets the default for nodes whose config is SILENT on
    // `share.web.access`; an explicit config value always wins (the daemon
    // enforces that — this flag can never weaken configured policy).
    let web_access = match access.as_deref() {
        Some("password") => Some(WebAccessMode::Password),
        Some("link") => Some(WebAccessMode::Link),
        Some(other) => {
            output::print_error(
                &format!("invalid --access '{other}' (expected password|link)"),
                json,
            );
            return 2;
        }
        None => None,
    };

    let req = StartShareRequest {
        run,
        nodes: if nodes.is_empty() { None } else { Some(nodes) },
        ttl_secs: ttl,
        approve: Some(approve_mode),
        web,
        web_access,
        web_password: password,
    };

    match DaemonClient::new().start_share(&req).await {
        Ok(resp) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&resp).unwrap_or_default()
                );
            } else if web {
                output::print_success(&format!(
                    "Sharing {} node(s) on the public web.",
                    resp.nodes.len()
                ));
                for w in &resp.warnings {
                    println!("  {} {}", output::yellow("!"), w);
                }
                println!();
                println!("  Public URL(s):");
                for u in &resp.public_urls {
                    // `access: None` = a pre-access-layer gateway. The daemon
                    // aborts password shares against those (skew guard), so
                    // None can only reach here for link-access behavior —
                    // label it honestly as link-only, never "protected".
                    let mode = match u.access {
                        Some(WebAccessMode::Password) => "password protected",
                        Some(WebAccessMode::Link) | None => "link only — anyone with the URL",
                    };
                    println!(
                        "    {}  {}",
                        output::cyan(&u.public_url),
                        output::dim(&format!("{} ({mode})", u.node))
                    );
                }
                if let Some(pw) = &resp.web_password {
                    println!();
                    println!("  Password:  {}", output::cyan(pw));
                    println!(
                        "  {}",
                        output::dim(
                            "send URL and password separately (two channels) for real secrecy,"
                        )
                    );
                    println!(
                        "  {}",
                        output::dim("or use a one-link that carries the key:")
                    );
                    for u in &resp.public_urls {
                        if u.access != Some(WebAccessMode::Link) {
                            println!(
                                "    {}",
                                output::cyan(&format!(
                                    "{}/#veld-key={}",
                                    u.public_url,
                                    fragment_encode(pw)
                                ))
                            );
                        }
                    }
                }
                println!();
                println!(
                    "  Stop:  {}",
                    output::dim(&format!("veld unshare {}", resp.share_id))
                );
                if resp
                    .public_urls
                    .iter()
                    .any(|u| u.access == Some(WebAccessMode::Link) || u.access.is_none())
                {
                    println!();
                    println!(
                        "  {}",
                        output::dim(
                            "(link-only URLs are the access token — share them only with people who should see this)"
                        )
                    );
                }
            } else {
                output::print_success(&format!(
                    "Sharing {} node(s) over peer-to-peer.",
                    resp.nodes.len()
                ));
                // Yellow `!` (matching the join side), not dim grey — one of
                // these warnings is the DANGER notice that a relay secret is
                // embedded in the join link, and it must not be the quietest
                // text on screen right as the link is shared.
                for w in &resp.warnings {
                    println!("  {} {}", output::yellow("!"), w);
                }
                println!();
                println!("  Send this link (opens in their browser):");
                println!("    {}", output::cyan(&resp.join_url));
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

/// `veld join <ticket> [--label ...] [--no-remember] [--json]`
///
/// `remember` (default true; cleared by `--no-remember`) controls whether a
/// relay token entered at the prompt is cached for next time.
pub async fn join(ticket: String, label: Option<String>, remember: bool, json: bool) -> i32 {
    use std::collections::BTreeMap;

    /// Cap interactive token retries so a persistently-wrong token can't loop.
    const MAX_TOKEN_PROMPTS: usize = 3;

    let client = DaemonClient::new();
    let mut relay_tokens: BTreeMap<String, String> = BTreeMap::new();
    let mut prompts = 0usize;

    loop {
        let req = JoinRequest {
            ticket: ticket.clone(),
            label: label.clone(),
            relay_tokens: relay_tokens.clone(),
            remember,
        };
        let resp = match client.join(&req).await {
            Ok(resp) => resp,
            Err(e) => {
                output::print_error(&e.to_string(), json);
                return 1;
            }
        };

        // The relay is token-gated and the daemon has no valid token yet. In
        // JSON mode we can't prompt — emit the response so a caller can handle
        // it. Interactively, prompt and retry (bounded).
        if let Some(relay_url) = resp.needs_relay_token.clone() {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&resp).unwrap_or_default()
                );
                return 1;
            }
            if prompts >= MAX_TOKEN_PROMPTS {
                output::print_error(
                    &format!("relay {relay_url} rejected the token ({MAX_TOKEN_PROMPTS} attempts)"),
                    false,
                );
                return 1;
            }
            prompts += 1;
            match prompt_relay_token(&relay_url, prompts > 1) {
                Some(token) if !token.is_empty() => {
                    relay_tokens.insert(relay_url, token);
                    continue;
                }
                _ => {
                    output::print_error("no relay token entered", false);
                    return 1;
                }
            }
        }

        // Success.
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
        return 0;
    }
}

/// Prompt on the terminal for a relay's auth token. Input is echoed (no hidden
/// input to avoid a dependency) — the doc note points at `VELD_SHARE_RELAY_TOKEN`
/// for a non-echoing alternative. Returns `None` on read error.
fn prompt_relay_token(relay_url: &str, retry: bool) -> Option<String> {
    use std::io::{BufRead, Write};
    eprintln!();
    if retry {
        eprintln!(
            "  {}",
            output::yellow("That token was rejected. Try again.")
        );
    }
    eprintln!("  Relay {relay_url} requires an authorization token to join.");
    eprint!(
        "  {} ",
        output::dim("Enter token (visible; or set VELD_SHARE_RELAY_TOKEN to avoid this):")
    );
    std::io::stderr().flush().ok()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    Some(line.trim().to_owned())
}

/// Percent-encode a password for the `#veld-key=…` URL fragment (the login
/// page decodes with `decodeURIComponent`). Generated passwords are already
/// fragment-safe; this covers custom ones with `&`, `#`, spaces, etc.
fn fragment_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
                    .map(|s| {
                        // For a web share the public URLs are the useful ones.
                        let urls = if s.public_urls.is_empty() {
                            s.urls.join(" ")
                        } else {
                            s.public_urls
                                .iter()
                                .map(|u| u.public_url.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        };
                        vec![s.id.clone(), s.nodes.join(", "), urls]
                    })
                    .collect();
                output::print_table(&["SHARE", "NODES", "URLS"], &rows);
                for s in &list.shares {
                    if let Some(pw) = &s.web_password {
                        println!(
                            "  {} {}",
                            output::dim(&format!("{} password:", s.id)),
                            output::cyan(pw)
                        );
                    }
                    for c in &s.connections {
                        println!("  {}", connection_line(&s.id, c));
                    }
                }
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
                for j in &list.joins {
                    for c in &j.connections {
                        println!("  {}", connection_line(&j.id, c));
                    }
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

/// One human line describing a tunnel's transport: who is connected and
/// whether traffic is direct or riding a relay. Relayed tunnels get a hint —
/// they are the usual answer to "why is my share slow?" (public relays
/// throttle throughput).
fn connection_line(id: &str, c: &veld_core::share::ShareConnectionInfo) -> String {
    use veld_core::share::ShareTransport;
    let who = if c.label.is_empty() {
        c.node_id.chars().take(10).collect::<String>()
    } else {
        c.label.clone()
    };
    let rtt = c
        .rtt_ms
        .map(|ms| format!(", rtt {ms}ms"))
        .unwrap_or_default();
    match c.transport {
        ShareTransport::Direct => {
            let via = c.via.as_deref().unwrap_or("-");
            output::dim(&format!("{id} {who}: direct ({via}{rtt})"))
        }
        ShareTransport::Relayed => format!(
            "{} {}",
            output::dim(&format!("{id} {who}:")),
            output::yellow(&format!(
                "relayed via {}{rtt} — throughput limited by the relay",
                c.via.as_deref().unwrap_or("unknown relay")
            ))
        ),
        ShareTransport::None => output::dim(&format!("{id} {who}: no open path")),
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

#[cfg(test)]
mod tests {
    use super::connection_line;
    use veld_core::share::{ShareConnectionInfo, ShareTransport};

    fn info(transport: ShareTransport, label: &str) -> ShareConnectionInfo {
        ShareConnectionInfo {
            node_id: "aaaabbbbccccdddd".into(),
            label: label.into(),
            transport,
            via: Some("203.0.113.7:4711".into()),
            rtt_ms: Some(12),
        }
    }

    // Substring assertions survive the ANSI color wrapping (content is inside
    // the escape sequences), so these hold with or without NO_COLOR.
    #[test]
    fn relayed_line_names_the_relay_and_the_cost() {
        let mut c = info(ShareTransport::Relayed, "gateway share.example");
        c.via = Some("https://euw1-1.relay.example./".into());
        let line = connection_line("sh-1", &c);
        assert!(line.contains("gateway share.example"), "{line}");
        assert!(
            line.contains("relayed via https://euw1-1.relay.example./"),
            "{line}"
        );
        assert!(line.contains("rtt 12ms"), "{line}");
        assert!(line.contains("throughput limited by the relay"), "{line}");
    }

    #[test]
    fn direct_line_shows_the_address_without_the_warning() {
        let line = connection_line("sh-1", &info(ShareTransport::Direct, ""));
        // Empty label → shortened node id identifies the peer.
        assert!(
            line.contains("aaaabbbbcc: direct (203.0.113.7:4711, rtt 12ms)"),
            "{line}"
        );
        assert!(!line.contains("throughput limited"), "{line}");
    }

    #[test]
    fn pathless_snapshot_reports_no_open_path() {
        let mut c = info(ShareTransport::None, "host");
        c.via = None;
        c.rtt_ms = None;
        let line = connection_line("sh-1", &c);
        assert!(line.contains("host: no open path"), "{line}");
    }
}
