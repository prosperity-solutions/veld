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

/// `veld runs show <id-prefix> [--json]` — one run in full: identity,
/// outcome, node results, and the graph snapshot it was started with.
pub async fn show(id_prefix: &str, json: bool) -> i32 {
    let Some((config_path, _cfg)) = super::load_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);
    let Some(db) = super::open_db(json) else {
        return 1;
    };
    let run = match db.get_run_by_id_prefix(&project_root, id_prefix) {
        Ok(Some(r)) => r,
        Ok(None) => {
            output::print_error(
                &format!("No run matches id prefix '{id_prefix}' (see `veld runs`)."),
                json,
            );
            return 1;
        }
        Err(e) => {
            output::print_error(&format!("{e}"), json);
            return 1;
        }
    };

    if json {
        // The full RunState (including the graph snapshot) plus the display
        // short id — same field meanings as `veld runs --json`.
        let mut payload = serde_json::to_value(&run).unwrap_or_default();
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("short_id".into(), run.short_id().into());
        }
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        return 0;
    }

    println!(
        "{} {}",
        output::bold("Environment:"),
        output::cyan(&run.name)
    );
    println!(
        "{} {} {}",
        output::bold("Run:"),
        run.short_id(),
        output::dim(&run.run_id.to_string()),
    );
    println!("{} {}", output::bold("Outcome:"), colorize_outcome(&run));
    println!(
        "{} {}",
        output::bold("Started:"),
        run.created_at
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S"),
    );
    if let Some(ended) = run.ended_at {
        println!(
            "{} {}  ({})",
            output::bold("Ended:"),
            ended
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S"),
            fmt_duration(&run),
        );
    }

    println!();
    println!("{}", output::bold("Nodes:"));
    let mut keys: Vec<&String> = run.nodes.keys().collect();
    keys.sort();
    let rows: Vec<Vec<String>> = keys
        .iter()
        .map(|k| {
            let ns = &run.nodes[*k];
            vec![
                ns.node_name.clone(),
                ns.variant.clone(),
                format!("{:?}", ns.status).to_lowercase(),
                ns.outputs.get("exit_code").cloned().unwrap_or_default(),
            ]
        })
        .collect();
    output::print_table(&["NODE", "VARIANT", "STATUS", "EXIT"], &rows);

    println!();
    match &run.graph_snapshot {
        None => println!(
            "{}",
            output::dim("No graph snapshot recorded (run started by an older veld).")
        ),
        Some(snap) => {
            println!(
                "{} {}",
                output::bold("Config:"),
                output::dim(&format!(
                    "veld.json sha256 {}…",
                    &snap.config_hash[..12.min(snap.config_hash.len())]
                )),
            );
            for (key, n) in &snap.nodes {
                println!();
                println!("  {} {}", output::cyan(key), output::dim(&n.step_type));
                if let Some(cmd) = &n.command {
                    println!("    {} {}", output::dim("command:"), cmd);
                }
                if let Some(cwd) = &n.cwd {
                    println!("    {} {}", output::dim("cwd:"), cwd);
                }
                if !n.env_keys.is_empty() {
                    println!("    {} {}", output::dim("env:"), n.env_keys.join(", "));
                }
                if let Some(t) = &n.url_template {
                    println!("    {} {}", output::dim("url:"), t);
                }
            }
        }
    }

    0
}

/// `veld runs diff <old> [<new>] [--json]` — what changed in the config
/// between two runs. With one id, the run is compared against its
/// predecessor in the same environment.
pub async fn diff(a: &str, b: Option<&str>, json: bool) -> i32 {
    let Some((config_path, _cfg)) = super::load_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);
    let Some(db) = super::open_db(json) else {
        return 1;
    };

    let resolve = |prefix: &str| -> Result<veld_core::state::RunState, String> {
        match db.get_run_by_id_prefix(&project_root, prefix) {
            Ok(Some(r)) => Ok(r),
            Ok(None) => Err(format!(
                "No run matches id prefix '{prefix}' (see `veld runs`)."
            )),
            Err(e) => Err(e.to_string()),
        }
    };

    // Two args: `diff <old> <new>`. One arg: <new> = the given run, <old> =
    // its predecessor in the same environment.
    let (old, new) = if let Some(b) = b {
        match (resolve(a), resolve(b)) {
            (Ok(o), Ok(n)) => (o, n),
            (Err(e), _) | (_, Err(e)) => {
                output::print_error(&e, json);
                return 1;
            }
        }
    } else {
        let new = match resolve(a) {
            Ok(r) => r,
            Err(e) => {
                output::print_error(&e, json);
                return 1;
            }
        };
        let history = match db.list_runs(&project_root, Some(&new.name)) {
            Ok(h) => h,
            Err(e) => {
                output::print_error(&format!("Failed to load run history: {e}"), json);
                return 1;
            }
        };
        let pos = history.iter().position(|r| r.run_id == new.run_id);
        let old = pos.and_then(|i| history.get(i + 1)).cloned();
        match old {
            Some(o) => (o, new),
            None => {
                output::print_error(
                    &format!(
                        "Run {} has no predecessor in environment '{}'.",
                        new.short_id(),
                        new.name
                    ),
                    json,
                );
                return 1;
            }
        }
    };

    let (Some(snap_old), Some(snap_new)) = (&old.graph_snapshot, &new.graph_snapshot) else {
        output::print_error(
            "Both runs need a graph snapshot to diff (runs started by an older veld have none).",
            json,
        );
        return 1;
    };

    let d = diff_snapshots(snap_old, snap_new);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "old": { "run_id": old.run_id, "short_id": old.short_id(), "outcome": old.outcome_label() },
                "new": { "run_id": new.run_id, "short_id": new.short_id(), "outcome": new.outcome_label() },
                "config_changed": d.config_changed,
                "added": d.added,
                "removed": d.removed,
                "changed": d.changed,
            }))
            .unwrap()
        );
        return 0;
    }

    println!(
        "{} {} ({}) → {} ({})",
        output::bold("Comparing"),
        old.short_id(),
        old.outcome_label(),
        new.short_id(),
        new.outcome_label(),
    );
    println!(
        "{} {}",
        output::bold("veld.json:"),
        if d.config_changed {
            output::yellow("changed")
        } else {
            output::green("identical")
        },
    );
    if d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty() {
        println!("{}", output::dim("Resolved graph is identical."));
        return 0;
    }
    for key in &d.added {
        println!("{} {}", output::green("+"), output::cyan(key));
    }
    for key in &d.removed {
        println!("{} {}", output::red("-"), output::cyan(key));
    }
    for ch in &d.changed {
        println!("{} {}", output::yellow("~"), output::cyan(&ch.node));
        for f in &ch.fields {
            if let Some(from) = &f.from {
                println!("    {} {}: {}", output::red("-"), f.field, from);
            }
            if let Some(to) = &f.to {
                println!("    {} {}: {}", output::green("+"), f.field, to);
            }
        }
    }

    0
}

