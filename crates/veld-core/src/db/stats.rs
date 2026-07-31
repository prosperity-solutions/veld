//! Per-node process resource stats, stored in the central database.
//!
//! Written by the daemon's stats sampler once per sample interval and read by
//! the management UI (`/api/stats`, `/api/stats/history`), `veld status` and
//! `veld stats`. Two tables, both cascade-deleting with their run:
//!
//! - `node_stats` — one row per node per sample: the tree aggregate (see the v2
//!   migration for the table, v7 for the memory-breakdown columns).
//! - `node_process_stats` — one row per *process* per sample (v7), so a
//!   subprocess can be graphed on its own.
//!
//! Readers come in two shapes. `latest_*` answers "what is it doing now" and is
//! what a 5s poll hits. The `*_buckets` queries answer "what has it been doing"
//! by averaging inside fixed-width time buckets **in SQL** — the payload size
//! then depends on the requested resolution rather than on how long the run has
//! been up, which is what makes a scrubbable six-hour graph affordable over a
//! 5s-cadence table.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::stats::{
    MemoryBreakdown, ProcessSample, ProcessSeries, ProcessStats, StatsBucket, StatsWindow,
};

use super::state::root_key;
use super::{Db, DbError, parse_ts, ts_to_str};

/// Columns selected for a [`ProcessStats`] row, in the order [`stats_from_row`]
/// expects them (prefixed with the node key).
const STATS_COLS: &str = "node_key, cpu_percent, memory_bytes, process_count, sampled_at, \
     cpu_seconds, footprint, virtual_bytes, private_clean, private_dirty, shared_clean, \
     shared_dirty, swap, wired";

/// Columns selected for a [`ProcessSample`] row, in the order
/// [`process_from_row`] expects them.
const PROC_COLS: &str = "pid, parent_pid, depth, name, cmd, cpu_percent, cpu_seconds, \
     memory_bytes, footprint, virtual_bytes, private_clean, private_dirty, shared_clean, \
     shared_dirty, swap, wired, started_at";

/// Read a nullable byte count. Stored as a signed INTEGER, so clamp before
/// widening — the same defensive `max(0)` the RSS column has always had.
fn opt_bytes(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<u64>> {
    Ok(row.get::<_, Option<i64>>(idx)?.map(|v| v.max(0) as u64))
}

fn stats_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, ProcessStats)> {
    let node_key: String = row.get(0)?;
    let cpu: f64 = row.get(1)?;
    let mem: i64 = row.get(2)?;
    let process_count: u32 = row.get(3)?;
    let sampled: String = row.get(4)?;
    let memory_bytes = mem.max(0) as u64;
    Ok((
        node_key,
        ProcessStats {
            cpu_percent: cpu as f32,
            // Stored as a signed INTEGER; clamp defensively before widening.
            memory_bytes,
            process_count,
            cpu_seconds: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            memory: MemoryBreakdown {
                // Pre-v7 rows have no footprint; fall back to RSS so an old
                // sample plots on the default metric instead of dropping the
                // line to zero mid-graph.
                footprint: opt_bytes(row, 6)?.unwrap_or(memory_bytes),
                virtual_bytes: opt_bytes(row, 7)?.unwrap_or(0),
                private_clean: opt_bytes(row, 8)?,
                private_dirty: opt_bytes(row, 9)?,
                shared_clean: opt_bytes(row, 10)?,
                shared_dirty: opt_bytes(row, 11)?,
                swap: opt_bytes(row, 12)?,
                wired: opt_bytes(row, 13)?,
            },
            sampled_at: parse_ts(&sampled).unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        },
    ))
}

fn process_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProcessSample> {
    let memory_bytes = row.get::<_, i64>(7)?.max(0) as u64;
    Ok(ProcessSample {
        pid: row.get(0)?,
        parent_pid: row.get(1)?,
        depth: row.get(2)?,
        name: row.get(3)?,
        cmd: row.get(4)?,
        cpu_percent: row.get::<_, f64>(5)? as f32,
        cpu_seconds: row.get(6)?,
        memory_bytes,
        memory: MemoryBreakdown {
            footprint: opt_bytes(row, 8)?.unwrap_or(memory_bytes),
            virtual_bytes: opt_bytes(row, 9)?.unwrap_or(0),
            private_clean: opt_bytes(row, 10)?,
            private_dirty: opt_bytes(row, 11)?,
            shared_clean: opt_bytes(row, 12)?,
            shared_dirty: opt_bytes(row, 13)?,
            swap: opt_bytes(row, 14)?,
            wired: opt_bytes(row, 15)?,
        },
        started_at: row.get::<_, Option<String>>(16)?.and_then(|s| parse_ts(&s)),
    })
}

