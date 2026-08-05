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
    /// Under `include` globs, "unknown node" has four distinct causes: never
    /// defined, defined but not matched by a glob, its file renamed out of a glob,
    /// or its file present but unparseable. A bare name cannot tell them apart, so
    /// the error carries what the reader needs to: the nodes that *do* exist, and
    /// a pointer at `veld config --files` for the glob→file→node chain.
    #[error(
        "unknown node \"{name}\"{}{}",
        if known.is_empty() {
            "\n  No nodes are defined at all.".to_owned()
        } else {
            format!("\n  Defined nodes: {}", known.join(", "))
        },
        if *split {
            "\n  This project's config is split across files — run \
             `veld config --files` to see which glob matched which file, and which \
             nodes each defines. A node can go missing because its file was renamed \
             out of an `include` glob."
                .to_owned()
        } else {
            String::new()
        }
    )]
    UnknownNode {
        name: String,
        /// Every defined node name, sorted.
        known: Vec<String>,
        /// Whether the config uses `include`, which changes what the likely cause is.
        split: bool,
    },

    #[error("node \"{node}\" has no variant \"{variant}\"")]
    UnknownVariant { node: String, variant: String },

    #[error("dependency cycle detected: {0}")]
    CycleDetected(String),

    #[error(
        "ambiguous variable reference \"{reference}\" — node \"{node}\" has multiple active variants ({variants:?}); use the qualified form ${{nodes.{node}:{hint}.{field}}}"
    )]
    AmbiguousReference {
        reference: String,
        node: String,
        variants: Vec<String>,
        hint: String,
        field: String,
    },

    #[error("unknown preset \"{0}\"")]
    UnknownPreset(String),

    #[error(
        "preset reference cycle: {0}. A preset may reference another with `@name`, but not \
         itself, directly or transitively"
    )]
    PresetCycle(String),

    /// Distinct from [`GraphError::PresetCycle`]: every path here is acyclic, the
    /// *total* is what blew the budget. A preset referenced from several places is
    /// expanded once per reference, so a legal tree can still cost 2^depth.
    #[error(
        "preset \"{name}\" cannot be expanded: it nests `@preset` references more than \
         {depth} levels deep, or takes more than {steps} expansion steps. Both limits are far \
         above any hand-written preset — flatten the tree, or split it into presets a reader \
         can follow",
        depth = PRESET_DEPTH_LIMIT,
        steps = PRESET_STEP_LIMIT
    )]
    PresetTooLarge { name: String },

    #[error(
        "node \"{node}:{variant}\" has sensitive_outputs {undeclared:?} not declared in outputs"
    )]
    UndeclaredSensitiveOutputs {
        node: String,
        variant: String,
        undeclared: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Parsing selection strings
// ---------------------------------------------------------------------------

/// Build an [`GraphError::UnknownNode`] carrying enough context to diagnose it.
fn unknown_node(name: &str, config: &VeldConfig) -> GraphError {
    let mut known: Vec<String> = config.nodes.keys().cloned().collect();
    known.sort();
    GraphError::UnknownNode {
        name: name.to_owned(),
        known,
        split: config.loaded_from_multiple_files,
    }
}

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
                .ok_or_else(|| unknown_node(&sel.node, config))?;

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

/// How deep `@preset` references may nest, and how many expansion steps one
/// [`expand_preset`] may take.
///
/// **The cycle guard is not enough on its own, and the shape of the recursion is
/// why.** `visiting.pop()` on the way out is deliberate — a preset referenced from
/// two places is legal — but it also means such a preset is expanded *once per
/// reference*, so `p{i} = ["@p{i-1}", "@p{i-1}"]` costs 2^i steps while every
/// individual path stays acyclic. Measured before this guard existed: a 735-byte
/// config with 25 such presets burned 12.9s of CPU, and a 200 000-long linear
/// chain aborted the process outright with `fatal runtime error: stack overflow`.
///
/// That matters beyond a slow CLI. Expansion runs inside the daemon — the desktop
/// repo listing ships each preset's expansion so a client can tell a run's
/// recorded preset from what that name means now — and a stack overflow in any
/// thread kills the whole daemon, taking every PTY session with it. A `veld.json`
/// arrives with a checked-out branch, so "the config is trusted" is not an
/// assumption this can rest on.
///
/// **The step budget is the cost bound; the depth cap only keeps the stack
/// finite.** They are deliberately far apart. Depth was 16 for one revision and
/// that was wrong: a *linear* 17-level chain costs 17 steps and microseconds —
/// something a generated per-layer config plausibly produces — and refusing it
/// narrowed the accepted config contract for no benefit, since cost was already
/// bounded. 256 frames of this function is nothing next to even a 2 MB thread
/// stack, while no hand-written tree comes near it.
const PRESET_DEPTH_LIMIT: usize = 256;
const PRESET_STEP_LIMIT: usize = 4096;

/// Expand a preset name into its selections, following `@other-preset`
/// references (F9.5).
///
/// A preset entry starting with `@` names another preset instead of a node, so
/// `"ci": ["@full", "extra:default"]` is "everything in `full`, plus one more".
/// Without this, a project with overlapping preset sets has to repeat every
/// selection, and the repetitions drift.
///
/// Cycles are an error rather than a hang: `a → b → a` is a config mistake, and
/// the error names the path so it is obvious which link to cut. Acyclic-but-huge
/// is an error too — see [`PRESET_DEPTH_LIMIT`] — because the cycle guard alone
/// leaves a legal tree costing 2^depth.
pub fn expand_preset(
    preset_name: &str,
    config: &VeldConfig,
) -> Result<Vec<NodeSelection>, GraphError> {
    let mut visiting = Vec::new();
    let mut steps = 0usize;
    expand_preset_inner(preset_name, config, &mut visiting, &mut steps)
}

fn expand_preset_inner(
    preset_name: &str,
    config: &VeldConfig,
    visiting: &mut Vec<String>,
    steps: &mut usize,
) -> Result<Vec<NodeSelection>, GraphError> {
    // The cycle check comes FIRST, and that ordering is a finding, not a style
    // choice: a cycle entered below the depth limit would otherwise be reported as
    // `PresetTooLarge` and lose the `@a → @b → @a` path, which is the entire value
    // of that error. A cycle is a config mistake with a precise diagnosis; being
    // over budget is not.
    if visiting.iter().any(|p| p == preset_name) {
        let mut path = visiting.clone();
        path.push(preset_name.to_owned());
        return Err(GraphError::PresetCycle(
            path.iter()
                .map(|p| format!("@{p}"))
                .collect::<Vec<_>>()
                .join(" → "),
        ));
    }
    // Counted per *expansion*, not per unique preset: the same preset reached
    // twice is two units of work, which is exactly the cost being bounded.
    *steps += 1;
    if *steps > PRESET_STEP_LIMIT || visiting.len() >= PRESET_DEPTH_LIMIT {
        return Err(GraphError::PresetTooLarge {
            name: preset_name.to_owned(),
        });
    }
    let presets = config
        .presets
        .as_ref()
        .ok_or_else(|| GraphError::UnknownPreset(preset_name.to_owned()))?;
    let items = presets
        .get(preset_name)
        .ok_or_else(|| GraphError::UnknownPreset(preset_name.to_owned()))?
        .selections();

    visiting.push(preset_name.to_owned());
    let mut out: Vec<NodeSelection> = Vec::new();
    for item in items {
        match item.strip_prefix('@') {
            Some(referenced) => {
                for sel in expand_preset_inner(referenced, config, visiting, steps)? {
                    // De-duplicate: two presets that both include a node should
                    // not start it twice.
                    if !out.contains(&sel) {
                        out.push(sel);
                    }
                }
            }
            None => {
                let sel = parse_selection(item)?;
                if !out.contains(&sel) {
                    out.push(sel);
                }
            }
        }
    }
    visiting.pop();
    Ok(out)
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
        // Resolved, not raw: `depends_on` is hoistable to node level (F3), so a
        // raw read here would silently drop an inherited edge.
        let resolved = config
            .resolved(&sel.node, &sel.variant)
            .expect("selection was validated");
        let mut dep_set = HashSet::new();
        if let Some(dep_map) = &resolved.depends_on {
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

    // 4. Validate sensitive_outputs are subsets of declared outputs.
    validate_sensitive_outputs(&all_nodes, config)?;

    // 5. Kahn's algorithm for topological sort into stages.
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
            .ok_or_else(|| unknown_node(&sel.node, config))?;
        if !node_cfg.variants.contains_key(&sel.variant) {
            return Err(GraphError::UnknownVariant {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
            });
        }
        let resolved = config
            .resolved(&sel.node, &sel.variant)
            .expect("variant existence checked above");

        if let Some(dep_map) = &resolved.depends_on {
            for (dep_node, dep_variant) in dep_map {
                // Validate the dependency target exists.
                let dep_node_cfg = config
                    .nodes
                    .get(dep_node)
                    .ok_or_else(|| unknown_node(dep_node, config))?;
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

    // Scan project-level env for unqualified refs.
    if let Some(env_map) = &config.env {
        // Only inline literals are interpolated, so only they can hold a
        // `${nodes.…}` reference.
        for v in env_map.values().flatten().filter_map(|v| v.as_literal()) {
            check_string_for_ambiguous_refs(v, &active_variants)?;
        }
    }

    // For each node, scan its env, variant env, and command strings for unqualified refs.
    for sel in all_nodes {
        let Some(resolved) = config.resolved(&sel.node, &sel.variant) else {
            continue;
        };

        // The command's own strings: for `argv`, every element (each is
        // interpolated independently); for `shell`, the whole string.
        let mut owned_strings: Vec<String> = Vec::new();
        if let Some(spec) = resolved.command.clone() {
            match spec {
                crate::config::CommandSpec::Argv(argv) => owned_strings.extend(argv),
                crate::config::CommandSpec::Shell(sh) => owned_strings.push(sh),
            }
        }
        // The resolved env, so a value hoisted to node level is scanned once and
        // an erased key is not scanned at all.
        if let Some(env_map) = &resolved.env {
            owned_strings.extend(
                env_map
                    .values()
                    .filter_map(|v| v.as_literal())
                    .map(str::to_owned),
            );
        }
        let strings_to_check: Vec<&str> = owned_strings.iter().map(String::as_str).collect();

        for s in strings_to_check {
            check_string_for_ambiguous_refs(s, &active_variants)?;
        }
    }

    Ok(())
}

/// Validate that sensitive_outputs are subsets of declared outputs.
fn validate_sensitive_outputs(
    all_nodes: &[NodeSelection],
    config: &VeldConfig,
) -> Result<(), GraphError> {
    for sel in all_nodes {
        let resolved = config
            .resolved(&sel.node, &sel.variant)
            .expect("selection was validated");
        if let Some(ref sensitive) = resolved.sensitive_outputs {
            let declared = resolved
                .outputs
                .as_ref()
                .map(|o| o.declared_keys())
                .unwrap_or_default();
            let undeclared: Vec<String> = sensitive
                .iter()
                .filter(|k| !declared.contains(k.as_str()))
                .cloned()
                .collect();
            if !undeclared.is_empty() {
                return Err(GraphError::UndeclaredSensitiveOutputs {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                    undeclared,
                });
            }
        }
    }
    Ok(())
}

/// Return all nodes that transitively depend on `target` within the given
/// set of active nodes. The result is in topological order (direct dependents
/// first, transitive dependents later).
pub fn get_dependents(
    target: &NodeSelection,
    all_nodes: &[NodeSelection],
    config: &VeldConfig,
) -> Vec<NodeSelection> {
    // Build reverse adjacency: for each node, which nodes depend on it.
    let mut reverse_deps: HashMap<String, Vec<NodeSelection>> = HashMap::new();
    for sel in all_nodes {
        let Some(resolved) = config.resolved(&sel.node, &sel.variant) else {
            continue;
        };
        if let Some(dep_map) = &resolved.depends_on {
            for (dep_node, dep_variant) in dep_map {
                let dep_key = format!("{dep_node}:{dep_variant}");
                reverse_deps.entry(dep_key).or_default().push(sel.clone());
            }
        }
    }

    // BFS from target through reverse edges.
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<NodeSelection> = VecDeque::new();
    let target_key = format!("{}:{}", target.node, target.variant);
    visited.insert(target_key.clone());

    if let Some(direct) = reverse_deps.get(&target_key) {
        for dep in direct {
            let key = format!("{}:{}", dep.node, dep.variant);
            if visited.insert(key) {
                queue.push_back(dep.clone());
            }
        }
    }

    let mut result = Vec::new();
    while let Some(sel) = queue.pop_front() {
        let key = format!("{}:{}", sel.node, sel.variant);
        if let Some(further) = reverse_deps.get(&key) {
            for dep in further {
                let dep_key = format!("{}:{}", dep.node, dep.variant);
                if visited.insert(dep_key) {
                    queue.push_back(dep.clone());
                }
            }
        }
        result.push(sel);
    }

    result
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NodeConfig, StepType, VariantConfig, VeldConfig};
    use crate::presets::PresetDef;
    use indexmap::IndexMap;
    use std::collections::HashMap;

    /// Build a `presets` map from the array form, which is all these tests need —
    /// `@ref` expansion does not care about a preset's key or label.
    fn presets<const N: usize>(entries: [(&str, &[&str]); N]) -> IndexMap<String, PresetDef> {
        entries
            .into_iter()
            .map(|(name, items)| {
                (
                    name.to_owned(),
                    PresetDef::Selections(items.iter().map(|s| (*s).to_owned()).collect()),
                )
            })
            .collect()
    }

    fn make_config() -> VeldConfig {
        // db -> api -> frontend (dependency chain)
        let db_variant = VariantConfig {
            step_type: Some(StepType::Command),
            cmd: crate::config::CommandKeys {
                shell: Some("echo db".into()),
                ..Default::default()
            },
            script: None,
            health_check: None,
            probes: None,
            depends_on: None,
            env: None,
            ports: None,
            files: None,
            outputs: None,
            sensitive_outputs: None,
            strict_outputs: true,
            skip_if: None,
            url_template: None,
            on_stop: None,
            client_log_levels: None,
            features: None,
            proxy: None,
            cwd: None,
            share: None,
        };
        let api_variant = VariantConfig {
            step_type: Some(StepType::StartServer),
            cmd: crate::config::CommandKeys {
                shell: Some("echo api".into()),
                ..Default::default()
            },
            script: None,
            health_check: None,
            probes: None,
            depends_on: Some(HashMap::from([("db".into(), Some("local".into()))])),
            env: None,
            ports: None,
            files: None,
            outputs: None,
            sensitive_outputs: None,
            strict_outputs: true,
            skip_if: None,
            url_template: None,
            on_stop: None,
            client_log_levels: None,
            features: None,
            proxy: None,
            cwd: None,
            share: None,
        };
        let frontend_variant = VariantConfig {
            step_type: Some(StepType::StartServer),
            cmd: crate::config::CommandKeys {
                shell: Some("echo fe".into()),
                ..Default::default()
            },
            script: None,
            health_check: None,
            probes: None,
            depends_on: Some(HashMap::from([("api".into(), Some("local".into()))])),
            env: None,
            ports: None,
            files: None,
            outputs: None,
            sensitive_outputs: None,
            strict_outputs: true,
            skip_if: None,
            url_template: None,
            on_stop: None,
            client_log_levels: None,
            features: None,
            proxy: None,
            cwd: None,
            share: None,
        };

        VeldConfig {
            schema: None,
            schema_version: "3".into(),
            name: "test".into(),
            url_template: "{service}.{run}.{project}.localhost".into(),
            presets: None,
            default_preset: None,
            client_log_levels: None,
            features: None,
            proxy: None,
            env: None,
            sharing: None,
            setup: None,
            teardown: None,
            vars: None,
            hooks: None,
            ide: None,
            loaded_from_multiple_files: false,
            deferred_findings: Vec::new(),
            nodes: HashMap::from([
                (
                    "db".into(),
                    NodeConfig {
                        default_variant: Some("local".into()),
                        url_template: None,
                        hidden: None,
                        client_log_levels: None,
                        features: None,
                        proxy: None,
                        env: None,
                        cwd: None,
                        actions: None,
                        step_type: None,
                        cmd: Default::default(),
                        probes: None,
                        share: None,
                        ports: None,
                        depends_on: None,
                        outputs: None,
                        on_stop: None,
                        files: None,
                        variants: HashMap::from([("local".into(), db_variant)]),
                    },
                ),
                (
                    "api".into(),
                    NodeConfig {
                        default_variant: Some("local".into()),
                        url_template: None,
                        hidden: None,
                        client_log_levels: None,
                        features: None,
                        proxy: None,
                        env: None,
                        cwd: None,
                        actions: None,
                        step_type: None,
                        cmd: Default::default(),
                        probes: None,
                        share: None,
                        ports: None,
                        depends_on: None,
                        outputs: None,
                        on_stop: None,
                        files: None,
                        variants: HashMap::from([("local".into(), api_variant)]),
                    },
                ),
                (
                    "frontend".into(),
                    NodeConfig {
                        default_variant: Some("local".into()),
                        url_template: None,
                        hidden: None,
                        client_log_levels: None,
                        features: None,
                        proxy: None,
                        env: None,
                        cwd: None,
                        actions: None,
                        step_type: None,
                        cmd: Default::default(),
                        probes: None,
                        share: None,
                        ports: None,
                        depends_on: None,
                        outputs: None,
                        on_stop: None,
                        files: None,
                        variants: HashMap::from([("local".into(), frontend_variant)]),
                    },
                ),
            ]),
        }
    }

    /// F9.5: a preset may reference another with `@name`, so overlapping sets do
    /// not have to repeat every selection and then drift.
    #[test]
    fn presets_compose_and_deduplicate() {
        let mut config = make_config();
        config.presets = Some(presets([
            ("core", &["db:local", "api:local"][..]),
            ("full", &["@core", "frontend:local"]),
            // Two references to the same preset must not start anything twice.
            ("ci", &["@full", "@core"]),
        ]));

        let full = expand_preset("full", &config).unwrap();
        let names: Vec<String> = full.iter().map(|s| s.to_string()).collect();
        assert_eq!(names, vec!["db:local", "api:local", "frontend:local"]);

        let ci = expand_preset("ci", &config).unwrap();
        assert_eq!(
            ci.len(),
            3,
            "an already-included selection is not repeated: {ci:?}"
        );
    }

    /// A cycle is a config mistake, so it is an error naming the path rather than
    /// a hang.
    #[test]
    fn preset_cycle_is_an_error_naming_the_path() {
        let mut config = make_config();
        config.presets = Some(presets([("a", &["@b"][..]), ("b", &["@a"])]));
        let err = expand_preset("a", &config).unwrap_err();
        assert!(matches!(err, GraphError::PresetCycle(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("@a") && msg.contains("@b"), "{msg}");

        // A preset referencing itself directly is the same class of mistake.
        config.presets = Some(presets([("self", &["@self"][..])]));
        assert!(matches!(
            expand_preset("self", &config),
            Err(GraphError::PresetCycle(_))
        ));
    }

    /// A preset tree that is **acyclic and still ruinous**: each level references
    /// the one below it twice, so the cycle guard never fires and the cost doubles
    /// per level. Before the step budget this was 12.9s of CPU for a 735-byte
    /// config — and expansion runs inside the daemon on an ungated `GET`, per poll.
    #[test]
    fn an_acyclic_preset_tree_cannot_cost_exponential_work() {
        let mut config = make_config();
        let mut entries: IndexMap<String, PresetDef> = IndexMap::new();
        entries.insert(
            "p0".to_owned(),
            PresetDef::Selections(vec!["db:local".to_owned()]),
        );
        for i in 1..=24 {
            entries.insert(
                format!("p{i}"),
                PresetDef::Selections(vec![format!("@p{}", i - 1), format!("@p{}", i - 1)]),
            );
        }
        config.presets = Some(entries);
        // The STEP budget is what refuses this, and only it can: 24 levels of nesting
        // is nowhere near the 256-frame depth cap, while the tree's 2^24 expansions
        // blow a 4096-step budget almost immediately. The assert below keeps that
        // attribution true if either constant ever moves.
        let err = expand_preset("p24", &config).unwrap_err();
        assert!(matches!(err, GraphError::PresetTooLarge { .. }), "{err:?}");
        // A `const` block, so this is checked when the crate compiles rather than
        // when the test runs: if the depth cap ever drops to 24 or below, the build
        // stops here instead of this test quietly passing for the wrong reason.
        const {
            assert!(
                PRESET_DEPTH_LIMIT > 24,
                "deepen the doubling tree above: at this depth cap the DEPTH limit \
                 would refuse it, and this test exists to pin the STEP budget"
            )
        };
    }

    /// A cycle must keep its own diagnosis. The budget check used to run first, so a
    /// cycle reported `PresetTooLarge` and lost the `@a → @b → @a` path — the one
    /// thing that error exists to print.
    #[test]
    fn a_cycle_is_still_a_cycle_and_not_a_budget_refusal() {
        let mut config = make_config();
        let mut entries: IndexMap<String, PresetDef> = IndexMap::new();
        for i in 0..40 {
            entries.insert(
                format!("p{i}"),
                PresetDef::Selections(vec![format!("@p{}", (i + 1) % 40)]),
            );
        }
        config.presets = Some(entries);
        let err = expand_preset("p0", &config).unwrap_err();
        assert!(matches!(err, GraphError::PresetCycle(_)), "{err:?}");
        assert!(err.to_string().contains("@p0"), "{err}");
    }

    /// A linear chain costs one step per level, so the depth cap must not be what
    /// refuses it. Depth 16 did — 17 generated layers, microseconds of work, hard
    /// failure — which narrowed the config contract for nothing.
    #[test]
    fn a_long_linear_chain_is_cheap_and_stays_legal() {
        let mut config = make_config();
        let mut entries: IndexMap<String, PresetDef> = IndexMap::new();
        entries.insert(
            "p0".to_owned(),
            PresetDef::Selections(vec!["db:local".to_owned()]),
        );
        for i in 1..64 {
            entries.insert(
                format!("p{i}"),
                PresetDef::Selections(vec![format!("@p{}", i - 1)]),
            );
        }
        config.presets = Some(entries);
        let sels = expand_preset("p63", &config).expect("a 64-level chain is legal");
        assert_eq!(sels.len(), 1);
    }

    /// The limits must not refuse a config a person would actually write. Bump
    /// `PRESET_DEPTH_LIMIT` down and this fails — which is the point of asserting a
    /// concrete shape rather than trusting the constant.
    #[test]
    fn ordinary_preset_nesting_stays_well_inside_the_limits() {
        let mut config = make_config();
        // A 5-level chain with a reused leaf at every level: deeper and wider than
        // the 2-3 levels real configs use.
        config.presets = Some(presets([
            ("base", &["db:local"][..]),
            ("l1", &["@base", "api:local"]),
            ("l2", &["@l1", "@base"]),
            ("l3", &["@l2", "@l1"]),
            ("l4", &["@l3", "@l2", "frontend:local"]),
        ]));
        let sels = expand_preset("l4", &config).expect("ordinary nesting must expand");
        assert_eq!(
            sels.len(),
            3,
            "dedup still collapses the reused leaves: {sels:?}"
        );
    }

    #[test]
    fn unknown_preset_reference_is_named() {
        let mut config = make_config();
        config.presets = Some(presets([("ci", &["@nope"][..])]));
        assert!(matches!(
            expand_preset("ci", &config),
            Err(GraphError::UnknownPreset(name)) if name == "nope"
        ));
    }

    /// Under `include` globs, "unknown node" has four causes, so the error carries
    /// the material to tell them apart.
    #[test]
    fn unknown_node_error_lists_defined_nodes() {
        let mut config = make_config();
        config.loaded_from_multiple_files = true;
        let err = resolve_selections(
            &[NodeSelection {
                node: "typo".into(),
                variant: "local".into(),
            }],
            &config,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("typo"), "{msg}");
        assert!(msg.contains("api"), "must list what does exist: {msg}");
        assert!(
            msg.contains("veld config --files"),
            "a split config must point at the glob→file→node chain: {msg}"
        );

        // A single-file config gets the node list but not the include advice,
        // which would only mislead.
        config.loaded_from_multiple_files = false;
        let msg = resolve_selections(
            &[NodeSelection {
                node: "typo".into(),
                variant: "local".into(),
            }],
            &config,
        )
        .unwrap_err()
        .to_string();
        assert!(msg.contains("api"));
        assert!(!msg.contains("veld config --files"), "{msg}");
    }

    #[test]
    fn test_get_dependents_leaf_node() {
        let config = make_config();
        let all_nodes = vec![
            NodeSelection {
                node: "db".into(),
                variant: "local".into(),
            },
            NodeSelection {
                node: "api".into(),
                variant: "local".into(),
            },
            NodeSelection {
                node: "frontend".into(),
                variant: "local".into(),
            },
        ];
        let target = NodeSelection {
            node: "frontend".into(),
            variant: "local".into(),
        };
        let deps = get_dependents(&target, &all_nodes, &config);
        assert!(deps.is_empty(), "leaf node should have no dependents");
    }

    #[test]
    fn test_get_dependents_root_node() {
        let config = make_config();
        let all_nodes = vec![
            NodeSelection {
                node: "db".into(),
                variant: "local".into(),
            },
            NodeSelection {
                node: "api".into(),
                variant: "local".into(),
            },
            NodeSelection {
                node: "frontend".into(),
                variant: "local".into(),
            },
        ];
        let target = NodeSelection {
            node: "db".into(),
            variant: "local".into(),
        };
        let deps = get_dependents(&target, &all_nodes, &config);
        assert_eq!(deps.len(), 2);
        let dep_names: Vec<String> = deps.iter().map(|d| d.node.clone()).collect();
        assert!(dep_names.contains(&"api".to_string()));
        assert!(dep_names.contains(&"frontend".to_string()));
    }

    #[test]
    fn test_get_dependents_middle_node() {
        let config = make_config();
        let all_nodes = vec![
            NodeSelection {
                node: "db".into(),
                variant: "local".into(),
            },
            NodeSelection {
                node: "api".into(),
                variant: "local".into(),
            },
            NodeSelection {
                node: "frontend".into(),
                variant: "local".into(),
            },
        ];
        let target = NodeSelection {
            node: "api".into(),
            variant: "local".into(),
        };
        let deps = get_dependents(&target, &all_nodes, &config);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].node, "frontend");
    }
}
