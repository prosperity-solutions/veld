use veld_core::config;

use crate::output;

/// `veld urls [--name <n>] [--json]`
pub async fn run(name: Option<String>, json: bool) -> i32 {
    let Some((config_path, _cfg)) = super::parse_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let Some(db) = super::open_db(json) else {
        return 1;
    };
    let project_state = match db.load_project_state(&project_root) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), json);
            return 1;
        }
    };

    let run_name = match super::resolve_run_name(name, &project_state, true, json) {
        Some(n) => n,
        None => return 1,
    };
    let run_name = run_name.as_str();

    let run_state = match project_state.get_run(run_name) {
        Some(r) => r,
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), json);
            return 1;
        }
    };

    // Routes are torn down when a run ends — the last run's URLs are dead.
    // Erroring beats printing 404s an agent would then curl believing the
    // environment is up.
    if !run_state.is_live() {
        if json {
            // Machine-readable shape an agent can branch on without parsing
            // an error string: no URLs, and explicitly not live.
            println!(
                "{}",
                serde_json::json!({
                    "urls": [],
                    // Present and empty, not absent: the live branch always
                    // emits it, and a consumer must not have to probe which
                    // shape it got.
                    "addresses": [],
                    "live": false,
                    "ended_at": run_state.ended_at.map(|t| t.to_rfc3339()),
                })
            );
        } else {
            let ended = run_state
                .ended_at
                .map(|t| {
                    format!(
                        " (last run ended {})",
                        t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
                    )
                })
                .unwrap_or_default();
            output::print_error(
                &format!("Environment '{run_name}' is not running{ended} — no live URLs."),
                false,
            );
        }
        return 1;
    }

    // Collect URLs from node states — every routed http port, not just the
    // primary, or a node's secondary hostnames would be reachable and
    // undiscoverable.
    let mut url_entries: Vec<(&str, &str, Option<&str>, &str)> = Vec::new();
    // And the raw (`tcp`) ports, which have a hostname and no URL. Listed apart
    // from the URLs, never folded in: an address with no scheme pasted into a
    // browser goes nowhere, and a run that exposes only raw ports used to print
    // "No URLs exposed" about ports it had just minted names for.
    let mut raw_entries: Vec<(&str, &str, &str, String)> = Vec::new();
    for ns in run_state.nodes.values() {
        for (port, url) in ns.routed_urls() {
            url_entries.push((&ns.node_name, &ns.variant, port, url));
        }
        for (port, address) in ns.raw_addresses() {
            raw_entries.push((&ns.node_name, &ns.variant, port, address));
        }
    }
    url_entries.sort_by_key(|(node, variant, port, _)| (*node, *variant, *port));
    raw_entries.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));

    if json {
        // Same top-level shape as the stopped branch above — an agent can
        // always read `.live` and `.urls` without probing the type first.
        // (Pre-v3 this was a bare array; the object shape is part of the v3
        // output changes.)
        // `node`, `variant` and `url` keep their meaning for every existing
        // consumer; `port` is the new key and is null for the primary, so a
        // single-port node's object is byte-identical apart from that field.
        let urls: Vec<serde_json::Value> = url_entries
            .iter()
            .map(|(node, variant, port, url)| {
                serde_json::json!({
                    "node": node,
                    "variant": variant,
                    "port": port,
                    "url": url,
                })
            })
            .collect();
        // A separate key, not an entry in `urls` with a null `url`: a consumer
        // that iterates `.urls` must never receive something it would open.
        let addresses: Vec<serde_json::Value> = raw_entries
            .iter()
            .map(|(node, variant, port, address)| {
                serde_json::json!({
                    "node": node,
                    "variant": variant,
                    "port": port,
                    "address": address,
                    "protocol": "tcp",
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "urls": urls,
                "addresses": addresses,
                "live": true,
            }))
            .unwrap()
        );
    } else if url_entries.is_empty() && raw_entries.is_empty() {
        output::print_info("No URLs exposed.");
    } else {
        for (node, variant, port, url) in &url_entries {
            let label = match port {
                Some(port) => format!("{node}:{variant}#{port}"),
                None => format!("{node}:{variant}"),
            };
            println!("{} {}", output::cyan(&label), url);
        }
        for (node, variant, port, address) in &raw_entries {
            let label = format!("{node}:{variant}#{port}");
            println!(
                "{} {} {}",
                output::cyan(&label),
                address,
                output::dim("(tcp)")
            );
        }
    }

    0
}