/// The aggregate expressions shared by both bucket queries, in the order
/// [`bucket_from_row`] reads them (after `bucket` and `samples`).
///
/// `CASE WHEN COUNT(x) = COUNT(*) THEN AVG(x) END` rather than a plain `AVG(x)`:
/// SQL's `AVG` skips NULLs, so a bucket where only some samples carried a page
/// class would average the ones that did and present the result as the whole
/// bucket's figure. An absent field must stay absent (the same rule
/// [`MemoryBreakdown::add`] enforces across a process tree) — otherwise a graph
/// silently understates itself across the v7 upgrade boundary or a
/// `VELD_STATS_MEMORY_DETAIL=off` toggle.
const BUCKET_AGGS_TAIL: &str = "AVG(memory_bytes), \
     AVG(COALESCE(footprint, memory_bytes)), MAX(COALESCE(footprint, memory_bytes)), \
     AVG(virtual_bytes), \
     CASE WHEN COUNT(private_clean) = COUNT(*) THEN AVG(private_clean) END, \
     CASE WHEN COUNT(private_dirty) = COUNT(*) THEN AVG(private_dirty) END, \
     CASE WHEN COUNT(shared_clean) = COUNT(*) THEN AVG(shared_clean) END, \
     CASE WHEN COUNT(shared_dirty) = COUNT(*) THEN AVG(shared_dirty) END, \
     CASE WHEN COUNT(swap) = COUNT(*) THEN AVG(swap) END, \
     CASE WHEN COUNT(wired) = COUNT(*) THEN AVG(wired) END";

/// [`BUCKET_AGGS_TAIL`] with the process-count slot filled in, in the order
/// [`bucket_from_row`] reads them.
///
/// The two tables differ in exactly this one column: `node_stats` stores the
/// tree's process count, while a `node_process_stats` row *is* one process, so
/// its count is the literal 1. Everything else is shared, which is the point —
/// the two queries must aggregate memory identically or the aggregate graph and
/// the per-process graph would disagree about the same bytes.
fn bucket_aggs(process_count_expr: &str) -> String {
    format!("AVG(cpu_percent), MAX(cpu_percent), {process_count_expr}, {BUCKET_AGGS_TAIL}")
}

/// Read one aggregated bucket. `idx0` is the column index of the bucket ordinal;
/// `samples` follows it, then [`BUCKET_AGGS`] in order.
fn bucket_from_row(
    row: &rusqlite::Row<'_>,
    window_start: DateTime<Utc>,
    bucket_secs: i64,
) -> rusqlite::Result<StatsBucket> {
    let bucket: i64 = row.get(0)?;
    let samples: u32 = row.get(1)?;
    // AVG returns REAL even over INTEGER columns; round rather than truncate so
    // a steady 100-byte series doesn't read as 99.
    fn avg_bytes(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<u64>> {
        Ok(row
            .get::<_, Option<f64>>(idx)?
            .map(|v| v.max(0.0).round() as u64))
    }
    let memory_bytes = avg_bytes(row, 5)?.unwrap_or(0);
    Ok(StatsBucket {
        bucket_start: window_start + chrono::Duration::seconds(bucket * bucket_secs),
        samples,
        cpu_percent: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0) as f32,
        cpu_peak: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0) as f32,
        process_count: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0) as f32,
        memory_bytes,
        memory: MemoryBreakdown {
            footprint: avg_bytes(row, 6)?.unwrap_or(memory_bytes),
            virtual_bytes: avg_bytes(row, 8)?.unwrap_or(0),
            private_clean: avg_bytes(row, 9)?,
            private_dirty: avg_bytes(row, 10)?,
            shared_clean: avg_bytes(row, 11)?,
            shared_dirty: avg_bytes(row, 12)?,
            swap: avg_bytes(row, 13)?,
            wired: avg_bytes(row, 14)?,
        },
        footprint_peak: avg_bytes(row, 7)?.unwrap_or(memory_bytes),
    })
}

