//! Engine actions: one row per start/stop/restart/action the engine was asked to
//! carry out, and the events it reported while doing it.
//!
//! The engine runs each action in its own worker process (`veld-daemon
//! --engine-worker`) so that a daemon restart or a `veld update` does not cut it
//! in half. These tables are how the pieces find each other across that
//! restart: the worker appends events and finally its outcome; the daemon (old
//! or new) streams them to whichever face asked; a face that lost its stream
//! picks it up again from the last `seq` it saw.
//!
//! **Nothing secret is stored here.** The request is the persisted half of an
//! action; the caller's env values and var answers go to the worker on its stdin
//! and nowhere else. See `veld_engine::api::ActionSecrets`.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, DbError, now_str};

/// An action's lifecycle. `running` until the worker (or the daemon, for a
/// worker that died) writes the end.
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_SUCCEEDED: &str = "succeeded";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_CANCELLED: &str = "cancelled";
/// The worker vanished without saying how it ended.
pub const STATUS_LOST: &str = "lost";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineActionRow {
    pub id: String,
    pub project_root: String,
    /// Empty for an action about no single run (`stop --all`).
    pub run_name: String,
    pub kind: String,
    pub request_json: String,
    pub status: String,
    pub outcome_json: Option<String>,
    pub worker_pid: Option<u32>,
    pub created_at: String,
    pub ended_at: Option<String>,
}

const COLUMNS: &str = "id, project_root, run_name, kind, request_json, status, outcome_json, \
                       worker_pid, created_at, ended_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EngineActionRow> {
    Ok(EngineActionRow {
        id: r.get(0)?,
        project_root: r.get(1)?,
        run_name: r.get(2)?,
        kind: r.get(3)?,
        request_json: r.get(4)?,
        status: r.get(5)?,
        outcome_json: r.get(6)?,
        worker_pid: r.get::<_, Option<i64>>(7)?.map(|p| p as u32),
        created_at: r.get(8)?,
        ended_at: r.get(9)?,
    })
}

impl Db {
    /// Record a new, running action.
    pub fn insert_engine_action(
        &self,
        id: &str,
        project_root: &str,
        run_name: &str,
        kind: &str,
        request_json: &str,
    ) -> Result<(), DbError> {
        self.lock().execute(
            "INSERT INTO engine_actions
                 (id, project_root, run_name, kind, request_json, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                project_root,
                run_name,
                kind,
                request_json,
                STATUS_RUNNING,
                now_str()
            ],
        )?;
        Ok(())
    }

    pub fn set_engine_action_pid(&self, id: &str, pid: u32) -> Result<(), DbError> {
        self.lock().execute(
            "UPDATE engine_actions SET worker_pid = ?2 WHERE id = ?1",
            params![id, pid as i64],
        )?;
        Ok(())
    }

