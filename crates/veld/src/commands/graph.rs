use veld_core::graph;

use crate::output;

/// `veld graph [node:variant...]`
pub async fn run(selections: Vec<String>) -> i32 {
    let Some((_config_path, config)) = super::load_config(false) else {
        return 1;
    };

    if selections.is_empty() {
        output::print_error(
            "Provide at least one node:variant selection to graph.",
            false,
        );
        return 1;
    }

    // Parse and resolve selections.
    let parsed: Vec<graph::NodeSelection> = match selections
        .iter()
        .map(|s| graph::parse_selection(s))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(p) => p,
        Err(e) => {
            output::print_error(&format!("{e}"), false);
            return 1;
        }
    };

    let resolved = match graph::resolve_selections(&parsed, &config) {
        Ok(r) => r,
        Err(e) => {
            output::print_error(&format!("{e}"), false);
            return 1;
        }
    };

    let plan = match graph::build_execution_plan(&resolved, &config) {
        Ok(p) => p,
        Err(e) => {
            output::print_error(&format!("Failed to resolve graph: {e}"), false);
            return 1;
        }
    };

    // Print ASCII dependency graph as stages.
    println!("{}", output::bold("Dependency graph (execution stages):"));
    println!();

    for (stage_idx, stage) in plan.iter().enumerate() {
        let stage_label = format!("Stage {}", stage_idx + 1);
        let is_last_stage = stage_idx == plan.len() - 1;
        let connector = if is_last_stage {
            "\u{2514}\u{2500}\u{2500}"
        } else {
            "\u{251c}\u{2500}\u{2500}"
        };

        let node_labels: Vec<String> = stage
            .iter()
            .map(|sel| output::cyan(&sel.to_string()))
            .collect();

        println!(
            "  {connector} {} [{}]",
            output::bold(&stage_label),
            node_labels.join(", "),
        );
    }

    0
}
