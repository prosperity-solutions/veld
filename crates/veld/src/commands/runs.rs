use veld_core::config;
use veld_core::state::RunState;

use crate::output;

/// `veld runs [--name <n>] [--json]` — run history: one row per execution
/// instance, newest first. Without `--name`, all environments' runs grouped
/// by environment.
pub async fn list(name: Option<&str>, json: bool) -> i32 {
    let Some((config_path, _cfg)) = super::load_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let Some(db) = super::open_db(json) else {
        return 1;
    };
    let runs = match db.list_runs(&project_root, name) {
        Ok(r) => r,
        Err(e) => {
            output::print_error(&format!("Failed to load run history: {e}"), json);
            return 1;
        }
    };

    if json {
        let payload: Vec<serde_json::Value> = runs
            .iter()
            .map(|r| {
                let mut nodes: Vec<serde_json::Value> = r
                    .nodes
                    .iter()
                    .map(|(key, ns)| {
                        serde_json::json!({
                            "key": key,
                            "node": ns.node_name,
                            "variant": ns.variant,
                            "status": ns.status,
                        })
                    })
                    .collect();
                nodes.sort_by_key(|n| n["key"].as_str().map(str::to_owned));
                serde_json::json!({
                    // `name` keeps meaning the environment name, as before.
                    "name": r.name,
                    "run_id": r.run_id,
                    "short_id": r.short_id(),
                    "status": r.status,
                    "end_reason": r.end_reason,
                    "end_detail": r.end_detail,
                    "created_at": r.created_at.to_rfc3339(),
                    "ended_at": r.ended_at.map(|t| t.to_rfc3339()),
                    "nodes": nodes,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else if runs.is_empty() {
        match name {
            Some(n) => output::print_info(&format!("No runs recorded for environment '{n}'.")),
            None => output::print_info("No runs recorded."),
        }
    } else {
        let single_env = name.is_some();
        let mut header: Vec<&str> = vec!["RUN", "STARTED", "ENDED", "DURATION", "OUTCOME"];
        if !single_env {
            header.insert(1, "ENV");
        }
        let rows: Vec<Vec<String>> = runs
            .iter()
            .map(|r| {
                let mut row = vec![
                    r.short_id(),
                    r.created_at
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
                    r.ended_at
                        .map(|t| {
                            t.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M")
                                .to_string()
                        })
                        .unwrap_or_else(|| "—".to_owned()),
                    fmt_duration(r),
                    colorize_outcome(r),
                ];
                if !single_env {
                    row.insert(1, r.name.clone());
                }
                row
            })
            .collect();
        output::print_table(&header, &rows);
    }

    0
}

/// Wall-clock span of the run: start → end, or start → now while live.
fn fmt_duration(run: &RunState) -> String {
    let end = run.ended_at.unwrap_or_else(chrono::Utc::now);
    let secs = (end - run.created_at).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn colorize_outcome(run: &RunState) -> String {
    use veld_core::state::{EndReason, RunStatus};
    let label = run.outcome_label();
    match (&run.end_reason, &run.status) {
        (Some(EndReason::Failed | EndReason::Crashed), _) => output::red(&label),
        (Some(EndReason::Completed), _) => output::green(&label),
        (Some(_), _) => output::dim(&label),
        (None, RunStatus::Running) => output::green(&label),
        (None, _) => output::yellow(&label),
    }
}