    /// Append one event; returns its `seq`.
    pub fn append_engine_event(&self, action_id: &str, json: &str) -> Result<i64, DbError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO engine_action_events (action_id, json, ts) VALUES (?1, ?2, ?3)",
            params![action_id, json, now_str()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Events of one action after `after`, oldest first, at most `limit`.
    pub fn engine_events_after(
        &self,
        action_id: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String)>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT seq, json FROM engine_action_events
             WHERE action_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![action_id, after, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// End a running action. **Guarded**: only a row still `running` changes, so
    /// a worker that finished and a daemon that concluded it was lost cannot
    /// overwrite each other. Returns whether this call was the one that ended it.
    pub fn finish_engine_action(
        &self,
        id: &str,
        status: &str,
        outcome_json: &str,
    ) -> Result<bool, DbError> {
        let n = self.lock().execute(
            "UPDATE engine_actions SET status = ?2, outcome_json = ?3, ended_at = ?4
             WHERE id = ?1 AND status = ?5",
            params![id, status, outcome_json, now_str(), STATUS_RUNNING],
        )?;
        Ok(n > 0)
    }

    /// End a running action and append its final event **in one transaction**.
    ///
    /// A stream reader decides an action is over when its status is no longer
    /// `running` and it has read every event. Written as two statements, a reader
    /// that looks between them sees "over" without the final event and ends the
    /// stream with no outcome — a face then reports a failure for an action that
    /// succeeded. Guarded like [`Self::finish_engine_action`]: only the call that
    /// ends a still-running action writes, so a worker finishing and a daemon
    /// declaring it lost cannot both append a final event.
    pub fn finish_engine_action_with_event(
        &self,
        id: &str,
        status: &str,
        outcome_json: &str,
        event_json: &str,
    ) -> Result<bool, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let ended_at = now_str();
        let n = tx.execute(
            "UPDATE engine_actions SET status = ?2, outcome_json = ?3, ended_at = ?4
             WHERE id = ?1 AND status = ?5",
            params![id, status, outcome_json, ended_at, STATUS_RUNNING],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO engine_action_events (action_id, json, ts) VALUES (?1, ?2, ?3)",
                params![id, event_json, ended_at],
            )?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn engine_action(&self, id: &str) -> Result<Option<EngineActionRow>, DbError> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM engine_actions WHERE id = ?1"),
                params![id],
                row,
            )
            .optional()?)
    }

    /// Every action still marked running, oldest first.
    pub fn running_engine_actions(&self) -> Result<Vec<EngineActionRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM engine_actions WHERE status = ?1 ORDER BY created_at"
        ))?;
        let rows = stmt
            .query_map(params![STATUS_RUNNING], row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Drop finished actions (and their events) that ended before `cutoff`
    /// (RFC 3339). Returns how many actions went.
    pub fn prune_engine_actions(&self, cutoff: &str) -> Result<usize, DbError> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM engine_action_events WHERE action_id IN
                 (SELECT id FROM engine_actions WHERE status != ?1 AND ended_at < ?2)",
            params![STATUS_RUNNING, cutoff],
        )?;
        let n = conn.execute(
            "DELETE FROM engine_actions WHERE status != ?1 AND ended_at < ?2",
            params![STATUS_RUNNING, cutoff],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    #[test]
    fn events_resume_from_a_seq_and_finish_is_single_winner() {
        let (_dir, db) = test_db();
        db.insert_engine_action("a1", "/p", "dev", "start", "{}")
            .unwrap();
        let s1 = db.append_engine_event("a1", "\"one\"").unwrap();
        let s2 = db.append_engine_event("a1", "\"two\"").unwrap();
        assert!(s2 > s1);
        let after = db.engine_events_after("a1", s1, 10).unwrap();
        assert_eq!(after, vec![(s2, "\"two\"".to_owned())]);

        assert!(
            db.finish_engine_action("a1", STATUS_SUCCEEDED, "{}")
                .unwrap()
        );
        assert!(!db.finish_engine_action("a1", STATUS_LOST, "{}").unwrap());
        let row = db.engine_action("a1").unwrap().unwrap();
        assert_eq!(row.status, STATUS_SUCCEEDED);
        assert!(db.running_engine_actions().unwrap().is_empty());
    }

    #[test]
    fn prune_keeps_running_actions() {
        let (_dir, db) = test_db();
        db.insert_engine_action("live", "/p", "dev", "start", "{}")
            .unwrap();
        db.insert_engine_action("done", "/p", "dev", "stop", "{}")
            .unwrap();
        db.append_engine_event("done", "1").unwrap();
        db.finish_engine_action("done", STATUS_SUCCEEDED, "{}")
            .unwrap();
        assert_eq!(db.prune_engine_actions("9999-01-01T00:00:00Z").unwrap(), 1);
        assert!(db.engine_action("live").unwrap().is_some());
        assert!(db.engine_events_after("done", 0, 10).unwrap().is_empty());
    }
}
