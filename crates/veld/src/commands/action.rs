use veld_core::config::CommandSpec;

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
    let Some(project_root) = super::project_root() else {
        return 1;
    };
    let client = match super::engine().await {
        Ok(c) => c,
        Err(code) => return code,
    };
    // The engine resolves (run, node, live outputs, command, env); this face
    // runs it, here, in this terminal — an action is interactive (a DB shell),
    // so it keeps the user's whole environment underneath what the engine adds.
    // The env names the node's `{ "env": … }` sources read, and this terminal's
    // values for them — resolved by the engine, run here.
    let names: Vec<String> = match client
        .get(
            "/api/engine/env-names",
            &[("project_root", project_root.to_string_lossy().into_owned())],
        )
        .await
    {
        Ok(names) => names,
        Err(e) => {
            output::print_error(&e.to_string(), json);
            return if e.is_update_in_progress() { 75 } else { 1 };
        }
    };
    let resolved = match client
        .resolve_action(&veld_engine::api::ResolveActionRequest {
            env: super::start::caller_env(&names),
            display_only: print || json,
            project_root,
            action: action_name.clone(),
            run: name,
            node,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            output::print_error(&e.to_string(), json);
            return if e.is_update_in_progress() { 75 } else { 1 };
        }
    };
    let spec = match &resolved.argv {
        Some(argv) => CommandSpec::Argv(argv.clone()),
        None => CommandSpec::Shell(resolved.command.clone()),
    };
    let node_key = resolved.node.clone().unwrap_or_default();
    // `--print` and `--json` are machine modes: their stdout is the payload.
    if let (Some(note), false) = (&resolved.note, print || json) {
        output::print_info(note);
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": resolved.run,
                "node": node_key,
                "action": action_name,
                "command": spec,
            }))
            .unwrap()
        );
        return 0;
    }
    if print {
        println!("{}", spec.display());
        return 0;
    }

    output::print_info(&format!(
        "Running action {} {}",
        output::cyan(&resolved.label),
        output::dim(&format!("(run: {}, node: {node_key})", resolved.run)),
    ));

    // An `argv` action is spawned directly. A `shell` action keeps running under
    // the user's `$SHELL` rather than `sh`: an action is an interactive
    // convenience (open a DB client, tail a queue), so the author's aliases and
    // shell functions are part of what they wrote.
    let spawn = match &spec {
        CommandSpec::Shell(script) => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
            let mut c = std::process::Command::new(&shell);
            c.arg("-c").arg(script);
            Ok(c)
        }
        argv => veld_core::process::std_command(argv),
    };
    let status = match spawn {
        Ok(mut c) => c.current_dir(&resolved.cwd).envs(&resolved.env).status(),
        Err(e) => {
            output::print_error(&format!("Failed to run action '{action_name}': {e}"), json);
            return 1;
        }
    };
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
    let Some((_config_path, cfg)) = super::parse_config(json) else {
        return 1;
    };
    let rows = veld_engine::run_action::list(&cfg);

    if json {
        let payload: Vec<serde_json::Value> = rows
            .iter()
            .map(|a| {
                serde_json::json!({
                    "node": a.node,
                    "name": a.name,
                    "label": a.label,
                    "description": a.description,
                    "requires_outputs": a.requires_outputs,
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
        .map(|a| {
            vec![
                a.name.clone(),
                a.node.clone(),
                a.description.clone().unwrap_or_default(),
            ]
        })
        .collect();
    output::print_table(&["ACTION", "NODE", "DESCRIPTION"], &table);
    0
}
