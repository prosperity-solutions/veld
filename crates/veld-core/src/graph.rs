use std::collections::{HashMap, HashSet, VecDeque};

use thiserror::Error;

use crate::config::VeldConfig;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A fully-qualified node+variant identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeSelection {
    pub node: String,
    pub variant: String,
}

impl std::fmt::Display for NodeSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.node, self.variant)
    }
}

/// A stage of nodes that can execute in parallel.
pub type Stage = Vec<NodeSelection>;

/// Ordered execution plan: each inner `Vec` is a parallel stage.
pub type ExecutionPlan = Vec<Stage>;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum GraphError {
    #[error("unknown node \"{0}\"")]
    UnknownNode(String),

    #[error("node \"{node}\" has no variant \"{variant}\"")]
    UnknownVariant { node: String, variant: String },

    #[error("dependency cycle detected: {0}")]
    CycleDetected(String),

    #[error("ambiguous variable reference \"{reference}\" — node \"{node}\" has multiple active variants ({variants:?}); use the qualified form ${{nodes.{node}:{hint}.{field}}}")]
    AmbiguousReference {
        reference: String,
        node: String,
        variants: Vec<String>,
        hint: String,
        field: String,
    },

    #[error("unknown preset \"{0}\"")]
    UnknownPreset(String),
}

// ---------------------------------------------------------------------------
// Parsing selection strings
// ---------------------------------------------------------------------------

/// Parse a `"node:variant"` selection string.
pub fn parse_selection(s: &str) -> Result<NodeSelection, GraphError> {
    if let Some((node, variant)) = s.split_once(':') {
        Ok(NodeSelection {
            node: node.to_owned(),
            variant: variant.to_owned(),
        })
    } else {
        // Bare node name — caller must resolve default variant.
        Ok(NodeSelection {
            node: s.to_owned(),
            variant: String::new(),
        })
    }
}

/// Resolve default variants for bare selections and validate against config.
pub fn resolve_selections(
    selections: &[NodeSelection],
    config: &VeldConfig,
) -> Result<Vec<NodeSelection>, GraphError> {
    selections
        .iter()
        .map(|sel| {
            let node_cfg = config
                .nodes
                .get(&sel.node)
                .ok_or_else(|| GraphError::UnknownNode(sel.node.clone()))?;

            let variant = if sel.variant.is_empty() {
                node_cfg
                    .default_variant
                    .clone()
                    .ok_or_else(|| GraphError::UnknownVariant {
                        node: sel.node.clone(),
                        variant: "(none — no default_variant set)".into(),
                    })?
            } else {
                sel.variant.clone()
            };

            if !node_cfg.variants.contains_key(&variant) {
                return Err(GraphError::UnknownVariant {
                    node: sel.node.clone(),
                    variant,
                });
            }

            Ok(NodeSelection {
                node: sel.node.clone(),
                variant,
            })
        })
        .collect()
}

/// Expand a preset name into its selections.
pub fn expand_preset(
    preset_name: &str,
    config: &VeldConfig,
) -> Result<Vec<NodeSelection>, GraphError> {
    let presets = config.presets.as_ref().ok_or_else(|| {
        GraphError::UnknownPreset(preset_name.to_owned())
    })?;
    let items = presets
        .get(preset_name)
        .ok_or_else(|| GraphError::UnknownPreset(preset_name.to_owned()))?;
    items.iter().map(|s| parse_selection(s)).collect()
}

// ---------------------------------------------------------------------------
// Graph building + topological sort
// ---------------------------------------------------------------------------

/// Build the complete dependency graph from end-node selections and return
/// an ordered execution plan (stages of parallel nodes).
pub fn build_execution_plan(
    endpoints: &[NodeSelection],
    config: &VeldConfig,
) -> Result<ExecutionPlan, GraphError> {
    // 1. Walk dependencies to collect all required nodes.
    let all_nodes = collect_all_nodes(endpoints, config)?;

    // 2. Build adjacency list (node -> set of nodes it depends on).
    let mut deps: HashMap<NodeSelection, HashSet<NodeSelection>> = HashMap::new();
    for sel in &all_nodes {
        let variant_cfg = &config.nodes[&sel.node].variants[&sel.variant];
        let mut dep_set = HashSet::new();
        if let Some(dep_map) = &variant_cfg.depends_on {
            for (dep_node, dep_variant) in dep_map {
                dep_set.insert(NodeSelection {
                    node: dep_node.clone(),
                    variant: dep_variant.clone(),
                });
            }
        }
        deps.insert(sel.clone(), dep_set);
    }

    // 3. Validate variable references for ambiguity.
    validate_variable_references(&all_nodes, config)?;

    // 4. Kahn's algorithm for topological sort into stages.
    topological_stages(&all_nodes, &deps)
}

