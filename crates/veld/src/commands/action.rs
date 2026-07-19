use std::collections::HashMap;
use std::path::Path;

use veld_core::config::{self, ActionConfig, VeldConfig};
use veld_core::state::{NodeState, RunState};
use veld_core::variables::{self, VariableContext};

use crate::output;

/// `veld action <name> [--name <run>] [--node <node>] [--print] [--json]`
///
/// Run a node-defined action against a running environment. Actions are the
/// generic mechanism behind things like "open the database in Postico": the
/// node declares a shell command, and the node's live outputs are made
/// available to it as `${output.KEY}` variables and `$KEY` environment
/// variables.
pub async fn run(
    action_name: String,
    name: Option<String>,
    node: Option<String>,
    print: bool,
    json: bool,
) -> i32 {
    let Some((config_path, cfg)) = super::load_config(json) else {
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

    // `--print` and `--json` are machine modes: keep stdout limited to the
    // payload by suppressing the "Using run …" notice.
    let machine = print || json;
    // A stopped run's outputs are no longer live, so don't offer one.
    let run_name = match super::resolve_run_name(name, &project_state, false, machine) {
        Some(n) => n,
        None => return 1,
    };

    let run_state = match project_state.get_run(&run_name) {
        Some(r) => r,
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), json);
            return 1;
        }
    };

    let (action, node_state) = match resolve_action(&cfg, run_state, &action_name, node.as_deref())
    {
        Ok(pair) => pair,
        Err(ActionResolveError::Unknown) => {
            let known = configured_action_names(&cfg);
            let detail = if known.is_empty() {
                format!("No actions are defined in {}.", config_path.display())
            } else {
                format!(
                    "Unknown action '{action_name}'. Defined actions: {}",
                    known.join(", ")
                )
            };
            output::print_error(&detail, json);
            return 1;
        }
        Err(ActionResolveError::Unavailable) => {
            output::print_error(
                &format!(
                    "Action '{action_name}' is not available for run '{run_name}' \
                         (no running node satisfies its required outputs)."
                ),
                json,
            );
            return 1;
        }
        Err(ActionResolveError::Ambiguous(nodes)) => {
            output::print_error(
                &format!(
                    "Action '{action_name}' is defined on multiple nodes in run \
                         '{run_name}'. Pick one with --node: {}",
                    nodes.join(", ")
                ),
                json,
            );
            return 1;
        }
    };

    // Build the interpolation context + environment from the node's live state.
    let ctx = build_context(&cfg, &run_name, &project_root, node_state, action);

    let resolved_command = match variables::interpolate(&action.command, &ctx) {
        Ok(c) => c,
        Err(e) => {
            output::print_error(&format!("Failed to resolve action command: {e}"), json);
            return 1;
        }
    };

    let node_key = format!("{}:{}", node_state.node_name, node_state.variant);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": run_name,
                "node": node_key,
                "action": action.name,
                "command": resolved_command,
            }))
            .unwrap()
        );
        return 0;
    }

    if print {
        println!("{resolved_command}");
        return 0;
    }

    // Resolve the working directory the same way the orchestrator does for the
    // running variant (variant > node > project root).
    let node_cfg = cfg.nodes.get(&node_state.node_name);
    let variant_cfg = node_cfg.and_then(|n| n.variants.get(&node_state.variant));
    let working_dir = config::resolve_cwd(
        &project_root,
        node_cfg.and_then(|n| n.cwd.as_deref()),
        variant_cfg.and_then(|v| v.cwd.as_deref()),
    );

    let env = match build_env(node_state, action, &ctx) {
        Ok(env) => env,
        Err(e) => {
            output::print_error(&format!("Failed to resolve action parameters: {e}"), json);
            return 1;
        }
    };

    output::print_info(&format!(
        "Running action {} {}",
        output::cyan(action.display_label()),
        output::dim(&format!("(run: {run_name}, node: {node_key})")),
    ));

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    let status = std::process::Command::new(&shell)
        .arg("-c")
        .arg(&resolved_command)
        .current_dir(&working_dir)
        .envs(env)
        .status();

    match status {
        Ok(s) => s.code().unwrap_or(if s.success() { 0 } else { 1 }),
        Err(e) => {
            output::print_error(&format!("Failed to run action '{action_name}': {e}"), json);
            1
        }
    }
}

