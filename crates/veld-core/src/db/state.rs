//! Environment/run state + global project registry, stored in the central
//! database.
//!
//! Two concepts, split in schema v3:
//! - **environments** — the durable named slot (`(project_root, name)`), what
//!   `--name` addresses. Survives stop/start cycles.
//! - **runs** — one execution instance each, keyed by `run_id`. At most one
//!   run per environment is *live* (`starting`/`running`/`stopping`), enforced
//!   by the `idx_runs_one_live` partial unique index; ended runs accumulate as
//!   retention-bounded history.
//!
//! Ending a run is a two-phase protocol (see the RunStatus docs):
//! [`Db::begin_ending`] persists the intent (`stopping` + `end_reason`)
//! *before* any PID is killed, then [`Db::finalize_run`] moves it to its
//! terminal status once teardown is done. Crash detection collapses both
//! phases into [`Db::finalize_crashed`], guarded on `starting`/`running` only
//! — never `stopping` — so a deliberate stop can't be relabeled as a crash.
//!
//! The registry is derived from the same tables, so there is no second store
//! that can drift.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::state::{
    EndDetail, EndReason, GlobalRegistry, NodeState, NodeStatus, ProjectState, ReadinessPhase,
    RegistryEntry, RegistryRunInfo, RunState, RunStatus,
};

use super::{Db, DbError, ts_to_str};

// ---------------------------------------------------------------------------
// Status <-> TEXT
// ---------------------------------------------------------------------------

pub(crate) fn run_status_str(s: &RunStatus) -> &'static str {
    match s {
        RunStatus::Starting => "starting",
        RunStatus::Running => "running",
        RunStatus::Stopping => "stopping",
        RunStatus::Stopped => "stopped",
        RunStatus::Failed => "failed",
        RunStatus::Crashed => "crashed",
    }
}

pub(crate) fn parse_run_status(s: &str) -> RunStatus {
    match s {
        "starting" => RunStatus::Starting,
        "running" => RunStatus::Running,
        "stopping" => RunStatus::Stopping,
        "failed" => RunStatus::Failed,
        "crashed" => RunStatus::Crashed,
        _ => RunStatus::Stopped,
    }
}

fn end_reason_str(r: &EndReason) -> &'static str {
    match r {
        EndReason::Stopped => "stopped",
        EndReason::Failed => "failed",
        EndReason::Crashed => "crashed",
        EndReason::Replaced => "replaced",
        EndReason::Completed => "completed",
    }
}