/// Recursively collect every node required by the endpoint selections.
fn collect_all_nodes(
    endpoints: &[NodeSelection],
    config: &VeldConfig,
) -> Result<Vec<NodeSelection>, GraphError> {
    let mut visited: HashSet<NodeSelection> = HashSet::new();
    let mut queue: VecDeque<NodeSelection> = VecDeque::new();

    for ep in endpoints {
        if visited.insert(ep.clone()) {
            queue.push_back(ep.clone());
        }
    }

    while let Some(sel) = queue.pop_front() {
        let node_cfg = config
            .nodes
            .get(&sel.node)
            .ok_or_else(|| GraphError::UnknownNode(sel.node.clone()))?;
        let variant_cfg =
            node_cfg
                .variants
                .get(&sel.variant)
                .ok_or_else(|| GraphError::UnknownVariant {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                })?;

        if let Some(dep_map) = &variant_cfg.depends_on {
            for (dep_node, dep_variant) in dep_map {
                // Validate the dependency target exists.
                let dep_node_cfg = config
                    .nodes
                    .get(dep_node)
                    .ok_or_else(|| GraphError::UnknownNode(dep_node.clone()))?;
                if !dep_node_cfg.variants.contains_key(dep_variant) {
                    return Err(GraphError::UnknownVariant {
                        node: dep_node.clone(),
                        variant: dep_variant.clone(),
                    });
                }

                let dep_sel = NodeSelection {
                    node: dep_node.clone(),
                    variant: dep_variant.clone(),
                };
                if visited.insert(dep_sel.clone()) {
                    queue.push_back(dep_sel);
                }
            }
        }
    }

    Ok(visited.into_iter().collect())
}

/// Kahn's algorithm producing parallel stages. Detects cycles.
fn topological_stages(
    nodes: &[NodeSelection],
    deps: &HashMap<NodeSelection, HashSet<NodeSelection>>,
) -> Result<ExecutionPlan, GraphError> {
    // In-degree map: in_deg[n] = number of unresolved deps of n.
    let mut in_deg: HashMap<&NodeSelection, usize> = HashMap::new();
    for n in nodes {
        let count = deps
            .get(n)
            .map(|d| d.iter().filter(|dep| deps.contains_key(dep)).count())
            .unwrap_or(0);
        in_deg.insert(n, count);
    }

    let mut plan: ExecutionPlan = Vec::new();
    let mut remaining: HashSet<&NodeSelection> = nodes.iter().collect();

    loop {
        let stage: Vec<&NodeSelection> = remaining
            .iter()
            .filter(|n| in_deg.get(*n).copied().unwrap_or(0) == 0)
            .copied()
            .collect();

        if stage.is_empty() {
            if remaining.is_empty() {
                break;
            }
            // Cycle detected — report the remaining nodes.
            let cycle_nodes: Vec<String> = remaining.iter().map(|n| n.to_string()).collect();
            return Err(GraphError::CycleDetected(cycle_nodes.join(", ")));
        }

        for resolved in &stage {
            remaining.remove(resolved);
            // Decrement in-degree for nodes that depended on `resolved`.
            for n in remaining.iter() {
                if let Some(d) = deps.get(*n) {
                    if d.contains(resolved) {
                        if let Some(count) = in_deg.get_mut(*n) {
                            *count = count.saturating_sub(1);
                        }
                    }
                }
            }
        }

        plan.push(stage.into_iter().cloned().collect());
    }

    Ok(plan)
}

// ---------------------------------------------------------------------------
// Variable reference ambiguity validation
// ---------------------------------------------------------------------------

/// Check that no unqualified `${nodes.X.field}` references are ambiguous
/// (i.e., node X has multiple active variants in the graph).
fn validate_variable_references(
    all_nodes: &[NodeSelection],
    config: &VeldConfig,
) -> Result<(), GraphError> {
    // Build a map: node_name -> list of active variants.
    let mut active_variants: HashMap<&str, Vec<&str>> = HashMap::new();
    for sel in all_nodes {
        active_variants
            .entry(&sel.node)
            .or_default()
            .push(&sel.variant);
    }

    // For each node, scan its env and command strings for unqualified refs.
    for sel in all_nodes {
        let variant_cfg = &config.nodes[&sel.node].variants[&sel.variant];

        let mut strings_to_check: Vec<&str> = Vec::new();
        if let Some(cmd) = &variant_cfg.command {
            strings_to_check.push(cmd);
        }
        if let Some(env_map) = &variant_cfg.env {
            for v in env_map.values() {
                strings_to_check.push(v);
            }
        }

        for s in strings_to_check {
            check_string_for_ambiguous_refs(s, &active_variants)?;
        }
    }

    Ok(())
}

fn check_string_for_ambiguous_refs(
    s: &str,
    active_variants: &HashMap<&str, Vec<&str>>,
) -> Result<(), GraphError> {
    // Match ${nodes.NAME.FIELD} (unqualified — no colon).
    let mut rest = s;
    while let Some(start) = rest.find("${nodes.") {
        let after = &rest[start + 8..];
        if let Some(end) = after.find('}') {
            let inner = &after[..end];
            // Check if it's unqualified (no ':' before the dot).
            if let Some(dot_pos) = inner.find('.') {
                let node_part = &inner[..dot_pos];
                let field_part = &inner[dot_pos + 1..];
                // Unqualified if node_part contains no ':'.
                if !node_part.contains(':') {
                    if let Some(variants) = active_variants.get(node_part) {
                        if variants.len() > 1 {
                            return Err(GraphError::AmbiguousReference {
                                reference: format!("${{nodes.{inner}}}"),
                                node: node_part.to_owned(),
                                variants: variants.iter().map(|v| (*v).to_owned()).collect(),
                                hint: variants[0].to_owned(),
                                field: field_part.to_owned(),
                            });
                        }
                    }
                }
            }
            rest = &after[end..];
        } else {
            break;
        }
    }
    Ok(())
}
