use crate::output;

/// `veld presets [--json]`
pub async fn run(json: bool) -> i32 {
    let Some((_config_path, config)) = super::load_config(json) else {
        return 1;
    };

    let presets = config.presets.as_ref();

    if json {
        let entries: Vec<serde_json::Value> = presets
            .map(|p| {
                p.iter()
                    .map(|(name, selections)| {
                        serde_json::json!({
                            "name": name,
                            "selections": selections,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        println!("{}", serde_json::to_string_pretty(&entries).unwrap());
    } else {
        match presets {
            Some(p) if !p.is_empty() => {
                let mut rows: Vec<Vec<String>> = Vec::new();
                let mut names: Vec<&String> = p.keys().collect();
                names.sort();
                for name in names {
                    rows.push(vec![
                        name.clone(),
                        p[name].join(", "),
                    ]);
                }
                output::print_table(&["PRESET", "SELECTIONS"], &rows);
            }
            _ => {
                output::print_info("No presets defined.");
            }
        }
    }

    0
}
