//! Node-defined actions (`actions` in veld.json): resolve one against a run, and
//! run it unattended for a face with no terminal (the IDE's action buttons).
//!
//! `veld action` in a terminal does not come through here to *run*: it is
//! interactive (a DB shell), so the CLI asks for the resolved command and execs it
//! itself, with the user's own environment underneath.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use veld_core::config::{self, ActionConfig, VeldConfig};
use veld_core::db::LogStream;
use veld_core::logging::LogWriter;
use veld_core::state::{NodeState, ProjectState, RunState};
use veld_core::variables::{self, VariableContext};

use crate::api::{ActionOutcome, FailureKind, ResolvedAction};

fn failed(message: impl Into<String>) -> ActionOutcome {
    ActionOutcome::Failed {
        message: message.into(),
        kind: FailureKind::Error,
        failed_node: None,
    }
}

/// Resolve `action_name` against a run: which node, what command (interpolated),
/// in which directory, with which environment overlay. Errors are the exact
/// user-facing messages `veld action` printed.
///
/// A stopped run's outputs are no longer live, so `run: None` never offers one.
/// `env` is the overlay only (node env, node outputs as `$KEY`, parameters, the
/// veld-owned `VELD_*` family); the caller runs the command with its own
/// environment underneath.
///
/// An `argv` action is returned as a single shell line with every element
/// single-quoted, so it keeps its argument count; `command` is always run with
/// `$SHELL -c`.
pub async fn resolve(
    cfg: &VeldConfig,
    config_path: &Path,
    state: &ProjectState,
    action_name: &str,
    run: Option<String>,
    node: Option<&str>,
    display_only: bool,
) -> Result<ResolvedAction, String> {
    let project_root = config::project_root(config_path);

    let (run_name, note) = crate::preflight::resolve_run_name(run, state, false)?;
    let run_state = state
        .get_run(&run_name)
        .ok_or_else(|| format!("Run '{run_name}' not found."))?;

    let (action, node_state) = match resolve_action(cfg, run_state, action_name, node) {
        Ok(pair) => pair,
        Err(ActionResolveError::Unknown) => {
            let known = configured_action_names(cfg);
            return Err(if known.is_empty() {
                format!("No actions are defined in {}.", config_path.display())
            } else {
                format!(
                    "Unknown action '{action_name}'. Defined actions: {}",
                    known.join(", ")
                )
            });
        }
        Err(ActionResolveError::Unavailable) => {
            return Err(format!(
                "Action '{action_name}' is not available for run '{run_name}' \
                 (no running node satisfies its required outputs)."
            ));
        }
        Err(ActionResolveError::Ambiguous(nodes)) => {
            return Err(format!(
                "Action '{action_name}' is defined on multiple nodes in run \
                 '{run_name}'. Pick one with --node: {}",
                nodes.join(", ")
            ));
        }
    };

    // Build the interpolation context + environment from the node's live state.
    let ctx = build_context(cfg, &run_name, &project_root, node_state, action);

    let Some(action_cmd) = action.cmd.spec() else {
        return Err(format!(
            "Action '{action_name}' declares no command \u{2014} set \"argv\" or \"shell\"."
        ));
    };
    let resolved_command = action_cmd
        .interpolate(&ctx)
        .map_err(|e| format!("Failed to resolve action command: {e}"))?;
    // An `argv` action is exec'd as its elements, never through a shell: joining
    // and re-splitting would depend on the user's `$SHELL` agreeing with POSIX
    // quoting, which fish does not. `command` is then only the display form.
    let (command, argv) = match resolved_command {
        config::CommandSpec::Shell(script) => (script, None),
        config::CommandSpec::Argv(argv) => (
            argv.iter()
                .map(|a| veld_core::console::quote(a))
                .collect::<Vec<_>>()
                .join(" "),
            Some(argv),
        ),
    };

    // Resolve the working directory the same way the orchestrator does for the
    // running variant (variant > node > project root).
    let node_cfg = cfg.nodes.get(&node_state.node_name);
    let variant_cfg = node_cfg.and_then(|n| n.variants.get(&node_state.variant));
    let cwd = config::resolve_cwd(
        &project_root,
        node_cfg.and_then(|n| n.cwd.as_deref()),
        variant_cfg.and_then(|v| v.cwd.as_deref()),
    );

    // `--print`/`--json` only show the command: a node env that cannot be
    // resolved right now (an unset `{ "env": … }`) must not stop that — it never
    // did before the engine owned this.
    let env = match build_env(cfg, run_state, &project_root, node_state, action, &ctx).await {
        Ok(env) => env,
        Err(_) if display_only => Default::default(),
        Err(e) => return Err(format!("Failed to resolve action parameters: {e}")),
    };

    Ok(ResolvedAction {
        note,
        command,
        argv,
        label: action.display_label().to_owned(),
        cwd,
        env,
        run: run_name,
        node: Some(format!("{}:{}", node_state.node_name, node_state.variant)),
    })
}