#[derive(serde::Serialize)]
struct SnapshotDiff {
    config_changed: bool,
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<NodeChange>,
}

#[derive(serde::Serialize)]
struct NodeChange {
    node: String,
    fields: Vec<FieldChange>,
}

#[derive(serde::Serialize)]
struct FieldChange {
    field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
}

/// Structural diff of two graph snapshots (old → new).
fn diff_snapshots(
    old: &veld_core::state::GraphSnapshot,
    new: &veld_core::state::GraphSnapshot,
) -> SnapshotDiff {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for key in new.nodes.keys() {
        if !old.nodes.contains_key(key) {
            added.push(key.clone());
        }
    }
    for key in old.nodes.keys() {
        if !new.nodes.contains_key(key) {
            removed.push(key.clone());
        }
    }
    for (key, n) in &new.nodes {
        let Some(o) = old.nodes.get(key) else {
            continue;
        };
        if o == n {
            continue;
        }
        let mut fields = Vec::new();
        let mut push = |field: &str, from: &Option<String>, to: &Option<String>| {
            if from != to {
                fields.push(FieldChange {
                    field: field.to_owned(),
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        };
        push("command", &o.command, &n.command);
        push("cwd", &o.cwd, &n.cwd);
        push("url_template", &o.url_template, &n.url_template);
        if o.step_type != n.step_type {
            fields.push(FieldChange {
                field: "type".to_owned(),
                from: Some(o.step_type.clone()),
                to: Some(n.step_type.clone()),
            });
        }
        if o.env_keys != n.env_keys {
            fields.push(FieldChange {
                field: "env".to_owned(),
                from: Some(o.env_keys.join(", ")),
                to: Some(n.env_keys.join(", ")),
            });
        }
        if !fields.is_empty() {
            changed.push(NodeChange {
                node: key.clone(),
                fields,
            });
        }
    }

    SnapshotDiff {
        config_changed: old.config_hash != new.config_hash,
        added,
        removed,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::state::{GraphSnapshot, NodeSnapshot};

    fn node(cmd: &str, env: &[&str]) -> NodeSnapshot {
        NodeSnapshot {
            step_type: "start_server".into(),
            command: Some(cmd.into()),
            cwd: None,
            env_keys: env.iter().map(|s| s.to_string()).collect(),
            url_template: None,
        }
    }

    fn snap(hash: &str, nodes: &[(&str, NodeSnapshot)]) -> GraphSnapshot {
        GraphSnapshot {
            config_hash: hash.into(),
            nodes: nodes
                .iter()
                .map(|(k, n)| (k.to_string(), n.clone()))
                .collect(),
        }
    }

    #[test]
    fn diff_detects_added_removed_changed_fields() {
        let old = snap(
            "aaa",
            &[
                ("api:local", node("npm run dev", &["PORT"])),
                ("cache:local", node("redis-server", &[])),
            ],
        );
        let new = snap(
            "bbb",
            &[
                (
                    "api:local",
                    node("npm run dev --turbo", &["PORT", "DATABASE_URL"]),
                ),
                ("worker:local", node("npm run worker", &[])),
            ],
        );
        let d = diff_snapshots(&old, &new);
        assert!(d.config_changed);
        assert_eq!(d.added, vec!["worker:local"]);
        assert_eq!(d.removed, vec!["cache:local"]);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].node, "api:local");
        let fields: Vec<&str> = d.changed[0]
            .fields
            .iter()
            .map(|f| f.field.as_str())
            .collect();
        assert_eq!(fields, vec!["command", "env"]);
    }

    #[test]
    fn diff_identical_snapshots_is_empty() {
        let s = snap("aaa", &[("api:local", node("npm run dev", &["PORT"]))]);
        let d = diff_snapshots(&s, &s);
        assert!(!d.config_changed);
        assert!(d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty());
    }
}
