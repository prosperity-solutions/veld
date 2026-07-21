use veld_core::config;

use crate::output;

/// `veld urls [--name <n>] [--json]`
pub async fn run(name: Option<String>, json: bool) -> i32 {
    let Some((config_path, _cfg)) = super::load_config(json) else {
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

    // Collect URLs from node states.
    let mut url_entries: Vec<(&str, &str, &str)> = Vec::new();
    for ns in run_state.nodes.values() {
        if let Some(ref url) = ns.url {
            url_entries.push((&ns.node_name, &ns.variant, url));
        }
    }
    url_entries.sort_by_key(|(node, variant, _)| (*node, *variant));

    if json {
        // Same top-level shape as the stopped branch above — an agent can
        // always read `.live` and `.urls` without probing the type first.
        // (Pre-v3 this was a bare array; the object shape is part of the v3
        // output changes.)
        let urls: Vec<serde_json::Value> = url_entries
            .iter()
            .map(|(node, variant, url)| {
                serde_json::json!({
                    "node": node,
                    "variant": variant,
                    "url": url,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "urls": urls,
                "live": true,
            }))
            .unwrap()
        );
    } else if url_entries.is_empty() {
        output::print_info("No URLs exposed.");
    } else {
        for (node, variant, url) in &url_entries {
            println!("{} {}", output::cyan(&format!("{node}:{variant}")), url,);
        }
    }

    0
}
