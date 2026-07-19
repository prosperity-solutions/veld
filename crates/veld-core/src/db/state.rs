//! Run state + global project registry, stored in the central database.
//!
//! Replaces the old per-project `.veld/state.json` and the global
//! `registry.json`. The registry is no longer a second store that can drift —
//! it is derived from the same `projects`/`runs`/`nodes` tables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::state::{
    GlobalRegistry, NodeState, NodeStatus, ProjectState, ReadinessPhase, RegistryEntry,
    RegistryRunInfo, RunState, RunStatus,
};

use super::{Db, DbError, ts_to_str};

// ---------------------------------------------------------------------------
// Status <-> TEXT
// ---------------------------------------------------------------------------

pub(crate) fn run_status_str(s: &RunStatus) -> &'static str {
    match s {
        RunStatus::Starting => "starting",
        RunStatus::Running => "running",
        RunStatus::Recovering => "recovering",
        RunStatus::Stopping => "stopping",
        RunStatus::Stopped => "stopped",
        RunStatus::Failed => "failed",
    }
}

pub(crate) fn parse_run_status(s: &str) -> RunStatus {
    match s {
        "starting" => RunStatus::Starting,
        "running" => RunStatus::Running,
        "recovering" => RunStatus::Recovering,
        "stopping" => RunStatus::Stopping,
        "failed" => RunStatus::Failed,
        _ => RunStatus::Stopped,
    }
}

fn node_status_str(s: &NodeStatus) -> &'static str {
    match s {
        NodeStatus::Pending => "pending",
        NodeStatus::Starting => "starting",
        NodeStatus::HealthChecking => "health_checking",
        NodeStatus::Healthy => "healthy",
        NodeStatus::Unhealthy => "unhealthy",
        NodeStatus::Failed => "failed",
        NodeStatus::Stopped => "stopped",
        NodeStatus::Skipped => "skipped",
    }
}

fn parse_node_status(s: &str) -> NodeStatus {
    match s {
        "pending" => NodeStatus::Pending,
        "starting" => NodeStatus::Starting,
        "health_checking" => NodeStatus::HealthChecking,
        "healthy" => NodeStatus::Healthy,
        "unhealthy" => NodeStatus::Unhealthy,
        "failed" => NodeStatus::Failed,
        "skipped" => NodeStatus::Skipped,
        _ => NodeStatus::Stopped,
    }
}

/// Canonical string key for a project root path.
pub(crate) fn root_key(project_root: &Path) -> String {
    project_root.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Row assembly
// ---------------------------------------------------------------------------

fn load_nodes(conn: &Connection, run_row: i64) -> Result<HashMap<String, NodeState>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT node_key, node_name, variant, status, pid, port, url, outputs,
                readiness_phases, recovery_count, consecutive_failures,
                last_liveness_error, sensitive_keys
         FROM nodes WHERE run_row = ?1",
    )?;
    let rows = stmt.query_map([run_row], |row| {
        let key: String = row.get(0)?;
        let outputs_json: String = row.get(7)?;
        let phases_json: String = row.get(8)?;
        let sensitive_json: String = row.get(12)?;
        let status: String = row.get(3)?;
        Ok((
            key,
            NodeState {
                node_name: row.get(1)?,
                variant: row.get(2)?,
                status: parse_node_status(&status),
                pid: row.get(4)?,
                port: row.get(5)?,
                url: row.get(6)?,
                outputs: serde_json::from_str(&outputs_json).unwrap_or_default(),
                readiness_phases: serde_json::from_str::<Vec<ReadinessPhase>>(&phases_json)
                    .unwrap_or_default(),
                recovery_count: row.get(9)?,
                consecutive_failures: row.get(10)?,
                last_liveness_error: row.get(11)?,
                sensitive_keys: serde_json::from_str(&sensitive_json).unwrap_or_default(),
            },
        ))
    })?;

    let mut nodes = HashMap::new();
    for row in rows {
        let (key, mut node) = row?;
        node.decrypt_sensitive_outputs();
        nodes.insert(key, node);
    }
    Ok(nodes)
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, RunState)> {
    let row_id: i64 = row.get(0)?;
    let run_id: String = row.get(1)?;
    let name: String = row.get(2)?;
    let project: String = row.get(3)?;
    let status: String = row.get(4)?;
    let execution_order: String = row.get(5)?;
    let created_at: String = row.get(6)?;
    let stopped_at: Option<String> = row.get(7)?;
    Ok((
        row_id,
        RunState {
            run_id: Uuid::parse_str(&run_id).unwrap_or_default(),
            name,
            project,
            status: parse_run_status(&status),
            nodes: HashMap::new(),
            execution_order: serde_json::from_str(&execution_order).unwrap_or_default(),
            created_at: super::parse_ts(&created_at)
                .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
            stopped_at: stopped_at.as_deref().and_then(super::parse_ts),
        },
    ))
}

