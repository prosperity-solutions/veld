use veld_core::config;
use veld_core::state::RunState;

use crate::output;

/// `veld runs [--name <n>] [--json]` — run history: one row per execution
/// instance, newest first. Without `--name`, all environments' runs grouped
/// by environment.
pub async fn list(name: Option<&str>, json: bool) -> i32 {
    let Some((config_path, _cfg)) = super::parse_config(json) else {
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
    let Some((config_path, cfg)) = super::parse_config(json) else {
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
        // The FULL RunState (nodes as a map of complete node states, plus
        // graph_snapshot/execution_order) with the display short id added.
        // Deliberately richer than `veld runs --json`'s list entries — this
        // is the drill-down; don't parse the two `.nodes` shapes the same way.
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
    if let Some(label) = super::start_origin_label(
        run.graph_snapshot
            .as_ref()
            .and_then(|s| s.started_from.as_ref()),
        &cfg,
    ) {
        println!("{} {}", output::bold("Started from:"), label);
    }
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
                    println!("    {} {}", output::dim("command:"), cmd.display());
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
    let Some((config_path, _cfg)) = super::parse_config(json) else {
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
                "origin_changed": d.origin_changed,
                "var_overrides_changed": d.var_overrides_changed,
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
        match d.config_changed {
            Some(true) => output::yellow("changed"),
            Some(false) => output::green("identical"),
            None => output::dim("unknown (hash unavailable)"),
        },
    );
    // Before the graph, because it answers a different question: two runs can
    // resolve to the same nodes and still have been *asked for* differently, and
    // "Resolved graph is identical" would otherwise be the whole report.
    if let Some(origin) = &d.origin_changed {
        println!(
            "{} {} → {}",
            output::bold("Started from:"),
            output::red(origin.from.as_deref().unwrap_or("not recorded")),
            output::green(origin.to.as_deref().unwrap_or("not recorded")),
        );
    }
    // Also before the graph: this is the difference `veld.json: identical` cannot
    // account for, so printing it after "Resolved graph is identical" would bury
    // the only answer the reader came for.
    if !d.var_overrides_changed.is_empty() {
        println!("{}", output::bold("Values for this machine:"));
        for change in &d.var_overrides_changed {
            println!(
                "  {} {} → {}",
                change.field,
                output::red(change.from.as_deref().unwrap_or("not declared")),
                output::green(change.to.as_deref().unwrap_or("not declared")),
            );
        }
    }
    if old.created_at > new.created_at {
        println!(
            "{}",
            output::dim("note: the first run is newer than the second — arguments reversed?"),
        );
    }
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
    /// `None` when either hash is unavailable (a config read failed at start
    /// time) — unknown, not "identical".
    config_changed: Option<bool>,
    /// How each run was *asked for*, when the two differ (`"preset stack"` vs
    /// `"selections api:local"`).
    ///
    /// Two runs can resolve to the same graph and still answer "what did I run"
    /// differently — one from `--preset stack`, one from the same tokens typed by
    /// hand. Without this the command prints "Resolved graph is identical" and
    /// omits the only thing that was not.
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_changed: Option<FieldChange>,
    /// Machine-overridable vars whose *provenance* differed between the runs.
    ///
    /// The one difference `config_hash` structurally cannot see: an override
    /// changes the effective configuration without changing a byte of
    /// `veld.json`, so two runs that behaved differently otherwise report
    /// "identical" here — which is the single most confusing thing this command
    /// could say. Names and scope only, never values: that is the snapshot's
    /// invariant, not a limitation of the diff.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    var_overrides_changed: Vec<FieldChange>,
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
        // Rendered structurally: `["a","b c"]` must not read the same as
        // `["a","b","c"]`, or a real command change diffs as no change.
        push(
            "command",
            &o.command.as_ref().map(|c| c.display()),
            &n.command.as_ref().map(|c| c.display()),
        );
        push("cwd", &o.cwd, &n.cwd);
        push("url_template", &o.url_template, &n.url_template);
        // Compared through the parsed type, not as strings: snapshots taken
        // before the rename recorded `start_server` and later ones record
        // `long_running` for the same unchanged config, and a tool whose whole
        // job is "what changed since it worked" must not report a change nobody
        // made. An unparseable value falls back to a string compare rather than
        // silently reading as equal.
        let same_type = match (
            veld_core::config::StepType::parse(&o.step_type),
            veld_core::config::StepType::parse(&n.step_type),
        ) {
            (Some(a), Some(b)) => a == b,
            _ => o.step_type == n.step_type,
        };
        if !same_type {
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

    let origin_changed = {
        // The expansion is part of the description, not just the name. Two runs from
        // the same `--preset stack` with different recorded selections is exactly
        // "the preset was redefined between these runs" — the case the expansion is
        // stored for — and comparing names alone reported them as identical.
        let describe = |s: &veld_core::state::GraphSnapshot| {
            s.started_from.as_ref().map(|o| {
                let tokens = o.selections.join(", ");
                match &o.preset {
                    Some(p) => format!("preset {p} ({tokens})"),
                    None => format!("selections {tokens}"),
                }
            })
        };
        let (from, to) = (describe(old), describe(new));
        // Absent on both sides is not a change: pre-provenance runs know nothing
        // about how they were asked for, and reporting that as a difference would
        // make every old pair look edited.
        (from != to).then_some(FieldChange {
            field: "started from".to_owned(),
            from,
            to,
        })
    };

    // Compared only when **both** runs recorded the field. `None` means the run
    // predates it, and treating that as "declared nothing" would report every var
    // as newly-appeared on the first diff after an upgrade — a difference that
    // did not happen. `Some([])` is a real answer ("this project has no machine
    // vars") and compares normally; that distinction is the whole reason the
    // field is an `Option` rather than a possibly-empty vec.
    let var_overrides_changed: Vec<FieldChange> = match (&old.var_overrides, &new.var_overrides) {
        (Some(old_vars), Some(new_vars)) => {
            // Union of both sides' names, so a var only one run recorded still
            // shows — a var added to the config between the runs is exactly the
            // case worth seeing.
            let mut var_names: Vec<&str> = old_vars
                .iter()
                .chain(new_vars.iter())
                .map(|v| v.name.as_str())
                .collect();
            var_names.sort_unstable();
            var_names.dedup();
            var_names
                .into_iter()
                .filter_map(|name| {
                    let find = |vs: &[veld_core::state::VarOverrideSnapshot]| {
                        vs.iter().find(|v| v.name == name).map(|v| v.from.clone())
                    };
                    let (from, to) = (find(old_vars), find(new_vars));
                    (from != to).then(|| FieldChange {
                        field: name.to_owned(),
                        from,
                        to,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    };

    SnapshotDiff {
        config_changed: (!old.config_hash.is_empty() && !new.config_hash.is_empty())
            .then(|| old.config_hash != new.config_hash),
        origin_changed,
        var_overrides_changed,
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
            command: Some(veld_core::state::CommandSnapshot::Shell(cmd.into())),
            cwd: None,
            env_keys: env.iter().map(|s| s.to_string()).collect(),
            url_template: None,
        }
    }

    fn snap(hash: &str, nodes: &[(&str, NodeSnapshot)]) -> GraphSnapshot {
        GraphSnapshot {
            config_hash: hash.into(),
            started_from: None,
            nodes: nodes
                .iter()
                .map(|(k, n)| (k.to_string(), n.clone()))
                .collect(),
            var_overrides: Some(Vec::new()),
        }
    }

    /// **The difference `config_hash` structurally cannot see.**
    ///
    /// A machine override changes the effective configuration without changing a
    /// byte of `veld.json`, so two runs that behaved differently have identical
    /// hashes and identical `NodeSnapshot`s (env is names-only). Before this,
    /// `veld runs diff` recorded the provenance and then printed "Resolved graph
    /// is identical" — the field existed and answered nothing.
    #[test]
    fn diff_reports_a_var_answered_differently_between_two_runs() {
        use veld_core::state::VarOverrideSnapshot;
        let vo = |from: &str| VarOverrideSnapshot {
            name: "log_level".to_owned(),
            from: from.to_owned(),
        };
        let mut old = snap("same", &[]);
        let mut new = snap("same", &[]);
        old.var_overrides = Some(vec![vo("default")]);
        new.var_overrides = Some(vec![vo("project")]);

        let d = diff_snapshots(&old, &new);
        assert_eq!(
            d.config_changed,
            Some(false),
            "the file is byte-identical — which is exactly why the hash cannot help"
        );
        assert_eq!(d.var_overrides_changed.len(), 1);
        assert_eq!(d.var_overrides_changed[0].field, "log_level");
        assert_eq!(d.var_overrides_changed[0].from.as_deref(), Some("default"));
        assert_eq!(d.var_overrides_changed[0].to.as_deref(), Some("project"));

        // Same provenance on both sides is not a difference.
        new.var_overrides = Some(vec![vo("default")]);
        assert!(diff_snapshots(&old, &new).var_overrides_changed.is_empty());
    }

    /// **The rename is not a config change.** Snapshots taken before it recorded
    /// `start_server`; later ones record `long_running` for the same unchanged
    /// config. Comparing the strings made the first `veld runs diff` spanning the
    /// upgrade report a type change on every long-running node in the project —
    /// in the one tool whose entire job is answering "what changed since it
    /// worked". A real change between the two types still reports.
    #[test]
    fn diff_does_not_invent_a_type_change_across_the_step_type_rename() {
        let mut old_node = node("serve", &[]);
        old_node.step_type = "start_server".into();
        let mut new_node = node("serve", &[]);
        new_node.step_type = "long_running".into();

        let d = diff_snapshots(
            &snap("same", &[("web:local", old_node.clone())]),
            &snap("same", &[("web:local", new_node.clone())]),
        );
        assert!(
            d.changed.is_empty(),
            "an alias is not a change: {} node(s) reported",
            d.changed.len()
        );

        // A genuine type change is still reported.
        let mut became_command = new_node.clone();
        became_command.step_type = "command".into();
        let d = diff_snapshots(
            &snap("same", &[("web:local", old_node)]),
            &snap("same", &[("web:local", became_command)]),
        );
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].fields[0].field, "type");
    }

    /// A var only one run recorded shows as declared-on-one-side, rather than
    /// being dropped: a var added to the config between two runs is precisely
    /// the case someone runs `diff` to understand.
    #[test]
    fn diff_reports_a_var_only_one_run_knew_about() {
        use veld_core::state::VarOverrideSnapshot;
        let mut old = snap("a", &[]);
        let mut new = snap("b", &[]);
        old.var_overrides = Some(vec![]);
        new.var_overrides = Some(vec![VarOverrideSnapshot {
            name: "runtime".to_owned(),
            from: "worktree".to_owned(),
        }]);
        let d = diff_snapshots(&old, &new);
        assert_eq!(d.var_overrides_changed.len(), 1);
        assert_eq!(d.var_overrides_changed[0].from, None);
        assert_eq!(d.var_overrides_changed[0].to.as_deref(), Some("worktree"));
    }

    /// **A run recorded before the field existed reports nothing, not a phantom
    /// difference.**
    ///
    /// `None` (absent) and `Some([])` (no machine vars declared) are different
    /// facts, and only the type distinguishes them — with an empty vec for both,
    /// the first diff after upgrading claimed every var had just appeared. The
    /// pair of assertions here is the point: absent stays silent, empty compares.
    #[test]
    fn diff_says_nothing_about_a_run_recorded_before_the_field_existed() {
        use veld_core::state::VarOverrideSnapshot;
        let mut old = snap("same", &[]);
        let mut new = snap("same", &[]);
        old.var_overrides = None; // predates the field
        new.var_overrides = Some(vec![VarOverrideSnapshot {
            name: "runtime".to_owned(),
            from: "project".to_owned(),
        }]);
        assert!(
            diff_snapshots(&old, &new).var_overrides_changed.is_empty(),
            "an absent record is not evidence that anything changed"
        );

        // …but a run that genuinely declared none still compares.
        old.var_overrides = Some(Vec::new());
        assert_eq!(
            diff_snapshots(&old, &new).var_overrides_changed.len(),
            1,
            "`Some([])` is a real answer and must not be treated as absent"
        );
    }

    #[test]
    fn diff_reports_a_changed_origin_even_when_the_graph_matches() {
        // The case that made this necessary: `--preset stack` and the same tokens
        // typed by hand resolve to one graph, so every other line of the report
        // says "identical" while the one thing that differs is invisible.
        let same = [("api:local", node("npm run dev", &["PORT"]))];
        let mut old = snap("aaa", &same);
        let mut new = snap("aaa", &same);
        old.started_from = Some(veld_core::state::StartOrigin {
            preset: Some("stack".into()),
            selections: vec!["api:local".into()],
        });
        new.started_from = Some(veld_core::state::StartOrigin {
            preset: None,
            selections: vec!["api:local".into()],
        });
        let d = diff_snapshots(&old, &new);
        assert!(d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty());
        let origin = d
            .origin_changed
            .expect("origin difference must be reported");
        assert_eq!(origin.from.as_deref(), Some("preset stack (api:local)"));
        assert_eq!(origin.to.as_deref(), Some("selections api:local"));
    }

    #[test]
    fn a_preset_redefined_between_two_runs_is_reported() {
        // Same preset name, different recorded expansion — the preset was edited
        // between the two runs. Comparing names alone called this identical, which
        // is the one case the stored expansion exists to catch.
        let same = [("api:local", node("npm run dev", &[]))];
        let mut old = snap("aaa", &same);
        let mut new = snap("bbb", &same);
        old.started_from = Some(veld_core::state::StartOrigin {
            preset: Some("stack".into()),
            selections: vec!["api:local".into()],
        });
        new.started_from = Some(veld_core::state::StartOrigin {
            preset: Some("stack".into()),
            selections: vec!["api:local".into(), "web:local".into()],
        });
        let origin = diff_snapshots(&old, &new)
            .origin_changed
            .expect("a redefined preset must be reported");
        assert_eq!(origin.from.as_deref(), Some("preset stack (api:local)"));
        assert_eq!(
            origin.to.as_deref(),
            Some("preset stack (api:local, web:local)")
        );
    }

    #[test]
    fn two_pre_provenance_runs_do_not_read_as_an_origin_change() {
        // Both sides absent is not a difference — otherwise every pair of runs
        // recorded before provenance existed would report one.
        let same = [("api:local", node("npm run dev", &[]))];
        let d = diff_snapshots(&snap("aaa", &same), &snap("aaa", &same));
        assert!(d.origin_changed.is_none());
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
        assert_eq!(d.config_changed, Some(true));
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
        assert_eq!(d.config_changed, Some(false));
        assert!(d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty());
    }
}
