use veld_core::graph;
use veld_core::orchestrator::Orchestrator;
use veld_core::share::DaemonClient;

use crate::output;

/// `veld restart [--name <n>] [--debug]`
pub async fn run(name: Option<String>, debug: bool) -> i32 {
    let Some((config_path, config)) = super::parse_config(false) else {
        return 1;
    };

    let mut orchestrator = match Orchestrator::new(config_path.clone(), config.clone()) {
        Ok(o) => o,
        Err(e) => {
            output::print_error(&format!("Failed to initialize: {e}"), false);
            return 1;
        }
    };

    let project_state = match orchestrator
        .db
        .load_project_state(&orchestrator.project_root)
    {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), false);
            return 1;
        }
    };

    let run_name = match super::resolve_run_name(name, &project_state, true, false) {
        Some(n) => n,
        None => return 1,
    };
    let run_name = run_name.as_str();

    // Capture the outgoing run's id before stopping: a restart mints a NEW
    // run id, so anything keyed to the old instance must be released here or
    // it is orphaned. Shares are exactly that — the daemon's share manager
    // holds them in memory keyed by run id, and nothing else here would release
    // this one: the GC pass unshares runs it finds dead, but the stop below
    // finalizes this run cleanly so GC never sees it as an orphan, leaving only
    // the TTL hours later. Meanwhile the share points at torn-down ports — and a
    // web share stays registered with the public gateway — while both dashboards
    // attach shares to the *current* run id and can no longer show it.
    // `veld stop` already does this; a restart is a stop followed by a start.
    let prior_run_id = project_state
        .runs
        .get(run_name)
        .map(|r| r.run_id.to_string());

    // Take the selections from the latest run — live or ended history (the
    // "dev crashed overnight → veld restart" case reads the crashed run).
    //
    // The origin travels with them, **verbatim**, or not at all. Node rows are the
    // dependency closure, so rebuilding an origin from them would record a wider
    // selection set than the invocation asked for — which is what
    // `StartOrigin::new` explicitly forbids, and it would make every restarted run
    // read as `redefined since start`.
    //
    // A run recorded before provenance existed therefore carries `None` forward.
    // Synthesising `selections: <the whole closure>` for it would put a claim the
    // user never made into the database permanently, and print it — worse than the
    // blank line `start_origin_label(None, …)` already produces.
    let (selections, origin) = match project_state.get_run(run_name) {
        Some(run_state) => {
            let selections: Vec<graph::NodeSelection> = run_state
                .nodes
                .values()
                .map(|ns| graph::NodeSelection {
                    node: ns.node_name.clone(),
                    variant: ns.variant.clone(),
                })
                .collect();
            let origin = run_state
                .graph_snapshot
                .as_ref()
                .and_then(|s| s.started_from.clone());
            (selections, origin)
        }
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), false);
            return 1;
        }
    };

    // **Before the stop, not after.** `veld start` refuses (or asks) before the
    // first spawn; a restart that discovered the same missing answer inside the
    // second `start` would already have torn the environment down — so pulling a
    // commit that adds a machine var with no default would take the developer's
    // running environment away and leave them with an error. Checked here, the
    // environment is still up when the refusal arrives.
    let project_root = veld_core::config::project_root(&config_path);
    // Kept, because the orchestrator that receives them below is discarded and
    // rebuilt after the stop — and an answer the human declined to save exists
    // nowhere but here. `veld restart` has no `--var`, so the refusal must not
    // offer one.
    let Some(machine_answers) =
        super::start::resolve_machine_vars(&mut orchestrator, &selections, &project_root, false)
            .await
    else {
        return 1;
    };

    output::print_info(&format!("Restarting environment '{run_name}'..."));

    // Stop the existing run (a no-op teardown when it already ended).
    if let Err(e) = orchestrator.stop(run_name).await {
        output::print_error(&format!("Failed to stop '{run_name}': {e}"), false);
        return 1;
    }

    // Best-effort, same contract as `veld stop`: silent on failure (the daemon
    // may be down, or there may be no shares). Unlike `veld stop`, this sits
    // BETWEEN the teardown and the restart, so it gets a timeout — a daemon that
    // accepts the connection but never answers would otherwise wedge the command
    // with the environment down and never brought back up, which is strictly
    // worse than the same hang once everything is already stopped.
    if let Some(run_id) = &prior_run_id {
        // Three outcomes, all distinct — collapsing them to 0 would make both
        // failures silent, and a share that survives a restart is not inert: the
        // new run re-binds the same ports, so a surviving WEB share keeps its
        // public URL live and quietly re-points it at the new processes. It is
        // also unreachable from either dashboard (both attach shares by run id,
        // and this one holds the dead run's) and GC never releases it, because
        // the stop above already finalized the run. Only the TTL would, hours
        // later. So a failure here has to be said out loud, with the manual
        // remedy. All output is stderr per the AGENTS.md diagnostics rule (the
        // `print_info` calls around it predate that convention).
        let notice = |msg: String| eprintln!("  {} {msg}", output::dim("»"));
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            DaemonClient::new().unshare_run(run_id),
        )
        .await
        {
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => notice(format!(
                "Released {n} share(s) of the previous run — re-share if you \
                 still need the URL."
            )),
            // A daemon that isn't running holds no shares — they live only in
            // its memory, never on disk — so there is nothing to warn about, and
            // the remedy below would fail identically (`veld shares` uses the
            // same client).
            Ok(Err(veld_core::share::DaemonError::NotRunning)) => {}
            Ok(Err(e)) => notice(format!(
                "Could not release the previous run's shares ({e}) — any share \
                 of it now points at the restarted run. Check `veld shares` \
                 and stop it with `veld unshare <id>`."
            )),
            Err(_) => notice(
                "Timed out releasing the previous run's shares — any share of \
                 it now points at the restarted run. Check `veld shares` and \
                 stop it with `veld unshare <id>`."
                    .to_string(),
            ),
        }
    }

    // Start again with a fresh orchestrator.
    let mut orchestrator = match Orchestrator::new(config_path, config) {
        Ok(o) => o,
        Err(e) => {
            output::print_error(&format!("Failed to initialize: {e}"), false);
            return 1;
        }
    };
    orchestrator.set_debug(debug);
    // The answers from the pre-flight above. Without this the second
    // orchestrator reads only the store, so a prompt answer the user chose not
    // to save is lost — after the environment has already been torn down.
    if !machine_answers.is_empty() {
        orchestrator.set_var_answers(machine_answers);
    }

    match orchestrator.start(&selections, run_name, origin).await {
        Ok(new_run) => {
            output::print_success(&format!("Environment '{run_name}' restarted."));

            let urls: Vec<(&str, &str)> = new_run
                .nodes
                .values()
                .filter_map(|ns| ns.url.as_ref().map(|u| (ns.node_name.as_str(), u.as_str())))
                .collect();

            if !urls.is_empty() {
                println!();
                for (node, url) in &urls {
                    println!("  {} {}", output::cyan(node), url);
                }
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to restart '{run_name}': {e}"), false);
            1
        }
    }
}
