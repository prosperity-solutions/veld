use crate::output;

/// `veld nodes [--json]`
pub async fn run(json: bool) -> i32 {
    let Some((_config_path, config)) = super::load_config(json) else {
        return 1;
    };

    if json {
        // Build structured output.
        let nodes: Vec<serde_json::Value> = config
            .nodes
            .iter()
            .map(|(name, node_cfg)| {
                let variants: Vec<&String> = node_cfg.variants.keys().collect();
                serde_json::json!({
                    "name": name,
                    "variants": variants,
                    "default_variant": node_cfg.default_variant,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&nodes).unwrap());
    } else if config.nodes.is_empty() {
        output::print_info("No nodes defined.");
    } else {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut names: Vec<&String> = config.nodes.keys().collect();
        names.sort();
        for name in names {
            let node_cfg = &config.nodes[name];
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
            ]);
        }
        output::print_table(&["NODE", "VARIANTS", "DEFAULT"], &rows);
    }

    0
}