/// `veld actions [--json]` — list the actions configured across all nodes.
pub async fn list(json: bool) -> i32 {
    let Some((_config_path, cfg)) = super::load_config(json) else {
        return 1;
    };

    // Collect (node, action) pairs sorted for stable output.
    let mut rows: Vec<(String, &ActionConfig)> = Vec::new();
    for (node_name, node_cfg) in &cfg.nodes {
        if let Some(actions) = &node_cfg.actions {
            for action in actions {
                rows.push((node_name.clone(), action));
            }
        }
    }
    rows.sort_by(|a, b| (a.0.as_str(), a.1.name.as_str()).cmp(&(b.0.as_str(), b.1.name.as_str())));

    if json {
        let payload: Vec<serde_json::Value> = rows
            .iter()
            .map(|(node, action)| {
                serde_json::json!({
                    "node": node,
                    "name": action.name,
                    "label": action.display_label(),
                    "description": action.description,
                    "requires_outputs": action.requires_outputs,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        return 0;
    }

    if rows.is_empty() {
        output::print_info("No actions defined. Add an \"actions\" array to a node in veld.json.");
        return 0;
    }

    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|(node, action)| {
            vec![
                action.name.clone(),
                node.clone(),
                action.description.clone().unwrap_or_default(),
            ]
        })
        .collect();
    output::print_table(&["ACTION", "NODE", "DESCRIPTION"], &table);
    0
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Why an action could not be resolved to a single runnable (action, node).
enum ActionResolveError {
    /// No node defines an action with this name.
    Unknown,
    /// The action exists but no running node satisfies its required outputs
    /// (or matches the `--node` filter).
    Unavailable,
    /// The action is available on more than one node; `--node` is needed.
    Ambiguous(Vec<String>),
}

/// Resolve `action_name` to the single (action, node state) it should run
/// against in `run_state`, honouring the optional `--node` filter.
fn resolve_action<'a>(
    config: &'a VeldConfig,
    run_state: &'a RunState,
    action_name: &str,
    node_filter: Option<&str>,
) -> Result<(&'a ActionConfig, &'a NodeState), ActionResolveError> {
    let mut defined = false;

    // Candidate (action, node_state) pairs that are actually runnable now.
    let mut candidates: Vec<(&ActionConfig, &NodeState)> = Vec::new();

    for (node_name, node_cfg) in &config.nodes {
        let Some(actions) = &node_cfg.actions else {
            continue;
        };
        let Some(action) = actions.iter().find(|a| a.name == action_name) else {
            continue;
        };
        defined = true;

        if node_filter.is_some_and(|f| f != node_name) {
            continue;
        }

        // Match against running node states for this config node, gated on the
        // action's required outputs.
        for node_state in run_state.nodes.values() {
            if node_state.node_name == *node_name && action.outputs_satisfied(&node_state.outputs) {
                candidates.push((action, node_state));
            }
        }
    }

    if !defined {
        return Err(ActionResolveError::Unknown);
    }

    candidates.sort_by(|a, b| {
        (a.1.node_name.as_str(), a.1.variant.as_str())
            .cmp(&(b.1.node_name.as_str(), b.1.variant.as_str()))
    });

    match candidates.as_slice() {
        [] => Err(ActionResolveError::Unavailable),
        [only] => Ok(*only),
        many => Err(ActionResolveError::Ambiguous(
            many.iter()
                .map(|(_, ns)| format!("{}:{}", ns.node_name, ns.variant))
                .collect(),
        )),
    }
}

/// All action names defined anywhere in the config (sorted, de-duplicated).
fn configured_action_names(config: &VeldConfig) -> Vec<String> {
    let mut names: Vec<String> = config
        .nodes
        .values()
        .filter_map(|n| n.actions.as_ref())
        .flatten()
        .map(|a| a.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

// ---------------------------------------------------------------------------
// Context + environment
// ---------------------------------------------------------------------------

/// Build the variable-interpolation context for an action:
/// - veld builtins (`${veld.run}`, `${veld.node}`, `${veld.port}`, …)
/// - the node's own outputs as `${output.KEY}`
/// - the action's static parameters as `${param.KEY}`
///
/// Actions are node-scoped: a command can only see the outputs of the node it
/// is attached to. (Those outputs are also exported as `$KEY` environment
/// variables in [`build_env`].)
fn build_context(
    config: &VeldConfig,
    run_name: &str,
    project_root: &Path,
    node_state: &NodeState,
    action: &ActionConfig,
) -> VariableContext {
    let mut ctx = VariableContext::new();
    ctx.set_builtin("run", run_name.to_owned());
    ctx.set_builtin("root", project_root.to_string_lossy().into_owned());
    ctx.set_builtin("project", config.name.clone());
    ctx.set_builtin("name", config.name.clone());
    ctx.set_builtin("node", node_state.node_name.clone());
    ctx.set_builtin("variant", node_state.variant.clone());
    if let Some(port) = node_state.port {
        ctx.set_builtin("port", port.to_string());
    }
    if let Some(url) = &node_state.url {
        ctx.set_builtin("url", url.clone());
    }

    for (k, v) in &node_state.outputs {
        ctx.set_output(k, v.clone());
    }

    // Parameters may themselves reference builtins/outputs already set above.
    if let Some(params) = &action.parameters {
        for (k, v) in params {
            let resolved = variables::interpolate(v, &ctx).unwrap_or_else(|_| v.clone());
            ctx.set_param(k, resolved);
        }
    }

    ctx
}

/// Build the environment for the spawned shell: inherit the parent env, then
/// export the node's live outputs and the action's resolved parameters as
/// `$KEY` variables, plus a few `VELD_*` context variables.
fn build_env(
    node_state: &NodeState,
    action: &ActionConfig,
    ctx: &VariableContext,
) -> Result<HashMap<String, String>, variables::VariableError> {
    let mut env: HashMap<String, String> = std::env::vars().collect();

    // Node outputs first; parameters can intentionally override them.
    for (k, v) in &node_state.outputs {
        env.insert(k.clone(), v.clone());
    }
    if let Some(params) = &action.parameters {
        for (k, v) in params {
            env.insert(k.clone(), variables::interpolate(v, ctx)?);
        }
    }

    env.insert("VELD_NODE".to_owned(), node_state.node_name.clone());
    env.insert("VELD_VARIANT".to_owned(), node_state.variant.clone());

    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::config::NodeConfig;

    fn config_with_actions(node: &str, actions: Vec<ActionConfig>) -> VeldConfig {
        let json = r#"{"schemaVersion":"2","name":"test","nodes":{}}"#;
        let mut cfg: VeldConfig = serde_json::from_str(json).unwrap();
        let node_json = r#"{"variants":{"local":{"type":"start_server","command":"x"}}}"#;
        let mut node_cfg: NodeConfig = serde_json::from_str(node_json).unwrap();
        node_cfg.actions = Some(actions);
        cfg.nodes.insert(node.to_owned(), node_cfg);
        cfg
    }

    fn action(name: &str, requires: &[&str]) -> ActionConfig {
        ActionConfig {
            name: name.to_owned(),
            label: None,
            description: None,
            command: "echo hi".to_owned(),
            parameters: None,
            requires_outputs: if requires.is_empty() {
                None
            } else {
                Some(requires.iter().map(|s| s.to_string()).collect())
            },
        }
    }

    fn node_state(name: &str, variant: &str, outputs: &[(&str, &str)]) -> NodeState {
        let mut ns = NodeState::new(name, variant);
        ns.outputs = outputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ns
    }

    fn run_with(nodes: Vec<NodeState>) -> RunState {
        let mut run = RunState::new("test-run", "test");
        for ns in nodes {
            run.nodes
                .insert(format!("{}:{}", ns.node_name, ns.variant), ns);
        }
        run
    }

    #[test]
    fn unknown_action_errors() {
        let cfg = config_with_actions("db", vec![action("postico", &[])]);
        let run = run_with(vec![node_state("db", "local", &[])]);
        assert!(matches!(
            resolve_action(&cfg, &run, "nope", None),
            Err(ActionResolveError::Unknown)
        ));
    }

    #[test]
    fn unavailable_when_outputs_missing() {
        let cfg = config_with_actions("db", vec![action("postico", &["DB_HOST"])]);
        let run = run_with(vec![node_state("db", "local", &[])]);
        assert!(matches!(
            resolve_action(&cfg, &run, "postico", None),
            Err(ActionResolveError::Unavailable)
        ));
    }

    #[test]
    fn resolves_single_available_action() {
        let cfg = config_with_actions("db", vec![action("postico", &["DB_HOST"])]);
        let run = run_with(vec![node_state("db", "local", &[("DB_HOST", "localhost")])]);
        let (a, ns) = resolve_action(&cfg, &run, "postico", None).ok().unwrap();
        assert_eq!(a.name, "postico");
        assert_eq!(ns.node_name, "db");
    }

    #[test]
    fn ambiguous_across_nodes_needs_filter() {
        let mut cfg = config_with_actions("primary", vec![action("psql", &[])]);
        let mut replica: NodeConfig =
            serde_json::from_str(r#"{"variants":{"local":{"type":"start_server","command":"x"}}}"#)
                .unwrap();
        replica.actions = Some(vec![action("psql", &[])]);
        cfg.nodes.insert("replica".to_owned(), replica);

        let run = run_with(vec![
            node_state("primary", "local", &[]),
            node_state("replica", "local", &[]),
        ]);
        match resolve_action(&cfg, &run, "psql", None) {
            Err(ActionResolveError::Ambiguous(nodes)) => {
                assert_eq!(nodes, vec!["primary:local", "replica:local"]);
            }
            _ => panic!("expected Ambiguous"),
        }
        // --node disambiguates.
        let (_, ns) = resolve_action(&cfg, &run, "psql", Some("replica"))
            .ok()
            .unwrap();
        assert_eq!(ns.node_name, "replica");
    }

    #[test]
    fn context_exposes_outputs_and_params() {
        let cfg = config_with_actions("db", vec![]);
        let mut a = action("postico", &[]);
        a.parameters = Some(HashMap::from([(
            "DSN".to_owned(),
            "pg://${output.DB_HOST}".to_owned(),
        )]));
        let ns = node_state("db", "local", &[("DB_HOST", "localhost")]);
        let ctx = build_context(&cfg, "dev", Path::new("/tmp"), &ns, &a);

        let out =
            variables::interpolate("${param.DSN} run=${veld.run} node=${veld.node}", &ctx).unwrap();
        assert_eq!(out, "pg://localhost run=dev node=db");
    }

    #[test]
    fn node_scoped_context_has_no_nodes_namespace() {
        // Actions are node-scoped: the `${nodes.*}` cross-node syntax is not
        // available inside an action command, so it fails to resolve.
        let cfg = config_with_actions("db", vec![]);
        let a = action("dump", &[]);
        let ns = node_state("db", "local", &[("DB_HOST", "localhost")]);
        let ctx = build_context(&cfg, "dev", Path::new("/tmp"), &ns, &a);

        assert_eq!(
            variables::interpolate("${output.DB_HOST}", &ctx).unwrap(),
            "localhost"
        );
        assert!(variables::interpolate("${nodes.db.DB_HOST}", &ctx).is_err());
    }
}