/// Resolve the `runs.id` of an environment's latest run, if one exists —
/// samples always describe the live (= latest) instance.
fn run_row_id(conn: &Connection, root: &str, run_name: &str) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT r.id FROM runs r
             JOIN environments e ON e.id = r.environment_id
             WHERE e.project_root = ?1 AND e.name = ?2
             ORDER BY r.id DESC LIMIT 1",
            params![root, run_name],
            |r| r.get(0),
        )
        .optional()?)
}

impl Db {
    /// Append a batch of samples (one per node) for a run in a single
    /// transaction. A no-op when `samples` is empty or the run no longer exists
    /// (it may have been removed between the sampler reading it and this write).
    ///
    /// `trees` carries the per-process rows for the same instant, keyed by node
    /// key. It is written in the same transaction as the aggregates, so a reader
    /// never sees a tree total whose processes haven't landed yet (or the
    /// reverse — which would render as a node that suddenly has no processes).
    pub fn record_node_stats(
        &self,
        project_root: &Path,
        run_name: &str,
        samples: &[(String, ProcessStats)],
        trees: &[(String, Vec<ProcessSample>)],
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
                    (run_row, node_key, cpu_percent, memory_bytes, process_count, sampled_at,
                     cpu_seconds, footprint, virtual_bytes, private_clean, private_dirty,
                     shared_clean, shared_dirty, swap, wired)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            for (node_key, s) in samples {
                stmt.execute(params![
                    run_row,
                    node_key,
                    s.cpu_percent as f64,
                    s.memory_bytes as i64,
                    s.process_count,
                    ts_to_str(s.sampled_at),
                    s.cpu_seconds,
                    s.memory.footprint as i64,
                    s.memory.virtual_bytes as i64,
                    s.memory.private_clean.map(|v| v as i64),
                    s.memory.private_dirty.map(|v| v as i64),
                    s.memory.shared_clean.map(|v| v as i64),
                    s.memory.shared_dirty.map(|v| v as i64),
                    s.memory.swap.map(|v| v as i64),
                    s.memory.wired.map(|v| v as i64),
                ])?;
            }
        }

