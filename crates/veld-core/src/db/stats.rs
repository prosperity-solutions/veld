//! Per-node process resource stats, stored in the central database.
//!
//! Written by the daemon's stats sampler once per sample interval and read by
//! the management UI (`/api/stats`) and `veld status`. Rows live in the
//! `node_stats` table (see the v2 migration) and cascade-delete with their run.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::stats::ProcessStats;

use super::state::root_key;
use super::{Db, DbError, parse_ts, ts_to_str};

/// Columns selected for a [`ProcessStats`] row, in the order [`stats_from_row`]
/// expects them (prefixed with the node key).
const STATS_COLS: &str = "node_key, cpu_percent, memory_bytes, process_count, sampled_at";

fn stats_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, ProcessStats)> {
    let node_key: String = row.get(0)?;
    let cpu: f64 = row.get(1)?;
    let mem: i64 = row.get(2)?;
    let process_count: u32 = row.get(3)?;
    let sampled: String = row.get(4)?;
    Ok((
        node_key,
        ProcessStats {
            cpu_percent: cpu as f32,
            // Stored as a signed INTEGER; clamp defensively before widening.
            memory_bytes: mem.max(0) as u64,
            process_count,
            sampled_at: parse_ts(&sampled).unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        },
    ))
}

/// Resolve the `runs.id` for a project root + run name, if the run exists.
fn run_row_id(conn: &Connection, root: &str, run_name: &str) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id FROM runs WHERE project_root = ?1 AND name = ?2",
            params![root, run_name],
            |r| r.get(0),
        )
        .optional()?)
}

impl Db {
    /// Append a batch of samples (one per node) for a run in a single
    /// transaction. A no-op when `samples` is empty or the run no longer exists
    /// (it may have been removed between the sampler reading it and this write).
    pub fn record_node_stats(
        &self,
        project_root: &Path,
        run_name: &str,
        samples: &[(String, ProcessStats)],
    ) -> Result<(), DbError> {
        if samples.is_empty() {
            return Ok(());
        }
        let root = root_key(project_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let Some(run_row) = run_row_id(&tx, &root, run_name)? else {
            // tx drops → rollback; nothing was written.
            return Ok(());
        };

        {
            // Column order after `run_row` mirrors STATS_COLS / stats_from_row;
            // keep the three in sync when adding a metric. The record/read
            // round-trip test catches a mismatch.
            let mut stmt = tx.prepare_cached(
                "INSERT INTO node_stats
                    (run_row, node_key, cpu_percent, memory_bytes, process_count, sampled_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (node_key, s) in samples {
                stmt.execute(params![
                    run_row,
                    node_key,
                    s.cpu_percent as f64,
                    s.memory_bytes as i64,
                    s.process_count,
                    ts_to_str(s.sampled_at),
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// The most recent sample for each node of a run, keyed by node key
    /// (`"node:variant"`). Empty when the run has no samples yet.
    pub fn latest_node_stats(
        &self,
        project_root: &Path,
        run_name: &str,
    ) -> Result<HashMap<String, ProcessStats>, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let Some(run_row) = run_row_id(&conn, &root, run_name)? else {
            return Ok(HashMap::new());
        };
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {STATS_COLS} FROM node_stats
             WHERE run_row = ?1
               AND sampled_at = (
                   SELECT MAX(sampled_at) FROM node_stats ns2
                   WHERE ns2.run_row = ?1 AND ns2.node_key = node_stats.node_key
               )"
        ))?;
        let rows = stmt.query_map([run_row], stats_from_row)?;
        let mut out = HashMap::new();
        for row in rows {
            let (key, stats) = row?;
            out.insert(key, stats);
        }
        Ok(out)
    }

    /// The last `limit` samples for one node, oldest-first (for sparklines).
    pub fn node_stats_history(
        &self,
        project_root: &Path,
        run_name: &str,
        node_key: &str,
        limit: usize,
    ) -> Result<Vec<ProcessStats>, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let Some(run_row) = run_row_id(&conn, &root, run_name)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {STATS_COLS} FROM node_stats
             WHERE run_row = ?1 AND node_key = ?2
             ORDER BY sampled_at DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![run_row, node_key, limit as i64], stats_from_row)?;
        let mut out: Vec<ProcessStats> =
            rows.map(|r| r.map(|(_, s)| s)).collect::<Result<_, _>>()?;
        // Query is newest-first for the LIMIT; callers want oldest-first.
        out.reverse();
        Ok(out)
    }

    /// Delete samples older than `cutoff`. Returns the number of rows removed.
    pub fn prune_node_stats_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, DbError> {
        let conn = self.lock();
        let n = conn.execute(
            "DELETE FROM node_stats WHERE sampled_at < ?1",
            params![ts_to_str(cutoff)],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::state::RunState;

    fn stat(cpu: f32, mem: u64, procs: u32, secs: i64) -> ProcessStats {
        ProcessStats {
            cpu_percent: cpu,
            memory_bytes: mem,
            process_count: procs,
            sampled_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
                + chrono::Duration::seconds(secs),
        }
    }

    #[test]
    fn record_latest_history_prune() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projStats");
        db.save_run(root, "proj", &RunState::new("dev", "proj"))
            .unwrap();

        db.record_node_stats(
            root,
            "dev",
            &[("web:local".into(), stat(10.0, 100, 3, 100))],
        )
        .unwrap();
        db.record_node_stats(
            root,
            "dev",
            &[("web:local".into(), stat(20.0, 200, 4, 200))],
        )
        .unwrap();
        db.record_node_stats(root, "dev", &[("api:local".into(), stat(5.0, 50, 1, 150))])
            .unwrap();

        let latest = db.latest_node_stats(root, "dev").unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(latest["web:local"].memory_bytes, 200);
        assert_eq!(latest["web:local"].process_count, 4);
        assert_eq!(latest["api:local"].memory_bytes, 50);

        let hist = db.node_stats_history(root, "dev", "web:local", 10).unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].memory_bytes, 100, "history is oldest-first");
        assert_eq!(hist[1].memory_bytes, 200);

        // Cutoff at t=175s removes t=100 and t=150, keeps t=200.
        let cutoff = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH + chrono::Duration::seconds(175);
        assert_eq!(db.prune_node_stats_older_than(cutoff).unwrap(), 2);
        assert_eq!(
            db.node_stats_history(root, "dev", "web:local", 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn record_unknown_run_is_noop() {
        let (_dir, db) = test_db();
        let root = Path::new("/nope");
        db.record_node_stats(root, "dev", &[("web:local".into(), stat(1.0, 1, 1, 1))])
            .unwrap();
        assert!(db.latest_node_stats(root, "dev").unwrap().is_empty());
    }

    #[test]
    fn stats_cascade_delete_with_run() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projCascade");
        db.save_run(root, "proj", &RunState::new("dev", "proj"))
            .unwrap();
        db.record_node_stats(root, "dev", &[("web:local".into(), stat(1.0, 1, 1, 1))])
            .unwrap();

        db.remove_run(root, "dev").unwrap();

        let n: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM node_stats", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "node_stats rows cascade-delete with their run");
    }
}
