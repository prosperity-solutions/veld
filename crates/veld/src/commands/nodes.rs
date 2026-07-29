use crate::output;

/// `veld nodes [--json]`
pub async fn run(json: bool) -> i32 {
    let Some((config_path, _)) = super::parse_config(json) else {
        return 1;
    };
    // Re-load with provenance: under `include` globs, "which file is this node in"
    // is the first question anyone asks, and a name alone cannot answer it.
    let loaded = match veld_core::config::parse_config_with_files(&config_path) {
        Ok(l) => l,
        Err(e) => {
            output::print_error(&format!("Failed to load config: {e}"), json);
            return 1;
        }
    };
    let config = &loaded.config;
    // node name -> "file:line"
    let defined_in: std::collections::HashMap<&str, String> = loaded
        .files
        .iter()
        .flat_map(|f| {
            f.nodes.iter().map(move |(name, line)| {
                (
                    name.as_str(),
                    if *line > 0 {
                        format!("{}:{line}", f.relative.display())
                    } else {
                        f.relative.display().to_string()
                    },
                )
            })
        })
        .collect();

    // Filter out hidden nodes.
    let visible_nodes: Vec<(&String, &veld_core::config::NodeConfig)> = config
        .nodes
        .iter()
        .filter(|(_, node_cfg)| !node_cfg.hidden.unwrap_or(false))
        .collect();

    if json {
        // Build structured output.
        let nodes: Vec<serde_json::Value> = visible_nodes
            .iter()
            .map(|(name, node_cfg)| {
                let variants: Vec<&String> = node_cfg.variants.keys().collect();
                serde_json::json!({
                    "name": name,
                    "variants": variants,
                    "default_variant": node_cfg.default_variant,
                    "defined_in": defined_in.get(name.as_str()),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&nodes).unwrap());
    } else if visible_nodes.is_empty() {
        if config.nodes.is_empty() {
            output::print_info("No nodes defined.");
        } else {
            output::print_info("All nodes are hidden.");
        }
    } else {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut sorted: Vec<(&String, &veld_core::config::NodeConfig)> = visible_nodes;
        sorted.sort_by_key(|(name, _)| name.to_owned());
        for (name, node_cfg) in sorted {
            let mut variants: Vec<&String> = node_cfg.variants.keys().collect();
            variants.sort();
            rows.push(vec![
                name.clone(),
                variants
                    .iter()
                    .map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                node_cfg.default_variant.clone().unwrap_or_default(),
                defined_in.get(name.as_str()).cloned().unwrap_or_default(),
            ]);
        }
        output::print_table(&["NODE", "VARIANTS", "DEFAULT", "DEFINED IN"], &rows);
    }

    0
}