        if !trees.is_empty() {
            // Every tree shares its node's `sampled_at` — look it up from the
            // aggregate rather than re-reading the clock, so the per-process
            // rows group with the aggregate they came from.
            let sampled_at: HashMap<&str, String> = samples
                .iter()
                .map(|(k, s)| (k.as_str(), ts_to_str(s.sampled_at)))
                .collect();
            let mut stmt = tx.prepare_cached(
                "INSERT INTO node_process_stats
                    (run_row, node_key, sampled_at, pid, parent_pid, depth, name, cmd,
                     cpu_percent, cpu_seconds, memory_bytes, footprint, virtual_bytes,
                     private_clean, private_dirty, shared_clean, shared_dirty, swap, wired,
                     started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19, ?20)",
            )?;
            for (node_key, procs) in trees {
                let Some(ts) = sampled_at.get(node_key.as_str()) else {
                    // A tree with no matching aggregate would be unreachable by
                    // every reader (they join on the aggregate's timestamp).
                    continue;
                };
                for p in procs {
                    stmt.execute(params![
                        run_row,
                        node_key,
                        ts,
                        p.pid,
                        p.parent_pid,
                        p.depth,
                        p.name,
                        p.cmd,
                        p.cpu_percent as f64,
                        p.cpu_seconds,
                        p.memory_bytes as i64,
                        p.memory.footprint as i64,
                        p.memory.virtual_bytes as i64,
                        p.memory.private_clean.map(|v| v as i64),
                        p.memory.private_dirty.map(|v| v as i64),
                        p.memory.shared_clean.map(|v| v as i64),
                        p.memory.shared_dirty.map(|v| v as i64),
                        p.memory.swap.map(|v| v as i64),
                        p.memory.wired.map(|v| v as i64),
                        p.started_at.map(ts_to_str),
                    ])?;
                }
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

    /// The processes of one node's most recent sample, in the pre-order the
    /// sampler recorded them (parents before children). Empty when the node has
    /// no per-process rows — either it predates v7 or detail is switched off.
    pub fn latest_process_tree(
        &self,
        project_root: &Path,
        run_name: &str,
        node_key: &str,
    ) -> Result<Vec<ProcessSample>, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let Some(run_row) = run_row_id(&conn, &root, run_name)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {PROC_COLS} FROM node_process_stats
             WHERE run_row = ?1 AND node_key = ?2
               AND sampled_at = (
                   SELECT MAX(sampled_at) FROM node_process_stats p2
                   WHERE p2.run_row = ?1 AND p2.node_key = ?2
               )
             ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![run_row, node_key], process_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One node's aggregate history over `window`, averaged into buckets.
    ///
    /// Buckets with no samples are **omitted, not zero-filled** — "the daemon
    /// wasn't sampling" and "the node used no memory" are different facts and a
    /// graph that renders them the same invents a drop to zero that never
    /// happened. Renderers break the line across the gap instead.
    pub fn node_stats_buckets(
        &self,
        project_root: &Path,
        run_name: &str,
        node_key: &str,
        window: StatsWindow,
    ) -> Result<Vec<StatsBucket>, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let Some(run_row) = run_row_id(&conn, &root, run_name)? else {
            return Ok(Vec::new());
        };
        let start_epoch = window.start.timestamp();
        let aggs = bucket_aggs("AVG(process_count)");
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT CAST((strftime('%s', sampled_at) - ?3) / ?4 AS INTEGER) AS bucket,
                    COUNT(*), {aggs}
             FROM node_stats
             WHERE run_row = ?1 AND node_key = ?2
               AND sampled_at >= ?5 AND sampled_at < ?6
             GROUP BY bucket
             ORDER BY bucket"
        ))?;
        let rows = stmt.query_map(
            params![
                run_row,
                node_key,
                start_epoch,
                window.bucket_secs,
                ts_to_str(window.start),
                ts_to_str(window.end),
            ],
            |row| bucket_from_row(row, window.start, window.bucket_secs),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Per-process history over `window`, one series per PID that appears in it.
    ///
    /// Series are ordered by peak footprint descending, so a caller that keeps
    /// the first N keeps the processes that matter. A PID whose process exited
    /// mid-window simply has no buckets after that point — PIDs are not stable
    /// identities (see [`ProcessSample`]), so a renderer must break the series
    /// rather than interpolate across the gap.
    pub fn process_stats_buckets(
        &self,
        project_root: &Path,
        run_name: &str,
        node_key: &str,
        window: StatsWindow,
    ) -> Result<Vec<ProcessSeries>, DbError> {
        let root = root_key(project_root);
        let conn = self.lock();
        let Some(run_row) = run_row_id(&conn, &root, run_name)? else {
            return Ok(Vec::new());
        };
        let start_epoch = window.start.timestamp();
        let start_ts = ts_to_str(window.start);
        let end_ts = ts_to_str(window.end);

        // Names first, one row per PID. A single MAX() aggregate with bare
        // columns is the one case where SQLite guarantees the bare values come
        // from the row that produced the max — so `name`/`cmd` are the *last*
        // ones observed for that PID, which is what a reused PID should report.
        let mut names: HashMap<u32, (String, Option<String>)> = HashMap::new();
        {
            let mut stmt = conn.prepare_cached(
                "SELECT pid, name, cmd, MAX(sampled_at) FROM node_process_stats
                 WHERE run_row = ?1 AND node_key = ?2
                   AND sampled_at >= ?3 AND sampled_at < ?4
                 GROUP BY pid",
            )?;
            let rows = stmt.query_map(params![run_row, node_key, start_ts, end_ts], |row| {
                Ok((row.get::<_, u32>(0)?, row.get(1)?, row.get(2)?))
            })?;
            for row in rows {
                let (pid, name, cmd) = row?;
                names.insert(pid, (name, cmd));
            }
        }
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let mut per_pid: HashMap<u32, Vec<StatsBucket>> = HashMap::new();
        {
            // A per-process row is one process, so the count slot is a literal 1
            // rather than a stored column.
            let aggs = bucket_aggs("1.0");
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT CAST((strftime('%s', sampled_at) - ?3) / ?4 AS INTEGER) AS bucket,
                        COUNT(*), {aggs}, pid
                 FROM node_process_stats
                 WHERE run_row = ?1 AND node_key = ?2
                   AND sampled_at >= ?5 AND sampled_at < ?6
                 GROUP BY bucket, pid
                 ORDER BY bucket"
            ))?;
            let rows = stmt.query_map(
                params![
                    run_row,
                    node_key,
                    start_epoch,
                    window.bucket_secs,
                    start_ts,
                    end_ts,
                ],
                |row| {
                    // `pid` is appended after BUCKET_AGGS (2 leading + 13 aggs).
                    let pid: u32 = row.get(15)?;
                    Ok((pid, bucket_from_row(row, window.start, window.bucket_secs)?))
                },
            )?;
            for row in rows {
                let (pid, bucket) = row?;
                per_pid.entry(pid).or_default().push(bucket);
            }
        }