fn parse_end_reason(s: &str) -> Option<EndReason> {
    match s {
        "stopped" => Some(EndReason::Stopped),
        "failed" => Some(EndReason::Failed),
        "crashed" => Some(EndReason::Crashed),
        "replaced" => Some(EndReason::Replaced),
        "completed" => Some(EndReason::Completed),
        _ => None,
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

/// SQL statuses that occupy the live slot. Must match both
/// `RunStatus::is_live` and the `idx_runs_one_live` partial index predicate.
const LIVE_SET: &str = "('starting','running','stopping')";

/// Map a unique-constraint violation on the one-live-run index to the typed
/// error; pass every other error through. SQLite reports a partial-index
/// violation by the indexed column ("UNIQUE constraint failed:
/// runs.environment_id"), not by index name — and `environment_id` appears in
/// no other unique constraint, so the match is unambiguous.
fn map_live_conflict(e: rusqlite::Error, env_name: &str) -> DbError {
    if let rusqlite::Error::SqliteFailure(ref code, ref msg) = e {
        if code.code == rusqlite::ErrorCode::ConstraintViolation
            && msg
                .as_deref()
                .is_some_and(|m| m.contains("runs.environment_id"))
        {
            return DbError::EnvironmentBusy(env_name.to_owned());
        }
    }
    e.into()
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
    let end_reason: Option<String> = row.get(5)?;
    let end_detail: Option<String> = row.get(6)?;
    let execution_order: String = row.get(7)?;
    let created_at: String = row.get(8)?;
    let ended_at: Option<String> = row.get(9)?;
    Ok((
        row_id,
        RunState {
            run_id: Uuid::parse_str(&run_id).unwrap_or_default(),
            name,
            project,
            status: parse_run_status(&status),
            end_reason: end_reason.as_deref().and_then(parse_end_reason),
            end_detail: end_detail
                .as_deref()
                .and_then(|d| serde_json::from_str::<EndDetail>(d).ok()),
            nodes: HashMap::new(),
            execution_order: serde_json::from_str(&execution_order).unwrap_or_default(),
            created_at: super::parse_ts(&created_at)
                .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
            ended_at: ended_at.as_deref().and_then(super::parse_ts),
        },
    ))
}

const RUN_COLS: &str = "r.id, r.run_id, e.name, p.name, r.status, r.end_reason, r.end_detail, \
                        r.execution_order, r.created_at, r.ended_at";
const RUN_JOIN: &str = "runs r JOIN environments e ON e.id = r.environment_id \
                        JOIN projects p ON p.root = e.project_root";
/// Correlated predicate selecting each environment's latest run (live runs
/// always sort into the latest position because a new run's `created_at` is
/// newer than every ended predecessor's).
const LATEST_PER_ENV: &str = "r.id = (SELECT r2.id FROM runs r2 \
                              WHERE r2.environment_id = r.environment_id \
                              ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1)";

impl Db {
    // -----------------------------------------------------------------------
    // Project state (latest run of each environment in one project)
    // -----------------------------------------------------------------------

    /// Load the latest run of every environment in a project.
    /// Sensitive output values are decrypted after loading.
    pub fn load_project_state(&self, project_root: &Path) -> Result<ProjectState, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RUN_COLS} FROM {RUN_JOIN}
             WHERE e.project_root = ?1 AND {LATEST_PER_ENV}"
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

    /// Load an environment's latest run by environment name.
    pub fn get_run(
        &self,
        project_root: &Path,
        run_name: &str,
    ) -> Result<Option<RunState>, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RUN_COLS} FROM {RUN_JOIN}
             WHERE e.project_root = ?1 AND e.name = ?2 AND {LATEST_PER_ENV}"
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

    /// Look up a single run by `run_id` prefix (git-style short ids). Errors
    /// on an ambiguous prefix; `Ok(None)` when nothing matches.
    pub fn get_run_by_id_prefix(
        &self,
        project_root: &Path,
        prefix: &str,
    ) -> Result<Option<RunState>, DbError> {
        // UUIDs are hex + hyphens; anything else can't match (and this keeps
        // LIKE wildcards out of the pattern without escaping gymnastics).
        if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Ok(None);
        }
        let root = root_key(project_root);
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RUN_COLS} FROM {RUN_JOIN}
             WHERE e.project_root = ?1 AND r.run_id LIKE ?2 || '%'
             ORDER BY r.created_at DESC LIMIT 2"
        ))?;
        let matches: Vec<(i64, RunState)> = stmt
            .query_map(params![root, prefix], run_from_row)?
            .collect::<Result<_, _>>()?;
        match matches.len() {
            0 => Ok(None),
            1 => {
                let (row_id, mut run) = matches.into_iter().next().unwrap();
                run.nodes = load_nodes(&conn, row_id)?;
                Ok(Some(run))
            }
            _ => Err(DbError::AmbiguousRunId(prefix.to_owned())),
        }
    }

    /// List runs (history), newest first: all of a project's, or one
    /// environment's when `run_name` is given. Nodes are loaded.
    pub fn list_runs(
        &self,
        project_root: &Path,
        run_name: Option<&str>,
    ) -> Result<Vec<RunState>, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let rows: Vec<(i64, RunState)> = match run_name {
            Some(name) => {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT {RUN_COLS} FROM {RUN_JOIN}
                     WHERE e.project_root = ?1 AND e.name = ?2
                     ORDER BY r.created_at DESC, r.id DESC"
                ))?;
                stmt.query_map(params![root, name], run_from_row)?
                    .collect::<Result<_, _>>()?
            }
            None => {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT {RUN_COLS} FROM {RUN_JOIN}
                     WHERE e.project_root = ?1
                     ORDER BY e.name ASC, r.created_at DESC, r.id DESC"
                ))?;
                stmt.query_map([&root], run_from_row)?
                    .collect::<Result<_, _>>()?
            }
        };
        let mut out = Vec::with_capacity(rows.len());
        for (row_id, mut run) in rows {
            run.nodes = load_nodes(&conn, row_id)?;
            out.push(run);
        }
        Ok(out)
    }

    /// Cheap status probe by `run_id` (no node loading) — used by log follow
    /// mode to notice its run ending.
    pub fn run_status_by_id(&self, run_id: &Uuid) -> Result<Option<RunStatus>, DbError> {
        let conn = self.lock();
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM runs WHERE run_id = ?1",
                [run_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(status.as_deref().map(parse_run_status))
    }

    /// Insert or update a run, keyed by `run_id`. This is the single write
    /// path — the registry is derived from the same tables, so there is no
    /// second store to update. Sensitive output values are encrypted.
    ///
    /// Guards:
    /// - **Terminal runs are immutable.** If the stored row is already
    ///   terminal, the whole transaction is a no-op — including the node
    ///   rewrite, so a stale read-modify-write (e.g. the monitor's liveness
    ///   pass racing a finalize) cannot overwrite an ended run's final node
    ///   states with live-era data.
    /// - **One live run per environment.** Inserting a live run while the
    ///   environment already has one fails with [`DbError::EnvironmentBusy`]
    ///   (the `idx_runs_one_live` partial unique index).
    pub fn save_run(
        &self,
        project_root: &Path,
        project_name: &str,
        run: &RunState,
    ) -> Result<(), DbError> {
        let root = root_key(project_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let stored: Option<String> = tx
            .query_row(
                "SELECT status FROM runs WHERE run_id = ?1",
                [run.run_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(status) = stored {
            if !parse_run_status(&status).is_live() {
                // Ended runs are history — never rewritten.
                tx.commit()?;
                return Ok(());
            }
        }

        tx.execute(
            "INSERT INTO projects (root, name) VALUES (?1, ?2)
             ON CONFLICT(root) DO UPDATE SET name = excluded.name",
            params![root, project_name],
        )?;

        tx.execute(
            "INSERT INTO environments (project_root, name, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(project_root, name) DO NOTHING",
            params![root, run.name, ts_to_str(run.created_at)],
        )?;
        let env_id: i64 = tx.query_row(
            "SELECT id FROM environments WHERE project_root = ?1 AND name = ?2",
            params![root, run.name],
            |r| r.get(0),
        )?;

        tx.execute(
            "INSERT INTO runs (environment_id, run_id, status, end_reason, end_detail,
                               execution_order, created_at, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(run_id) DO UPDATE SET
               -- Once `begin_ending` has moved a run to 'stopping', a stale
               -- writer (a snapshot taken before the ending began) must not
               -- pull it back into the crash detectors' scan set...
               status = CASE WHEN runs.status = 'stopping'
                             THEN runs.status ELSE excluded.status END,
               -- ...and the first ender's stored intent always wins.
               end_reason = COALESCE(runs.end_reason, excluded.end_reason),
               end_detail = COALESCE(runs.end_detail, excluded.end_detail),
               execution_order = excluded.execution_order,
               ended_at = excluded.ended_at",
            params![
                env_id,
                run.run_id.to_string(),
                run_status_str(&run.status),
                run.end_reason.as_ref().map(end_reason_str),
                run.end_detail
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                serde_json::to_string(&run.execution_order)?,
                ts_to_str(run.created_at),
                run.ended_at.map(ts_to_str),
            ],
        )
        .map_err(|e| map_live_conflict(e, &run.name))?;

        let run_row: i64 = tx.query_row(
            "SELECT id FROM runs WHERE run_id = ?1",
            [run.run_id.to_string()],
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

    // -----------------------------------------------------------------------
    // Ending protocol
    // -----------------------------------------------------------------------

    /// Phase 1 of ending a run: persist the intent *before* killing anything.
    /// Guarded on the live pre-ending statuses — the first ender wins the
    /// label; returns whether this call won. After this, the run is out of
    /// the crash detectors' scan set (they gate on `starting`/`running`).
    pub fn begin_ending(
        &self,
        run_id: &Uuid,
        reason: EndReason,
        detail: Option<&EndDetail>,
    ) -> Result<bool, DbError> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runs SET status = 'stopping', end_reason = ?2, end_detail = ?3,
                             ending_at = ?4
             WHERE run_id = ?1 AND status IN ('starting','running')",
            params![
                run_id.to_string(),
                end_reason_str(&reason),
                detail.map(serde_json::to_string).transpose()?,
                super::now_str(),
            ],
        )?;
        Ok(changed == 1)
    }

    /// Phase 2: move a `stopping` run to its terminal status (derived from the
    /// stored `end_reason`; a legacy row without one counts as `stopped`).
    /// Call once PIDs are confirmed dead and teardown has run. Returns whether
    /// this call performed the transition.
    pub fn finalize_run(&self, run_id: &Uuid) -> Result<bool, DbError> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runs SET
               status = CASE COALESCE(end_reason, 'stopped')
                          WHEN 'failed' THEN 'failed'
                          WHEN 'crashed' THEN 'crashed'
                          ELSE 'stopped' END,
               end_reason = COALESCE(end_reason, 'stopped'),
               ended_at = ?2
             WHERE run_id = ?1 AND status = 'stopping'",
            params![run_id.to_string(), super::now_str()],
        )?;
        Ok(changed == 1)
    }

    /// Crash detection: both phases in one guarded step. The guard is exactly
    /// `starting`/`running` — never `stopping` — which is what makes the
    /// protocol race-free: `begin_ending` commits while PIDs are still alive,
    /// so by the time a detector sees dead PIDs the guard finds `stopping`
    /// and no-ops. Returns whether this call performed the transition.
    pub fn finalize_crashed(
        &self,
        run_id: &Uuid,
        detail: Option<&EndDetail>,
    ) -> Result<bool, DbError> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runs SET status = 'crashed', end_reason = 'crashed',
                             end_detail = ?2, ended_at = ?3
             WHERE run_id = ?1 AND status IN ('starting','running')",
            params![
                run_id.to_string(),
                detail.map(serde_json::to_string).transpose()?,
                super::now_str(),
            ],
        )?;
        Ok(changed == 1)
    }

    /// `stopping` runs whose ending began before `cutoff` (or that predate the
    /// `ending_at` column) — candidates for the daemon's grace-gated
    /// stale-`stopping` reaper. Dead PIDs under `stopping` is the *normal*
    /// state of a healthy slow teardown, hence the grace period on both
    /// branches. Returns `(project_root, project_name, run)` with nodes loaded.
    pub fn stale_stopping_runs(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<(PathBuf, String, RunState)>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RUN_COLS}, e.project_root FROM {RUN_JOIN}
             WHERE r.status = 'stopping'
               AND (r.ending_at IS NULL OR r.ending_at < ?1)"
        ))?;
        let rows: Vec<(i64, RunState, String)> = stmt
            .query_map([ts_to_str(cutoff)], |row| {
                let (row_id, run) = run_from_row(row)?;
                let root: String = row.get(10)?;
                Ok((row_id, run, root))
            })?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (row_id, mut run, root) in rows {
            run.nodes = load_nodes(&conn, row_id)?;
            let project = run.project.clone();
            out.push((PathBuf::from(root), project, run));
        }
        Ok(out)
    }

    /// Terminal runs that still record node PIDs — targets for the GC's
    /// straggler sweep (a finalize whose kill could not be confirmed). PIDs of
    /// confirmed-dead processes are nulled at finalize time, so a recorded PID
    /// under a terminal run means "possibly still alive".
    pub fn terminal_runs_with_pids(&self) -> Result<Vec<RunState>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RUN_COLS} FROM {RUN_JOIN}
             WHERE r.status NOT IN {LIVE_SET}
               AND EXISTS (SELECT 1 FROM nodes n
                           WHERE n.run_row = r.id AND n.pid IS NOT NULL)"
        ))?;
        let rows: Vec<(i64, RunState)> = stmt
            .query_map([], run_from_row)?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (row_id, mut run) in rows {
            run.nodes = load_nodes(&conn, row_id)?;
            out.push(run);
        }
        Ok(out)
    }

    /// Null a node's recorded PID (and mark it stopped) after its death has
    /// been confirmed. Targeted update: `save_run` deliberately refuses to
    /// touch terminal runs, and this is the one legitimate post-terminal write.
    pub fn clear_node_pid(&self, run_id: &Uuid, node_key: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "UPDATE nodes SET pid = NULL, status = 'stopped'
             WHERE node_key = ?2
               AND run_row = (SELECT id FROM runs WHERE run_id = ?1)",
            params![run_id.to_string(), node_key],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Retention (called from the daemon GC pass, never from finalize — stop
    // and crash detection are latency-sensitive; an 11th history row for ten
    // minutes is harmless)
    // -----------------------------------------------------------------------

    /// Delete one ended run and its logs by `run_id`. `nodes`/`node_stats`
    /// cascade by FK; `log_lines` has no FK (logs deliberately outlive state
    /// rows) so they are deleted explicitly. Live runs are never deleted.
    /// Also drops the environment/project rows when nothing references them.
    pub fn delete_ended_run(&self, run_id: &Uuid) -> Result<bool, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let deleted = tx.execute(
            &format!("DELETE FROM runs WHERE run_id = ?1 AND status NOT IN {LIVE_SET}"),
            [run_id.to_string()],
        )?;
        if deleted == 1 {
            tx.execute(
                "DELETE FROM log_lines WHERE run_id = ?1",
                [run_id.to_string()],
            )?;
            tx.execute(
                "DELETE FROM environments WHERE NOT EXISTS
                   (SELECT 1 FROM runs WHERE environment_id = environments.id)",
                [],
            )?;
            tx.execute(
                "DELETE FROM projects WHERE NOT EXISTS
                   (SELECT 1 FROM environments WHERE project_root = projects.root)",
                [],
            )?;
        }
        tx.commit()?;
        Ok(deleted == 1)
    }

    /// Run ids of ended runs beyond the newest `keep` per environment, plus
    /// ended runs older than `cutoff` — the GC deletes each via
    /// [`Db::delete_ended_run`].
    pub fn prunable_run_ids(
        &self,
        keep: usize,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Uuid>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT run_id FROM (
                 SELECT run_id, ended_at,
                        ROW_NUMBER() OVER (PARTITION BY environment_id
                                           ORDER BY created_at DESC, id DESC) AS rn
                 FROM runs WHERE status NOT IN {LIVE_SET}
             ) WHERE rn > ?1 OR (ended_at IS NOT NULL AND ended_at < ?2)"
        ))?;
        let ids = stmt
            .query_map(params![keep as i64, ts_to_str(cutoff)], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect())
    }

    /// Remove a project and all of its environments/runs (e.g. the project
    /// directory no longer exists on disk).
    pub fn remove_project(&self, project_root: &Path) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM projects WHERE root = ?1",
            [root_key(project_root)],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Registry (derived view over projects/environments/runs/nodes)
    // -----------------------------------------------------------------------

    /// Assemble the global registry: every environment with its latest run.
    /// URLs are derived from node state, so the registry can never drift from
    /// the run state.
    pub fn registry(&self) -> Result<GlobalRegistry, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT r.id, e.project_root, p.name, e.name, r.run_id, r.status
             FROM {RUN_JOIN} WHERE {LATEST_PER_ENV}"
        ))?;
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

    /// End a live run cleanly: begin_ending + finalize.
    fn end_run(db: &Db, run: &RunState, reason: EndReason) {
        assert!(db.begin_ending(&run.run_id, reason, None).unwrap());
        assert!(db.finalize_run(&run.run_id).unwrap());
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
    fn ended_runs_accumulate_as_history() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");

        let first = sample_run("dev");
        db.save_run(root, "proj", &first).unwrap();
        end_run(&db, &first, EndReason::Stopped);

        let second = sample_run("dev");
        db.save_run(root, "proj", &second).unwrap();

        // Latest wins the name lookup; both exist in history.
        let latest = db.get_run(root, "dev").unwrap().unwrap();
        assert_eq!(latest.run_id, second.run_id);
        let history = db.list_runs(root, Some("dev")).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].run_id, second.run_id);
        assert_eq!(history[1].run_id, first.run_id);
        assert_eq!(history[1].status, RunStatus::Stopped);
        assert_eq!(history[1].end_reason, Some(EndReason::Stopped));
        assert!(history[1].ended_at.is_some());
        // The ended run's final node states survive.
        assert_eq!(history[1].nodes["web:local"].node_name, "web");
    }

    #[test]
    fn one_live_run_per_environment() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");
        db.save_run(root, "proj", &sample_run("dev")).unwrap();

        match db.save_run(root, "proj", &sample_run("dev")) {
            Err(DbError::EnvironmentBusy(name)) => assert_eq!(name, "dev"),
            other => panic!("expected EnvironmentBusy, got {other:?}"),
        }

        // A different environment is unaffected.
        db.save_run(root, "proj", &sample_run("staging")).unwrap();
    }

    #[test]
    fn terminal_runs_are_immutable() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");
        let run = sample_run("dev");
        db.save_run(root, "proj", &run).unwrap();
        end_run(&db, &run, EndReason::Stopped);

        // A stale writer (e.g. the monitor holding a pre-finalize snapshot)
        // must not resurrect the run or rewrite its final node states.
        let mut stale = run.clone();
        stale.status = RunStatus::Running;
        stale.nodes.get_mut("web:local").unwrap().pid = Some(9999);
        db.save_run(root, "proj", &stale).unwrap();

        let stored = db.get_run(root, "dev").unwrap().unwrap();
        assert_eq!(stored.status, RunStatus::Stopped);
        assert_eq!(stored.nodes["web:local"].pid, Some(4242));
    }

    #[test]
    fn ending_protocol_first_ender_wins() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");
        let run = sample_run("dev");
        db.save_run(root, "proj", &run).unwrap();

        assert!(
            db.begin_ending(&run.run_id, EndReason::Stopped, None)
                .unwrap()
        );
        // Second ender (e.g. replaced-path) loses.
        assert!(
            !db.begin_ending(&run.run_id, EndReason::Replaced, None)
                .unwrap()
        );
        // Crash detection no-ops against a stopping run.
        assert!(!db.finalize_crashed(&run.run_id, None).unwrap());

        assert!(db.finalize_run(&run.run_id).unwrap());
        assert!(!db.finalize_run(&run.run_id).unwrap());
        let stored = db.get_run(root, "dev").unwrap().unwrap();
        assert_eq!(stored.status, RunStatus::Stopped);
        assert_eq!(stored.end_reason, Some(EndReason::Stopped));
    }

    #[test]
    fn crash_detection_labels_crashed() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");
        let run = sample_run("dev");
        db.save_run(root, "proj", &run).unwrap();

        let detail = EndDetail {
            failed_node: Some("web:local".into()),
            ..Default::default()
        };
        assert!(db.finalize_crashed(&run.run_id, Some(&detail)).unwrap());
        let stored = db.get_run(root, "dev").unwrap().unwrap();
        assert_eq!(stored.status, RunStatus::Crashed);
        assert_eq!(stored.end_reason, Some(EndReason::Crashed));
        assert_eq!(
            stored.end_detail.unwrap().failed_node.as_deref(),
            Some("web:local")
        );
        assert!(stored.ended_at.is_some());
    }

    #[test]
    fn run_id_prefix_lookup() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");
        let run = sample_run("dev");
        db.save_run(root, "proj", &run).unwrap();

        let short = run.short_id();
        let found = db.get_run_by_id_prefix(root, &short).unwrap().unwrap();
        assert_eq!(found.run_id, run.run_id);
        assert!(db.get_run_by_id_prefix(root, "zzzz").unwrap().is_none());
    }

    #[test]
    fn retention_prunes_beyond_cap_and_age() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");

        let mut ended: Vec<RunState> = Vec::new();
        for _ in 0..4 {
            let run = sample_run("dev");
            db.save_run(root, "proj", &run).unwrap();
            end_run(&db, &run, EndReason::Stopped);
            ended.push(run);
        }

        // Cap 2: the two oldest are prunable.
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(168);
        let prunable = db.prunable_run_ids(2, cutoff).unwrap();
        assert_eq!(prunable.len(), 2);
        for id in &prunable {
            assert!(db.delete_ended_run(id).unwrap());
        }
        assert_eq!(db.list_runs(root, Some("dev")).unwrap().len(), 2);

        // A live run is never deleted, even when passed explicitly.
        let live = sample_run("dev");
        db.save_run(root, "proj", &live).unwrap();
        assert!(!db.delete_ended_run(&live.run_id).unwrap());
    }

    #[test]
    fn delete_last_run_drops_environment_and_project() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projA");
        let run = sample_run("dev");
        db.save_run(root, "proj", &run).unwrap();
        end_run(&db, &run, EndReason::Stopped);

        assert!(db.delete_ended_run(&run.run_id).unwrap());
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
