//! Service log storage: one row per log line, scoped by project + run.
//!
//! Replaces the old `.veld/logs/{run}/*.log` files. Writers (the CLI, the
//! detached `veld _log` pipeline wrapper, and the daemon) insert rows; readers
//! (`veld logs`, the management UI) query by scope. Follow mode polls for
//! rows with `id` greater than the last one seen — `id` is a global,
//! monotonically increasing insert order across all writer processes.

use std::path::Path;

use rusqlite::params;

use super::{Db, DbError, ts_to_str};
use crate::db::state::root_key;

/// Which log stream a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    /// Server process stdout/stderr (per node).
    Server,
    /// Browser-side client logs (per node).
    Client,
    /// Setup/command step output (per node).
    Setup,
    /// Orchestration trace (`--debug`, per run).
    Debug,
    /// Veld-internal lifecycle events: liveness, recovery (per run).
    Internal,
}

impl LogStream {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogStream::Server => "server",
            LogStream::Client => "client",
            LogStream::Setup => "setup",
            LogStream::Debug => "debug",
            LogStream::Internal => "internal",
        }
    }
}

/// One stored log line.
#[derive(Debug, Clone)]
pub struct LogRow {
    pub id: i64,
    pub node: Option<String>,
    pub variant: Option<String>,
    pub stream: String,
    /// RFC 3339 UTC timestamp string.
    pub ts: String,
    pub line: String,
}

/// A filter for reading logs. `node`/`variant` of `None` match any node;
/// `streams` of `None` matches all streams.
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub node: Option<String>,
    pub variant: Option<String>,
    pub streams: Option<Vec<&'static str>>,
}

impl LogFilter {
    fn where_clause(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut sql = String::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(ref node) = self.node {
            sql.push_str(" AND node = ?");
            params.push(Box::new(node.clone()));
        }
        if let Some(ref variant) = self.variant {
            sql.push_str(" AND variant = ?");
            params.push(Box::new(variant.clone()));
        }
        // An empty stream list is treated like `None` (match all) — emitting
        // `IN ()` would be a SQL syntax error.
        if let Some(ref streams) = self.streams {
            if !streams.is_empty() {
                sql.push_str(" AND stream IN (");
                for (i, s) in streams.iter().enumerate() {
                    if i > 0 {
                        sql.push(',');
                    }
                    sql.push('?');
                    params.push(Box::new(s.to_string()));
                }
                sql.push(')');
            }
        }
        (sql, params)
    }
}

fn row_to_log(row: &rusqlite::Row<'_>) -> rusqlite::Result<LogRow> {
    Ok(LogRow {
        id: row.get(0)?,
        node: row.get(1)?,
        variant: row.get(2)?,
        stream: row.get(3)?,
        ts: row.get(4)?,
        line: row.get(5)?,
    })
}

const LOG_COLS: &str = "id, node, variant, stream, ts, line";