        let mut out: Vec<ProcessSeries> = per_pid
            .into_iter()
            .map(|(pid, buckets)| {
                let (name, cmd) = names
                    .remove(&pid)
                    .unwrap_or_else(|| (format!("pid {pid}"), None));
                ProcessSeries {
                    pid,
                    name,
                    cmd,
                    buckets,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            let peak = |s: &ProcessSeries| s.buckets.iter().map(|b| b.footprint_peak).max();
            peak(b)
                .cmp(&peak(a))
                // PID as the tiebreak keeps the order stable across polls, so a
                // colour assigned to a series doesn't hop between processes.
                .then(a.pid.cmp(&b.pid))
        });
        Ok(out)
    }

    /// Delete aggregate samples older than `cutoff`. Returns rows removed.
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

    /// Delete per-process samples older than `cutoff`. Returns rows removed.
    ///
    /// Pruned on a shorter horizon than the aggregates (see the GC's
    /// `MAX_PROCESS_STATS_AGE_HOURS`): these rows outnumber them by the tree
    /// size, and a per-subprocess breakdown answers "what is happening" rather
    /// than "what happened yesterday".
    pub fn prune_node_process_stats_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, DbError> {
        let conn = self.lock();
        let n = conn.execute(
            "DELETE FROM node_process_stats WHERE sampled_at < ?1",
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
    use crate::stats::MemoryMetric;

    fn at(secs: i64) -> DateTime<Utc> {
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH + chrono::Duration::seconds(secs)
    }

    fn stat(cpu: f32, mem: u64, procs: u32, secs: i64) -> ProcessStats {
        ProcessStats {
            cpu_percent: cpu,
            memory_bytes: mem,
            process_count: procs,
            cpu_seconds: cpu as f64 * 2.0,
            memory: MemoryBreakdown {
                footprint: mem / 2,
                virtual_bytes: mem * 10,
                private_clean: Some(mem / 8),
                private_dirty: Some(mem / 4),
                shared_clean: Some(mem / 8),
                shared_dirty: Some(mem / 16),
                swap: Some(0),
                wired: Some(0),
            },
            sampled_at: at(secs),
        }
    }

    fn proc_sample(pid: u32, parent: Option<u32>, name: &str, mem: u64) -> ProcessSample {
        ProcessSample {
            pid,
            parent_pid: parent,
            depth: if parent.is_some() { 1 } else { 0 },
            name: name.to_owned(),
            cmd: Some(format!("{name} --flag")),
            cpu_percent: 1.5,
            cpu_seconds: 3.0,
            memory_bytes: mem,
            memory: MemoryBreakdown {
                footprint: mem / 2,
                virtual_bytes: mem * 10,
                private_dirty: Some(mem / 4),
                ..Default::default()
            },
            started_at: Some(at(10)),
        }
    }

    /// Set up a run with `web:local` samples at the given (mem, secs) points.
    fn seeded(root: &Path, points: &[(u64, i64)]) -> (tempfile::TempDir, Db) {
        let (dir, db) = test_db();
        db.save_run(root, "proj", &RunState::new("dev", "proj"))
            .unwrap();
        for (mem, secs) in points {
            db.record_node_stats(
                root,
                "dev",
                &[("web:local".into(), stat(10.0, *mem, 2, *secs))],
                &[(
                    "web:local".into(),
                    vec![
                        proc_sample(100, None, "node", mem / 2),
                        proc_sample(101, Some(100), "esbuild", mem / 2),
                    ],
                )],
            )
            .unwrap();
        }
        (dir, db)
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
            &[],
        )
        .unwrap();
        db.record_node_stats(
            root,
            "dev",
            &[("web:local".into(), stat(20.0, 200, 4, 200))],
            &[],
        )
        .unwrap();
        db.record_node_stats(
            root,
            "dev",
            &[("api:local".into(), stat(5.0, 50, 1, 150))],
            &[],
        )
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
        let cutoff = at(175);
        assert_eq!(db.prune_node_stats_older_than(cutoff).unwrap(), 2);
        assert_eq!(
            db.node_stats_history(root, "dev", "web:local", 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn breakdown_round_trips() {
        let root = Path::new("/tmp/projBreakdown");
        let (_dir, db) = seeded(root, &[(4096, 100)]);
        let latest = db.latest_node_stats(root, "dev").unwrap();
        let s = &latest["web:local"];
        assert_eq!(s.memory.footprint, 2048);
        assert_eq!(s.memory.virtual_bytes, 40960);
        assert_eq!(s.memory.private_dirty, Some(1024));
        assert_eq!(s.memory.swap, Some(0), "a real zero is not an absence");
        assert_eq!(s.cpu_seconds, 20.0);
        assert_eq!(s.memory_metric(MemoryMetric::Footprint), Some(2048));
    }

    #[test]
    fn process_rows_round_trip_in_preorder() {
        let root = Path::new("/tmp/projProcs");
        let (_dir, db) = seeded(root, &[(4096, 100)]);
        let tree = db.latest_process_tree(root, "dev", "web:local").unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].pid, 100, "parents before children");
        assert_eq!(tree[0].depth, 0);
        assert_eq!(tree[1].pid, 101);
        assert_eq!(tree[1].parent_pid, Some(100));
        assert_eq!(tree[1].cmd.as_deref(), Some("esbuild --flag"));
        assert_eq!(tree[1].memory.private_dirty, Some(512));
        assert_eq!(tree[1].started_at, Some(at(10)));
        // Only the newest sample's processes come back.
        db.record_node_stats(
            root,
            "dev",
            &[("web:local".into(), stat(10.0, 8192, 1, 200))],
            &[(
                "web:local".into(),
                vec![proc_sample(100, None, "node", 8192)],
            )],
        )
        .unwrap();
        let tree = db.latest_process_tree(root, "dev", "web:local").unwrap();
        assert_eq!(tree.len(), 1, "latest sample only");
    }

    #[test]
    fn buckets_average_and_omit_gaps() {
        let root = Path::new("/tmp/projBuckets");
        // Samples at 0,5 (bucket 0) and 20,25 (bucket 2); bucket 1 is empty.
        let (_dir, db) = seeded(root, &[(100, 0), (300, 5), (1000, 20), (2000, 25)]);
        let window = StatsWindow {
            start: at(0),
            end: at(30),
            bucket_secs: 10,
        };
        let buckets = db
            .node_stats_buckets(root, "dev", "web:local", window)
            .unwrap();
        assert_eq!(buckets.len(), 2, "the empty bucket is omitted, not zeroed");
        assert_eq!(buckets[0].bucket_start, at(0));
        assert_eq!(buckets[0].samples, 2);
        assert_eq!(buckets[0].memory_bytes, 200, "mean of 100 and 300");
        assert_eq!(buckets[1].bucket_start, at(20));
        assert_eq!(buckets[1].memory_bytes, 1500);
        assert_eq!(buckets[1].footprint_peak, 1000, "peak of 500 and 1000");
        assert_eq!(buckets[1].memory.private_dirty, Some(375));
    }

    #[test]
    fn bucket_drops_a_class_that_is_missing_from_any_sample() {
        let root = Path::new("/tmp/projPartial");
        let (_dir, db) = test_db();
        db.save_run(root, "proj", &RunState::new("dev", "proj"))
            .unwrap();
        // One detailed sample and one basic sample land in the same bucket.
        db.record_node_stats(
            root,
            "dev",
            &[("web:local".into(), stat(10.0, 1000, 1, 0))],
            &[],
        )
        .unwrap();
        let mut basic = stat(10.0, 1000, 1, 2);
        basic.memory = MemoryBreakdown::basic(1000, 5000);
        db.record_node_stats(root, "dev", &[("web:local".into(), basic)], &[])
            .unwrap();

        let buckets = db
            .node_stats_buckets(
                root,
                "dev",
                "web:local",
                StatsWindow {
                    start: at(0),
                    end: at(10),
                    bucket_secs: 10,
                },
            )
            .unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].samples, 2);
        assert_eq!(
            buckets[0].memory.private_dirty, None,
            "a bucket where one sample lacked the class reports no class total"
        );
        assert_eq!(
            buckets[0].memory.footprint, 750,
            "footprint still averages: 500 detailed + 1000 RSS fallback"
        );
    }

