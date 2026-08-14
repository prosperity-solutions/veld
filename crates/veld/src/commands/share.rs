//! `veld share` / `join` / `shares` / `unshare` / `leave` — peer-to-peer
//! environment sharing. These are thin clients over the daemon's control API;
//! the daemon holds the iroh endpoint and does the real work.

use crate::output;
use veld_core::config::WebAccessMode;
use veld_core::share::{ApprovalMode, DaemonClient, JoinRequest, StartShareRequest};

/// `"3h 12m"` / `"12m"` / `"under a minute"` — a countdown as a human reads it.
///
/// Mirrors the dashboard's `formatRemaining`. Duplicated rather than shared
/// because the two live in different languages for different surfaces, and the
/// shape is small enough that a crate boundary would cost more than it saves —
/// but keep them saying the same thing.
fn humanize(seconds: i64) -> String {
    if seconds < 60 {
        return "under a minute".to_owned();
    }
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    match (hours, minutes) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// `"in 2h (14:32 local)"` — the share's own lifetime, as the receipt states it.
///
/// Both halves on purpose: the duration answers "did the `--ttl` I passed, or the
/// `veld.json` my team committed, actually take" — which nothing in the default
/// output used to answer, since two of the three sources clamp silently — and the
/// wall-clock time answers "will this still work after lunch" without arithmetic.
/// Local time, like the log timestamps: this is read by the person at the machine.
fn expires_note(expires_at: i64) -> String {
    let secs_left = expires_at - chrono::Utc::now().timestamp();
    match chrono::DateTime::from_timestamp(expires_at, 0) {
        Some(at) => {
            let local = at.with_timezone(&chrono::Local).format("%H:%M");
            // A share already past its expiry is not a thing a caller can produce
            // any more (the mint floors every branch above zero), but a clock step
            // between mint and this line can still get here — and "in -3m" reads
            // as a bug in veld rather than a clock that moved.
            if secs_left <= 0 {
                format!("now ({local} local)")
            } else {
                format!("in {} ({local} local)", humanize(secs_left))
            }
        }
        // `saturating_add` at the mint site can produce a timestamp `chrono`
        // refuses; `--ttl 9223372036854775807` is the way there.
        None => "not for a very long time".to_owned(),
    }
}

/// One line saying whether this machine will still be up to serve the link that
/// was just printed.
///
/// The share is only half the answer: a laptop that suspends drops it, and the
/// automatic keep-awake that prevents that is a *setting* — so the person who
/// just ran `veld share` is exactly the person who needs to know which way it is
/// set, and this is the only surface that reaches them. There is still no
/// keep-awake subcommand; this reports state, it does not change any.
///
/// **Polled, because arming is asynchronous.** The daemon reconciles the hold on
/// a detached task after the share is inserted — deliberately, so a share start
/// never waits on a process spawn and a privileged-helper round trip. So the
/// first read usually lands before the hold exists. A second is bounded hard:
/// this is a receipt line, and a `veld share` made slower to print it would be a
/// bad trade every time.
async fn keep_awake_line(client: &DaemonClient) -> Option<String> {
    for attempt in 0..5 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        // A daemon that cannot answer is not worth a line, and not worth a
        // retry either — `veld share` has already succeeded by this point.
        let state = client.caffeinate().await.ok()?;
        if !state.supported {
            // Nothing can keep this machine awake and nothing the reader could
            // do about it. Silence beats a caveat with no action in it.
            return None;
        }
        let armed = state.active && (state.reason == "sharing" || state.reason == "both");
        if armed {
            let until = match state.remaining_secs {
                // Naming *which* deadline this is, when it is the share's own —
                // this receipt is the surface that reaches the person who just
                // shared, so "stays awake for 1h 59m" under a 4-hour setting is
                // read here first and misread here first.
                //
                // `"sharing"` only, never `"both"`: `remaining_secs` is the later
                // of the two deadlines, so under a manual hold the number is not
                // the share's and attributing it would be the same false claim in
                // a new place.
                Some(secs) if state.reason == "sharing" && state.sharing_bound_by_share => {
                    format!("for {} — until your sharing expires", humanize(secs))
                }
                Some(secs) => format!("for {}", humanize(secs)),
                None => "until you turn it off".to_owned(),
            };
            let lid = if state.covers_lid {
                ""
            } else {
                ", unless you shut the lid"
            };
            return Some(format!("this machine stays awake {until}{lid}"));
        }
        // A hold somebody asked for themselves already covers the share, and
        // saying "off" under it would be wrong.
        if state.active {
            return None;
        }
    }
    // Budget spent without the hold appearing. That is *usually* the switch
    // being off, but it is also what a loaded machine looks like while the
    // detached reconcile is still opening a database and spawning `pmset` — so
    // the line stops short of the definite claim it used to make.
    Some("this machine may sleep and drop the share".to_owned())
}

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

    // Project scope: a run name is unique per project, not globally, so tell the
    // daemon which project this invocation means.
    //
    // Keyed on the config's EXISTENCE, not on parsing it — `veld share` never
    // reads the config, and `load_config_from_cwd().ok()` would silently drop
    // the scope for a veld.json that fails to parse or carries an unsupported
    // schema_version. Dropping it re-enables machine-wide name resolution on the
    // one path where a wrong answer publishes URLs. Still `None` outside any
    // project, which is a real case: `veld share` works from anywhere.
    let project_root = std::env::current_dir()
        .ok()
        .and_then(|cwd| veld_core::config::discover_config(&cwd).ok())
        .map(|config_path| {
            veld_core::config::project_root(&config_path)
                .display()
                .to_string()
        });

    let req = StartShareRequest {
        run,
        project_root,
        nodes: if nodes.is_empty() { None } else { Some(nodes) },
        ttl_secs: ttl,
        approve: Some(approve_mode),
        web,
        web_access,
        web_password: password,
    };

    let client = DaemonClient::new();
    match client.start_share(&req).await {
        Ok(resp) => {
            if json {
                // The same fact the human receipt prints as `Awake:`, as a field
                // — an agent driving `veld share` has exactly the same reason to
                // know whether this machine will still be up to serve the link,
                // and the docs promise the line without qualifying it by output
                // mode. `null` when there is nothing to say.
                let mut out = serde_json::to_value(&resp).unwrap_or_default();
                if let Some(obj) = out.as_object_mut() {
                    obj.insert(
                        "keep_awake".to_owned(),
                        match keep_awake_line(&client).await {
                            Some(note) => serde_json::Value::String(note),
                            None => serde_json::Value::Null,
                        },
                    );
                }
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
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
                    "  Stop:    {}",
                    output::dim(&format!("veld unshare {}", resp.share_id))
                );
                // The lifetime actually in force, which until now only `--json`
                // carried. Three things can decide it (`--ttl`, the project's
                // veld.json, this machine's setting) and two of them silently
                // clamp, so "did my number take?" had no answer in the default
                // output — and the `Awake:` line below is not it: that one only
                // appears when keep-awake is on *and* the share is the binding
                // deadline.
                println!("  Expires: {}", output::dim(&expires_note(resp.expires_at)));
                if let Some(note) = keep_awake_line(&client).await {
                    println!("  Awake:   {}", output::dim(&note));
                }
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
                println!(
                    "  Expires:  {}",
                    output::dim(&expires_note(resp.expires_at))
                );
                if let Some(note) = keep_awake_line(&client).await {
                    println!("  Awake:    {}", output::dim(&note));
                }
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
            let total = resp.urls.len() + resp.addresses.len();
            output::print_success(&format!(
                "Joined — {total} endpoint(s) now reachable on this machine:"
            ));
            println!();
            for url in &resp.urls {
                println!("    {}", output::cyan(url));
            }
            // Raw endpoints are addresses, not URLs, and the port is this
            // machine's listener rather than the origin's — printing them as
            // URLs would send someone to a port nothing is listening on.
            for addr in &resp.addresses {
                println!("    {}  {}", output::cyan(addr), output::dim("(tcp)"));
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
                        // Raw ports have no scheme and no route, so they go in
                        // the same column but marked — never as a bare token a
                        // reader would paste into a browser.
                        let urls = if s.addresses.is_empty() {
                            urls
                        } else {
                            let raw = s
                                .addresses
                                .iter()
                                .map(|a| format!("{a} (tcp)"))
                                .collect::<Vec<_>>()
                                .join(" ");
                            if urls.is_empty() {
                                raw
                            } else {
                                format!("{urls} {raw}")
                            }
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
                    .map(|j| {
                        let mut reachable = j.urls.clone();
                        // The joiner's local listener address, not the origin's.
                        reachable.extend(j.addresses.iter().map(|a| format!("{a} (tcp)")));
                        vec![j.id.clone(), j.nodes.join(", "), reachable.join(" ")]
                    })
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

    #[test]
    fn the_expiry_receipt_states_a_duration_and_a_wall_clock() {
        // The line exists to answer "did my --ttl take", so the duration is the
        // load-bearing half; the clock time is what saves the reader arithmetic.
        let note = super::expires_note(chrono::Utc::now().timestamp() + 2 * 60 * 60);
        assert!(
            note.starts_with("in 1h 59m") || note.starts_with("in 2h"),
            "{note}"
        );
        assert!(note.contains("local"), "{note}");
    }

    #[test]
    fn an_absurd_expiry_does_not_render_as_a_negative_countdown() {
        // `--ttl i64::MAX` saturates at the mint site, which produces a timestamp
        // `chrono` will not represent. Neither half of this may render as a bug in
        // veld: no panic, and no "in -3m".
        assert_eq!(
            super::expires_note(i64::MAX),
            "not for a very long time",
            "a saturated expiry must not fall through to arithmetic"
        );
        let past = super::expires_note(chrono::Utc::now().timestamp() - 180);
        assert!(past.starts_with("now ("), "{past}");
    }
}