impl Db {
    /// Append one log line. `node`/`variant` are `None` for run-level streams
    /// (debug/internal).
    #[allow(clippy::too_many_arguments)]
    pub fn append_log(
        &self,
        project_root: &Path,
        run_name: &str,
        node: Option<&str>,
        variant: Option<&str>,
        stream: LogStream,
        ts: chrono::DateTime<chrono::Utc>,
        line: &str,
    ) -> Result<(), DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "INSERT INTO log_lines (project_root, run_name, node, variant, stream, ts, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        stmt.execute(params![
            root_key(project_root),
            run_name,
            node,
            variant,
            stream.as_str(),
            ts_to_str(ts),
            line,
        ])?;
        Ok(())
    }

    /// Last `n` lines matching the filter, in insertion order.
    pub fn tail_logs(
        &self,
        project_root: &Path,
        run_name: &str,
        filter: &LogFilter,
        n: usize,
    ) -> Result<Vec<LogRow>, DbError> {
        let (where_sql, mut extra) = filter.where_clause();
        let sql = format!(
            "SELECT {LOG_COLS} FROM log_lines
             WHERE project_root = ? AND run_name = ?{where_sql}
             ORDER BY id DESC LIMIT ?"
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(root_key(project_root)),
            Box::new(run_name.to_owned()),
        ];
        params.append(&mut extra);
        params.push(Box::new(n as i64));

        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut rows: Vec<LogRow> = stmt
            .query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
                row_to_log,
            )?
            .collect::<Result<_, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// All lines with a timestamp at or after `cutoff`, matching the filter.
    pub fn logs_since(
        &self,
        project_root: &Path,
        run_name: &str,
        filter: &LogFilter,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<LogRow>, DbError> {
        let (where_sql, mut extra) = filter.where_clause();
        let sql = format!(
            "SELECT {LOG_COLS} FROM log_lines
             WHERE project_root = ? AND run_name = ? AND ts >= ?{where_sql}
             ORDER BY id"
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(root_key(project_root)),
            Box::new(run_name.to_owned()),
            Box::new(ts_to_str(cutoff)),
        ];
        params.append(&mut extra);

        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
                row_to_log,
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// All lines with `id > after_id`, matching the filter (follow mode).
    pub fn logs_after_id(
        &self,
        project_root: &Path,
        run_name: &str,
        filter: &LogFilter,
        after_id: i64,
    ) -> Result<Vec<LogRow>, DbError> {
        let (where_sql, mut extra) = filter.where_clause();
        let sql = format!(
            "SELECT {LOG_COLS} FROM log_lines
             WHERE project_root = ? AND run_name = ? AND id > ?{where_sql}
             ORDER BY id"
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(root_key(project_root)),
            Box::new(run_name.to_owned()),
            Box::new(after_id),
        ];
        params.append(&mut extra);

        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
                row_to_log,
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The highest log row id (0 when empty). Snapshot point for follow mode.
    pub fn max_log_id(&self) -> Result<i64, DbError> {
        let conn = self.lock();
        let id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM log_lines", [], |r| {
            r.get(0)
        })?;
        Ok(id)
    }

    /// Delete log lines older than `cutoff`. Returns the number deleted.
    pub fn prune_logs_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, DbError> {
        let conn = self.lock();
        let n = conn.execute("DELETE FROM log_lines WHERE ts < ?1", [ts_to_str(cutoff)])?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    fn append(db: &Db, node: Option<&str>, stream: LogStream, line: &str) {
        db.append_log(
            Path::new("/tmp/p"),
            "dev",
            node,
            node.map(|_| "local"),
            stream,
            chrono::Utc::now(),
            line,
        )
        .unwrap();
    }

    #[test]
    fn tail_and_follow() {
        let (_dir, db) = test_db();
        for i in 0..10 {
            append(&db, Some("web"), LogStream::Server, &format!("line {i}"));
        }
        append(&db, None, LogStream::Internal, "internal line");

        let filter = LogFilter {
            node: Some("web".into()),
            variant: None,
            streams: Some(vec!["server"]),
        };
        let tail = db
            .tail_logs(Path::new("/tmp/p"), "dev", &filter, 3)
            .unwrap();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].line, "line 7");
        assert_eq!(tail[2].line, "line 9");

        // Follow from the snapshot point.
        let last = db.max_log_id().unwrap();
        append(&db, Some("web"), LogStream::Server, "line 10");
        let new = db
            .logs_after_id(Path::new("/tmp/p"), "dev", &filter, last)
            .unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].line, "line 10");

        // Unfiltered tail sees the internal line too.
        let all = db
            .tail_logs(Path::new("/tmp/p"), "dev", &LogFilter::default(), 100)
            .unwrap();
        assert_eq!(all.len(), 12);
    }

    #[test]
    fn since_and_prune() {
        let (_dir, db) = test_db();
        let old = chrono::Utc::now() - chrono::Duration::hours(2);
        db.append_log(
            Path::new("/tmp/p"),
            "dev",
            Some("web"),
            Some("local"),
            LogStream::Server,
            old,
            "old line",
        )
        .unwrap();
        append(&db, Some("web"), LogStream::Server, "new line");

        let cutoff = chrono::Utc::now() - chrono::Duration::hours(1);
        let since = db
            .logs_since(Path::new("/tmp/p"), "dev", &LogFilter::default(), cutoff)
            .unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].line, "new line");

        assert_eq!(db.prune_logs_older_than(cutoff).unwrap(), 1);
        let rest = db
            .tail_logs(Path::new("/tmp/p"), "dev", &LogFilter::default(), 100)
            .unwrap();
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn ids_stay_monotonic_after_full_prune() {
        // Follow mode uses the max id as a watermark; AUTOINCREMENT guarantees
        // a fresh insert after pruning everything still gets a larger id.
        let (_dir, db) = test_db();
        append(&db, Some("web"), LogStream::Server, "old");
        let watermark = db.max_log_id().unwrap();
        db.prune_logs_older_than(chrono::Utc::now() + chrono::Duration::hours(1))
            .unwrap();
        append(&db, Some("web"), LogStream::Server, "new");
        let rows = db
            .logs_after_id(Path::new("/tmp/p"), "dev", &LogFilter::default(), watermark)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, "new");
        assert!(rows[0].id > watermark);
    }
}