/// One configured action, for `veld actions` (the CLI renders it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionListing {
    pub node: String,
    pub name: String,
    pub label: String,
    pub description: Option<String>,
    pub requires_outputs: Option<Vec<String>>,
}

/// The actions configured across all nodes, sorted by (node, name) for stable
/// output.
pub fn list(cfg: &VeldConfig) -> Vec<ActionListing> {
    let mut rows: Vec<(&String, &ActionConfig)> = Vec::new();
    for (node_name, node_cfg) in &cfg.nodes {
        if let Some(actions) = &node_cfg.actions {
            for action in actions {
                rows.push((node_name, action));
            }
        }
    }
    rows.sort_by(|a, b| (a.0.as_str(), a.1.name.as_str()).cmp(&(b.0.as_str(), b.1.name.as_str())));
    rows.into_iter()
        .map(|(node, action)| ActionListing {
            node: node.clone(),
            name: action.name.clone(),
            label: action.display_label().to_owned(),
            description: action.description.clone(),
            requires_outputs: action.requires_outputs.clone(),
        })
        .collect()
}

/// Run `action` against `run` with no terminal: stdin null, output to the run's
/// internal log (each line prefixed `[action <name>] `).
///
/// The command runs under `$SHELL -c` (falling back to `/bin/sh`), as the
/// terminal path does: an action is an interactive convenience, so the author's
/// aliases and shell functions are part of what they wrote.
pub async fn run_unattended(
    config_path: &Path,
    cfg: VeldConfig,
    run: &str,
    action: &str,
    node: Option<String>,
) -> ActionOutcome {
    let project_root = config::project_root(config_path);
    let db = match veld_core::db::Db::open() {
        Ok(db) => db,
        Err(e) => return failed(format!("Failed to open state: {e}")),
    };
    let state = match db.load_project_state(&project_root) {
        Ok(s) => s,
        Err(e) => return failed(format!("Failed to load state: {e}")),
    };
    let resolved = match resolve(
        &cfg,
        config_path,
        &state,
        action,
        Some(run.to_owned()),
        node.as_deref(),
        false,
    )
    .await
    {
        Ok(r) => r,
        Err(message) => return failed(message),
    };

    let mut log = LogWriter::for_run(db, &project_root, &resolved.run, LogStream::Internal);
    if let Some(run_state) = state.get_run(&resolved.run) {
        log.set_run_id(run_state.run_id);
    }
    let write = |text: String| {
        let log = &log;
        async move {
            for line in text.lines() {
                let _ = log.write_line(&format!("[action {action}] {line}")).await;
            }
        }
    };

    let mut command = match resolved.argv.as_deref() {
        Some([program, args @ ..]) => {
            let mut c = tokio::process::Command::new(program);
            c.args(args);
            c
        }
        _ => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
            let mut c = tokio::process::Command::new(shell);
            c.arg("-c").arg(&resolved.command);
            c
        }
    };
    // Output to a file, and the child waited on — not its pipes. An action that
    // backgrounds something (`nohup x &`, a tail) leaves a descendant holding
    // whatever it inherited; waiting for a pipe's EOF would wait for that
    // descendant, and the worker (and its `engine_actions` row) with it.
    let capture = std::env::temp_dir().join(format!("veld-action-{}.log", uuid::Uuid::new_v4()));
    // Owner-only and new: it holds the action's output, which may carry secrets,
    // and `temp_dir()` is `/tmp` for a service without `TMPDIR`.
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&capture)
    };
    let file = match opened {
        Ok(f) => f,
        Err(e) => return failed(format!("Failed to run action '{action}': {e}")),
    };
    let stderr = match file.try_clone() {
        Ok(f) => f,
        Err(e) => return failed(format!("Failed to run action '{action}': {e}")),
    };
    let status = command
        .current_dir(&resolved.cwd)
        .envs(&resolved.env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(file))
        .stderr(std::process::Stdio::from(stderr))
        .status()
        .await;
    // Lossy: one non-UTF-8 byte must not drop the whole output.
    let captured = std::fs::read(&capture)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let _ = std::fs::remove_file(&capture);
    match status {
        Ok(status) => {
            write(captured).await;
            let exit_code = status
                .code()
                .unwrap_or(if status.success() { 0 } else { 1 });
            write(format!("exited with code {exit_code}")).await;
            ActionOutcome::ActionRan { exit_code }
        }
        Err(e) => {
            write(format!("failed to start: {e}")).await;
            failed(format!("Failed to run action '{action}': {e}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Why an action could not be resolved to a single runnable (action, node).
pub(crate) enum ActionResolveError {
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
pub(crate) fn resolve_action<'a>(
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
pub(crate) fn configured_action_names(config: &VeldConfig) -> Vec<String> {
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
        // `url` **and** its pieces, from the same derivation the node's own
        // command and its `on_stop` use. Setting only `url` here meant
        // `${veld.url.hostname}` — the spelling the schema and the configuration
        // reference both teach — failed an action with "unknown built-in
        // variable" while working everywhere else.
        for (key, value) in crate::orchestrator::url_builtins(url) {
            ctx.set_builtin(key, value);
        }
    }
    // A named port is an output at start time; make it addressable here by the
    // same `${veld.ports.<name>}` spelling the node's own command used.
    for (k, v) in &node_state.outputs {
        if k.starts_with("ports.") {
            ctx.set_builtin(k, v.clone());
        }
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

/// Build the environment overlay for the spawned shell: the node's declared `env`, the node's live outputs as `$KEY`, the action's
/// resolved parameters, and the veld-owned `VELD_*` variables.
///
/// Precedence (lowest to highest): (the face's inherited env, underneath) < node `env` < node
/// outputs < action parameters. Outputs are the freshest truth about the live
/// node, so they win over the env it was started with; the action's own params
/// win over both. This is the same ordering the orchestrator uses, so an action
/// sees what the node it runs against sees.
async fn build_env(
    config: &VeldConfig,
    run_state: &RunState,
    project_root: &Path,
    node_state: &NodeState,
    action: &ActionConfig,
    ctx: &VariableContext,
) -> Result<BTreeMap<String, String>, String> {
    // Deliberately NOT seeded from `std::env::vars()`: this is the overlay only.
    // The face that execs the command (the CLI, in the user's terminal) lays it
    // over its own whole environment; `run_unattended` lays it over this
    // process's. Either way the precedence below is unchanged.
    let mut env: BTreeMap<String, String> = BTreeMap::new();

    // The node's declared `env` (variant > node > project), resolved with the
    // same builder the node process uses, so an action on a node can read the
    // very variables the node was started with — a `stop-container` action
    // needs the `CONTAINER_NAME` the node was given. Previously an action had
    // to re-derive it in its own argv.
    //
    // Best-effort, never action-fatal: an action's interpolation context has no
    // `${vars.*}` (by design — see the Availability notes), so a node env that
    // references a var cannot resolve here. That must not block the action it
    // was attached to; it just means that env doesn't reach the action.
    if let Some(resolved) = config.resolved(&node_state.node_name, &node_state.variant) {
        if let Some(env_cfg) = resolved.env {
            if let Ok((node_env, _)) = crate::orchestrator::build_env(
                Some(&env_cfg),
                ctx,
                &format!(
                    "nodes.{}.variants.{}",
                    node_state.node_name, node_state.variant
                ),
                project_root,
            )
            .await
            {
                env.extend(node_env);
            } else {
                tracing::warn!(
                    node = node_state.node_name,
                    variant = node_state.variant,
                    "action could not resolve the node's declared env — it will not be exposed to the action"
                );
            }
        }
    }

    // Node outputs first; parameters can intentionally override them.
    for (k, v) in &node_state.outputs {
        env.insert(k.clone(), v.clone());
    }
    if let Some(params) = &action.parameters {
        for (k, v) in params {
            env.insert(
                k.clone(),
                variables::interpolate(v, ctx).map_err(|e| e.to_string())?,
            );
        }
    }

    // The veld-owned vars — `VELD_RUN`, `VELD_ROOT`, `VELD_PORT`/`VELD_URL`/
    // `VELD_HOST_*`, `VELD_NODE`, `VELD_VARIANT` — from the shared builder, so
    // an action sees the same family the node's own process and `on_stop` do.
    env.extend(crate::orchestrator::veld_owned_env(
        &crate::orchestrator::NodeEnvContext::from_node_state(
            run_state.name.as_str(),
            Some(run_state.run_id.to_string()),
            project_root,
            &config.name,
            node_state,
        ),
    ));

    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use veld_core::config::NodeConfig;

    fn config_with_actions(node: &str, actions: Vec<ActionConfig>) -> VeldConfig {
        let json = r#"{"schemaVersion":"3","name":"test","nodes":{}}"#;
        let mut cfg: VeldConfig = serde_json::from_str(json).unwrap();
        let node_json = r#"{"variants":{"local":{"type":"start_server","shell": "x"}}}"#;
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
            cmd: veld_core::config::CommandKeys {
                shell: Some("echo hi".to_owned()),
                ..Default::default()
            },
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
            serde_json::from_str(r#"{"variants":{"local":{"type":"start_server","shell": "x"}}}"#)
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

    fn state_with(run: RunState) -> ProjectState {
        ProjectState {
            runs: HashMap::from([(run.name.clone(), run)]),
        }
    }

    #[tokio::test]
    async fn resolve_builds_overlay_env_without_inherited_vars() {
        let mut a = action("postico", &["DB_HOST"]);
        a.cmd = veld_core::config::CommandKeys {
            argv: Some(vec!["psql".to_owned(), "${output.DB_HOST} x".to_owned()]),
            ..Default::default()
        };
        a.parameters = Some(HashMap::from([(
            "DSN".to_owned(),
            "pg://${output.DB_HOST}".to_owned(),
        )]));
        let cfg = config_with_actions("db", vec![a]);
        let run = run_with(vec![node_state("db", "local", &[("DB_HOST", "localhost")])]);
        let state = state_with(run);

        // A single live run is picked when none is named.
        let r = resolve(
            &cfg,
            Path::new("/proj/veld.json"),
            &state,
            "postico",
            None,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(r.run, "test-run");
        assert_eq!(r.node.as_deref(), Some("db:local"));
        // argv keeps its argument count: the element with a space stays one word.
        assert_eq!(r.command, "'psql' 'localhost x'");
        assert_eq!(r.env["DB_HOST"], "localhost");
        assert_eq!(r.env["DSN"], "pg://localhost");
        assert_eq!(r.env["VELD_RUN"], "test-run");
        assert!(
            !r.env.contains_key("PATH"),
            "env is an overlay, not the inherited env"
        );
    }

    #[tokio::test]
    async fn resolve_reports_user_facing_errors() {
        let cfg = config_with_actions("db", vec![action("postico", &["DB_HOST"])]);
        let state = state_with(run_with(vec![node_state("db", "local", &[])]));
        let p = Path::new("/proj/veld.json");
        let e = resolve(
            &cfg,
            p,
            &state,
            "nope",
            Some("test-run".into()),
            None,
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(e, "Unknown action 'nope'. Defined actions: postico");
        let e = resolve(
            &cfg,
            p,
            &state,
            "postico",
            Some("test-run".into()),
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(e.contains("is not available for run 'test-run'"));
        let e = resolve(&cfg, p, &state, "postico", Some("gone".into()), None, false)
            .await
            .unwrap_err();
        assert_eq!(e, "Run 'gone' not found.");
    }

    #[test]
    fn list_is_sorted_by_node_then_name() {
        let mut cfg = config_with_actions("zeta", vec![action("b", &[]), action("a", &[])]);
        let mut n: NodeConfig =
            serde_json::from_str(r#"{"variants":{"local":{"type":"start_server","shell": "x"}}}"#)
                .unwrap();
        n.actions = Some(vec![action("z", &["K"])]);
        cfg.nodes.insert("alpha".to_owned(), n);
        let rows = list(&cfg);
        let keys: Vec<_> = rows
            .iter()
            .map(|r| (r.node.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(keys, [("alpha", "z"), ("zeta", "a"), ("zeta", "b")]);
        assert_eq!(rows[0].requires_outputs, Some(vec!["K".to_owned()]));
    }
}