    #[test]
    fn process_series_split_by_pid_sorted_by_peak() {
        let root = Path::new("/tmp/projSeries");
        let (_dir, db) = seeded(root, &[(1000, 0), (2000, 10)]);
        let window = StatsWindow {
            start: at(0),
            end: at(20),
            bucket_secs: 10,
        };
        let series = db
            .process_stats_buckets(root, "dev", "web:local", window)
            .unwrap();
        assert_eq!(series.len(), 2, "one series per pid");
        // Both processes carry mem/2, so peaks tie and the pid tiebreak orders
        // them — the point being the order is deterministic across polls.
        assert_eq!(series[0].pid, 100);
        assert_eq!(series[1].pid, 101);
        assert_eq!(series[0].name, "node");
        assert_eq!(series[0].buckets.len(), 2);
        assert_eq!(series[0].buckets[0].bucket_start, at(0));
        assert_eq!(series[0].buckets[0].memory_bytes, 500);
        assert_eq!(series[0].buckets[1].memory_bytes, 1000);
    }

    #[test]
    fn window_for_points_caps_resolution() {
        let w = StatsWindow::for_points(at(0), at(3600), 200);
        assert_eq!(w.bucket_secs, 18);
        let count = 3600 / w.bucket_secs;
        assert!(count <= 200, "point budget is a ceiling, got {count}");
        // A window shorter than the budget can't go below 1s.
        let w = StatsWindow::for_points(at(0), at(10), 200);
        assert_eq!(w.bucket_secs, 1);
        // Rounding up must never produce more buckets than asked for.
        let w = StatsWindow::for_points(at(0), at(999), 100);
        assert_eq!(w.bucket_secs, 10);
        assert!(999 / w.bucket_secs <= 100);
    }

