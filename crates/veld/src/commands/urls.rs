use veld_core::config;
use veld_core::state::ProjectState;

use crate::output;

/// `veld urls [--name <n>] [--json]`
pub async fn run(name: Option<String>, json: bool) -> i32 {
    let Some((config_path, _cfg)) = super::load_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let project_state = match ProjectState::load(&project_root) {
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

    // Collect URLs from node states.
    let mut url_entries: Vec<(&str, &str, &str)> = Vec::new();
    for ns in run_state.nodes.values() {
        if let Some(ref url) = ns.url {
            url_entries.push((&ns.node_name, &ns.variant, url));
        }
    }
    url_entries.sort_by_key(|(node, variant, _)| (*node, *variant));

    if json {
        let payload: Vec<serde_json::Value> = url_entries
            .iter()
            .map(|(node, variant, url)| {
                serde_json::json!({
                    "node": node,
                    "variant": variant,
                    "url": url,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else if url_entries.is_empty() {
        output::print_info("No URLs exposed.");
    } else {
        for (node, variant, url) in &url_entries {
            println!("{} {}", output::cyan(&format!("{node}:{variant}")), url,);
        }
    }

    0
}