const RUN_COLS: &str = "r.id, r.run_id, r.name, p.name, r.status, r.execution_order, \
                        r.created_at, r.stopped_at";

impl Db {
    // -----------------------------------------------------------------------
    // Project state (all runs of one project)
    // -----------------------------------------------------------------------

    /// Load all runs for a project (replacement for `ProjectState::load`).
    /// Sensitive output values are decrypted after loading.
    pub fn load_project_state(&self, project_root: &Path) -> Result<ProjectState, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RUN_COLS} FROM runs r JOIN projects p ON p.root = r.project_root
             WHERE r.project_root = ?1"
        ))?;
        let runs: Vec<(i64, RunState)> = stmt
            .query_map([&root], run_from_row)?
            .collect::<Result<_, _>>()?;

        let mut state = ProjectState::default();
        for (row_id, mut run) in runs {
            run.nodes = load_nodes(&conn, row_id)?;
            state.runs.insert(run.name.clone(), run);
        }
        Ok(state)
    }

    /// Load a single run by name.
    pub fn get_run(
        &self,
        project_root: &Path,
        run_name: &str,
    ) -> Result<Option<RunState>, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RUN_COLS} FROM runs r JOIN projects p ON p.root = r.project_root
             WHERE r.project_root = ?1 AND r.name = ?2"
        ))?;
        let found = stmt
            .query_row(params![root, run_name], run_from_row)
            .optional()?;
        match found {
            Some((row_id, mut run)) => {
                run.nodes = load_nodes(&conn, row_id)?;
                Ok(Some(run))
            }
            None => Ok(None),
        }
    }

    /// Insert or replace a run (and its project row). This is the single
    /// write path — the registry is derived from the same tables, so there is
    /// no second store to update. Sensitive output values are encrypted.
    pub fn save_run(
        &self,
        project_root: &Path,
        project_name: &str,
        run: &RunState,
    ) -> Result<(), DbError> {
        let root = root_key(project_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        tx.execute(
            "INSERT INTO projects (root, name) VALUES (?1, ?2)
             ON CONFLICT(root) DO UPDATE SET name = excluded.name",
            params![root, project_name],
        )?;

        tx.execute(
            "INSERT INTO runs (project_root, name, run_id, status, execution_order, created_at, stopped_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_root, name) DO UPDATE SET
               run_id = excluded.run_id,
               status = excluded.status,
               execution_order = excluded.execution_order,
               created_at = excluded.created_at,
               stopped_at = excluded.stopped_at",
            params![
                root,
                run.name,
                run.run_id.to_string(),
                run_status_str(&run.status),
                serde_json::to_string(&run.execution_order)?,
                ts_to_str(run.created_at),
                run.stopped_at.map(ts_to_str),
            ],
        )?;

        let run_row: i64 = tx.query_row(
            "SELECT id FROM runs WHERE project_root = ?1 AND name = ?2",
            params![root, run.name],
            |r| r.get(0),
        )?;

        tx.execute("DELETE FROM nodes WHERE run_row = ?1", [run_row])?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO nodes (run_row, node_key, node_name, variant, status, pid, port, url,
                                    outputs, readiness_phases, recovery_count,
                                    consecutive_failures, last_liveness_error, sensitive_keys)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            for (key, node) in &run.nodes {
                let mut node = node.clone();
                node.encrypt_sensitive_outputs();
                stmt.execute(params![
                    run_row,
                    key,
                    node.node_name,
                    node.variant,
                    node_status_str(&node.status),
                    node.pid,
                    node.port,
                    node.url,
                    serde_json::to_string(&node.outputs)?,
                    serde_json::to_string(&node.readiness_phases)?,
                    node.recovery_count,
                    node.consecutive_failures,
                    node.last_liveness_error,
                    serde_json::to_string(&node.sensitive_keys)?,
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Remove a run. Also removes the project row when it has no runs left
    /// (mirrors the old registry behavior). Logs and feedback are kept and
    /// cleaned up separately (GC by age, stale-run cleanup on name reuse).
    pub fn remove_run(&self, project_root: &Path, run_name: &str) -> Result<(), DbError> {
        let root = root_key(project_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM runs WHERE project_root = ?1 AND name = ?2",
            params![root, run_name],
        )?;
        tx.execute(
            "DELETE FROM projects WHERE root = ?1
             AND NOT EXISTS (SELECT 1 FROM runs WHERE project_root = ?1)",
            [&root],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Remove a project and all of its runs (e.g. the project directory no
    /// longer exists on disk).
    pub fn remove_project(&self, project_root: &Path) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM projects WHERE root = ?1",
            [root_key(project_root)],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Registry (derived view over projects/runs/nodes)
    // -----------------------------------------------------------------------

    /// Assemble the global registry (replacement for `GlobalRegistry::load`).
    /// URLs are derived from node state, so the registry can never drift from
    /// the run state again.
    pub fn registry(&self) -> Result<GlobalRegistry, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT r.id, r.project_root, p.name, r.name, r.run_id, r.status
             FROM runs r JOIN projects p ON p.root = r.project_root",
        )?;
        let rows: Vec<(i64, String, String, String, String, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;

        let mut url_stmt = conn.prepare_cached(
            "SELECT node_key, url FROM nodes WHERE run_row = ?1 AND url IS NOT NULL",
        )?;

        let mut registry = GlobalRegistry::default();
        for (run_row, root, project_name, run_name, run_id, status) in rows {
            let urls: HashMap<String, String> = url_stmt
                .query_map([run_row], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<Result<_, _>>()?;
            let entry = registry
                .projects
                .entry(root.clone())
                .or_insert_with(|| RegistryEntry {
                    project_root: PathBuf::from(&root),
                    project_name: project_name.clone(),
                    runs: HashMap::new(),
                });
            entry.runs.insert(
                run_name.clone(),
                RegistryRunInfo {
                    run_id: Uuid::parse_str(&run_id).unwrap_or_default(),
                    name: run_name,
                    status: parse_run_status(&status),
                    urls,
                },
            );
        }
        Ok(registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    fn sample_run(name: &str) -> RunState {
        let mut run = RunState::new(name, "proj");
        run.status = RunStatus::Running;
        let mut node = NodeState::new("web", "local");
        node.status = NodeStatus::Healthy;
        node.pid = Some(4242);
        node.port = Some(3000);
        node.url = Some("https://web.test.veld.localhost".into());
        node.outputs.insert("token".into(), "secret-value".into());
        node.sensitive_keys = vec!["token".into()];
        node.readiness_phases.push(ReadinessPhase {
            phase: 1,
            passed: true,
            last_error: None,
            passed_at: Some(chrono::Utc::now()),
        });
        node.recovery_count = 2;
        node.consecutive_failures = 1;
        node.last_liveness_error = Some("probe timed out".into());
        run.execution_order.push("web:local".into());
        run.nodes.insert("web:local".into(), node);
        run
    }

    #[test]
    fn save_load_roundtrip() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");
        let run = sample_run("dev");
        db.save_run(root, "proj", &run).unwrap();

        let state = db.load_project_state(root).unwrap();
        let loaded = state.get_run("dev").unwrap();
        assert_eq!(loaded.run_id, run.run_id);
        assert_eq!(loaded.status, RunStatus::Running);
        assert_eq!(loaded.execution_order, vec!["web:local".to_string()]);
        // Assert EVERY NodeState field so an unwired or misaligned column
        // fails here instead of silently dropping data.
        let node = &loaded.nodes["web:local"];
        assert_eq!(node.node_name, "web");
        assert_eq!(node.variant, "local");
        assert_eq!(node.status, NodeStatus::Healthy);
        assert_eq!(node.pid, Some(4242));
        assert_eq!(node.port, Some(3000));
        assert_eq!(node.url.as_deref(), Some("https://web.test.veld.localhost"));
        assert_eq!(node.readiness_phases.len(), 1);
        assert!(node.readiness_phases[0].passed);
        assert_eq!(node.recovery_count, 2);
        assert_eq!(node.consecutive_failures, 1);
        assert_eq!(node.last_liveness_error.as_deref(), Some("probe timed out"));
        assert_eq!(node.sensitive_keys, vec!["token".to_string()]);
        // Sensitive outputs come back decrypted.
        assert_eq!(node.outputs["token"], "secret-value");

        // ...but are encrypted at rest.
        let raw: String = db
            .lock()
            .query_row("SELECT outputs FROM nodes", [], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("secret-value"));
    }

    #[test]
    fn save_is_upsert_and_remove_cleans_project() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");
        db.save_run(root, "proj", &sample_run("dev")).unwrap();
        let mut updated = sample_run("dev");
        updated.status = RunStatus::Stopped;
        db.save_run(root, "proj", &updated).unwrap();

        let state = db.load_project_state(root).unwrap();
        assert_eq!(state.runs.len(), 1);
        assert_eq!(state.runs["dev"].status, RunStatus::Stopped);

        db.remove_run(root, "dev").unwrap();
        assert!(db.load_project_state(root).unwrap().runs.is_empty());
        assert!(db.registry().unwrap().projects.is_empty());
    }

    #[test]
    fn registry_derives_urls() {
        let (_dir, db) = test_db();
        db.save_run(Path::new("/tmp/projA"), "proj", &sample_run("dev"))
            .unwrap();
        let reg = db.registry().unwrap();
        let entry = &reg.projects["/tmp/projA"];
        assert_eq!(entry.project_name, "proj");
        assert_eq!(
            entry.runs["dev"].urls["web:local"],
            "https://web.test.veld.localhost"
        );
    }

    #[test]
    fn get_run_missing_is_none() {
        let (_dir, db) = test_db();
        assert!(db.get_run(Path::new("/nope"), "dev").unwrap().is_none());
    }
}