    #[test]
    fn record_unknown_run_is_noop() {
        let (_dir, db) = test_db();
        let root = Path::new("/nope");
        db.record_node_stats(
            root,
            "dev",
            &[("web:local".into(), stat(1.0, 1, 1, 1))],
            &[],
        )
        .unwrap();
        assert!(db.latest_node_stats(root, "dev").unwrap().is_empty());
    }

    #[test]
    fn stats_cascade_delete_with_run() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/projCascade");
        let run = RunState::new("dev", "proj");
        db.save_run(root, "proj", &run).unwrap();
        db.record_node_stats(
            root,
            "dev",
            &[("web:local".into(), stat(1.0, 1, 1, 1))],
            &[("web:local".into(), vec![proc_sample(1, None, "sh", 1)])],
        )
        .unwrap();

        assert!(
            db.begin_ending(&run.run_id, crate::state::EndReason::Stopped, None)
                .unwrap()
        );
        assert!(db.finalize_run(&run.run_id).unwrap());
        assert!(db.delete_ended_run(&run.run_id).unwrap());

        for table in ["node_stats", "node_process_stats"] {
            let n: i64 = db
                .lock()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} rows cascade-delete with their run");
        }
    }

    #[test]
    fn prunes_process_rows_independently() {
        let root = Path::new("/tmp/projPrune");
        let (_dir, db) = seeded(root, &[(100, 100), (200, 300)]);
        assert_eq!(db.prune_node_process_stats_older_than(at(200)).unwrap(), 2);
        assert_eq!(
            db.node_stats_history(root, "dev", "web:local", 10)
                .unwrap()
                .len(),
            2,
            "aggregates keep their own, longer horizon"
        );
        let tree = db.latest_process_tree(root, "dev", "web:local").unwrap();
        assert_eq!(tree.len(), 2, "the newer sample's processes survive");
    }
}
