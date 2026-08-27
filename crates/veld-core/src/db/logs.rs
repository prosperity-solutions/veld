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
    /// Project `setup`/`teardown` step output. Read run-level, but the rows
    /// carry a pseudo-node (`setup`/`teardown` + the step name as variant) so
    /// two steps stay distinguishable in one interleaved stream. A `command`
    /// node's output is *not* here — it goes to `Server`, like any node's.
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

/// Whether a stored stream name is scoped per node (rows carry node/variant
/// and readers window/filter by node) as opposed to run-level (internal/debug
/// rows have `node = NULL`; setup rows carry a node but are read run-level).
/// Single source of truth for readers that split sources by kind.
pub fn stream_is_per_node(stream: &str) -> bool {
    stream == LogStream::Server.as_str() || stream == LogStream::Client.as_str()
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
/// `streams` of `None` matches all streams; `run_id` of `None` matches every
/// run instance under the name — including legacy pre-v3 rows whose stored
/// `run_id` is NULL — while `Some` scopes to one instance.
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub node: Option<String>,
    pub variant: Option<String>,
    pub streams: Option<Vec<&'static str>>,
    pub run_id: Option<String>,
}

impl LogFilter {
    fn where_clause(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut sql = String::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(ref run_id) = self.run_id {
            sql.push_str(" AND run_id = ?");
            params.push(Box::new(run_id.clone()));
        }
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
    /// (debug/internal). `run_id` scopes the line to one run instance; `None`
    /// only for writers that predate the run (never in new code paths).
    #[allow(clippy::too_many_arguments)]
    pub fn append_log(
        &self,
        project_root: &Path,
        run_name: &str,
        run_id: Option<&str>,
        node: Option<&str>,
        variant: Option<&str>,
        stream: LogStream,
        ts: chrono::DateTime<chrono::Utc>,
        line: &str,
    ) -> Result<(), DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "INSERT INTO log_lines (project_root, run_name, run_id, node, variant, stream, ts, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        stmt.execute(params![
            root_key(project_root),
            run_name,
            run_id,
            node,
            variant,
            stream.as_str(),
            ts_to_str(ts),
            line,
        ])?;
        Ok(())
    }

    /// Append many lines that share a scope, as one transaction. Each line
    /// carries its own timestamp.
    ///
    /// A build tool emits tens of thousands of lines through one sink, and each
    /// autocommit `INSERT` takes the process-wide connection lock on its own —
    /// serializing against every other node in the stage and against the daemon.
    /// Grouping the batch the reader already produced makes that one lock
    /// acquisition and one commit.
    ///
    /// **Measured, on the #332 incident data: 6.47 WAL pages written per log
    /// line at one row per transaction, 0.658 at 32 rows — a 9.8× reduction.**
    /// That is the single largest write-amplification lever in this codebase,
    /// and it needs no migration, no setting and no reader change. Every
    /// per-line writer should reach for this rather than [`Db::append_log`];
    /// `logging::LogBatch` is the buffer that makes one out of a line-at-a-time
    /// reader.
    ///
    /// Timestamps are **per row**, not per batch. An earlier shape stamped the
    /// whole batch with one `ts`, which is fine for a batch that came out of a
    /// single `read` but wrong for one assembled over a flush window: readers
    /// tiebreak on `id` so within-source ordering stays exact either way, but a
    /// shared stamp fuzzes cross-source interleaving by the whole window and
    /// quietly makes `veld logs --since` answer from a coarser clock than it
    /// looks like it has.
    #[allow(clippy::too_many_arguments)]
    pub fn append_logs(
        &self,
        project_root: &Path,
        run_name: &str,
        run_id: Option<&str>,
        node: Option<&str>,
        variant: Option<&str>,
        stream: LogStream,
        lines: &[(chrono::DateTime<chrono::Utc>, String)],
    ) -> Result<(), DbError> {
        if lines.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO log_lines (project_root, run_name, run_id, node, variant, stream, ts, line)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            let root = root_key(project_root);
            for (ts, line) in lines {
                stmt.execute(params![
                    root,
                    run_name,
                    run_id,
                    node,
                    variant,
                    stream.as_str(),
                    ts_to_str(*ts),
                    line,
                ])?;
            }
        }
        tx.commit()?;
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
            None,
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
            run_id: None,
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
            None,
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

    /// Each row keeps its own arrival time. A shared per-batch stamp is what
    /// this replaced, and it made the flush window visible in the data: every
    /// line in a batch answering `--since` from the same coarse instant.
    #[test]
    fn a_batch_stamps_every_line_with_its_own_time() {
        let (_dir, db) = test_db();
        let early = chrono::DateTime::parse_from_rfc3339("2026-08-12T15:26:20.000000Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let late = early + chrono::Duration::seconds(7);

        db.append_logs(
            Path::new("/tmp/p"),
            "dev",
            None,
            Some("web"),
            Some("local"),
            LogStream::Server,
            &[(early, "first".to_owned()), (late, "second".to_owned())],
        )
        .unwrap();

        let rows = db
            .tail_logs(Path::new("/tmp/p"), "dev", &LogFilter::default(), 10)
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].line, "first");
        assert_eq!(rows[1].line, "second");
        assert_ne!(
            rows[0].ts, rows[1].ts,
            "two lines batched together must not collapse onto one timestamp"
        );
        assert!(
            rows[0].ts.starts_with("2026-08-12T15:26:20"),
            "got {}",
            rows[0].ts
        );
        assert!(
            rows[1].ts.starts_with("2026-08-12T15:26:27"),
            "got {}",
            rows[1].ts
        );
    }

    /// An empty batch must be a no-op rather than an empty transaction — the
    /// flush paths call it on every deadline, whether or not anything arrived.
    #[test]
    fn an_empty_batch_writes_nothing() {
        let (_dir, db) = test_db();
        db.append_logs(
            Path::new("/tmp/p"),
            "dev",
            None,
            None,
            None,
            LogStream::Internal,
            &[],
        )
        .unwrap();
        assert!(
            db.tail_logs(Path::new("/tmp/p"), "dev", &LogFilter::default(), 10)
                .unwrap()
                .is_empty()
        );
    }

    /// The reclaim is bounded per call and resumable, and it reports what is
    /// left so a caller can say so. Before this it was a bare
    /// `PRAGMA incremental_vacuum` — the whole freelist in one write transaction,
    /// on every GC pass, whether or not anything had been freed.
    #[test]
    fn page_reclaim_is_skipped_when_there_is_nothing_to_reclaim() {
        let (_dir, db) = test_db();
        // Nothing deleted yet, so nothing on the freelist and nothing to do.
        assert_eq!(
            db.vacuum().unwrap(),
            0,
            "a database with an empty freelist must report nothing remaining"
        );
    }

    /// **The production bound, pinned.** The test below drives an injected
    /// budget, which proves `vacuum_pages` respects its argument and proves
    /// nothing about `vacuum()` — reverting that to a bare
    /// `PRAGMA incremental_vacuum;` left the whole suite green.
    ///
    /// A bare pragma drains the freelist to SQLite's floor in one transaction,
    /// so the tell is that a single `vacuum()` must leave `before - budget`
    /// pages behind.
    ///
    /// **The fixture is deliberately several times the budget.** A first version
    /// used 20,000 rows for a ~2,200-page freelist, and at that size the two
    /// cases are indistinguishable: SQLite's own floor was ~300 pages, so the
    /// unbounded pass reclaimed ~1,900 — inside the 2,000 budget — and the test
    /// passed against the exact reversion it was written to catch. Verified by
    /// re-introducing the bare pragma and watching it fail.
    #[test]
    fn one_vacuum_pass_reclaims_at_most_its_page_budget() {
        let (_dir, db) = test_db();

        // Enough fat rows that deleting them frees several times one pass's
        // budget. Batched, so this costs milliseconds rather than 60,000
        // transactions.
        let filler = "x".repeat(400);
        let rows: Vec<(chrono::DateTime<chrono::Utc>, String)> = (0..60_000)
            .map(|i| (chrono::Utc::now(), format!("line {i} {filler}")))
            .collect();
        for chunk in rows.chunks(5_000) {
            db.append_logs(
                Path::new("/tmp/p"),
                "dev",
                None,
                Some("web"),
                Some("local"),
                LogStream::Server,
                chunk,
            )
            .unwrap();
        }
        db.prune_logs_older_than(chrono::Utc::now() + chrono::Duration::hours(1))
            .unwrap();

        let freelist = || -> u32 {
            let conn = db.lock();
            conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))
                .unwrap()
        };
        let before = freelist();
        let budget = Db::VACUUM_PAGES_PER_PASS;
        assert!(
            before > budget * 2,
            "precondition: the freelist ({before}) must be well clear of one pass's budget \
             ({budget}), or this test cannot tell a bounded pass from an unbounded one — see \
             the doc comment"
        );

        let remaining = db.vacuum().unwrap();
        let reclaimed = before - remaining;

        // Upper bound: the pass is bounded. A stepped *bare* pragma empties the
        // whole freelist and trips this.
        assert!(
            reclaimed <= budget,
            "one pass reclaimed {reclaimed} pages against a budget of {budget} — the reclaim \
             is unbounded again"
        );
        // Lower bound: the pass actually *works*, and this half is the one that
        // catches the subtler reversion. Driving the pragma with
        // `execute_batch` reclaims exactly ONE page per call regardless of the
        // argument, because the pragma emits a result row per relocated page and
        // `execute_batch` steps a statement once — see `Db::vacuum_pages`. That
        // shape satisfies the bound above perfectly while reclaiming nothing,
        // which is what shipped for the life of this database.
        //
        // Measured at exactly `budget` on this fixture; asserted at 90% of it so
        // a future SQLite that declines to relocate a page or two does not turn
        // a working reclaim into a red suite.
        assert!(
            reclaimed >= budget * 9 / 10,
            "one pass reclaimed only {reclaimed} of a {before}-page freelist against a budget \
             of {budget}. One page means the pragma is being run with `execute_batch` instead \
             of being stepped."
        );
    }

    /// The bound is the point of fix 9: one pass must relocate at most its page
    /// budget, so a mass delete drains over several GC passes instead of one very
    /// long write transaction that every other veld process waits behind on its
    /// 10-second `busy_timeout`.
    ///
    /// Tested through the injectable budget rather than the production 2,000,
    /// because exceeding 2,000 free pages needs ~8 MB of fixture and this suite
    /// does not need to be slow to make the point.
    #[test]
    fn page_reclaim_is_bounded_per_pass_and_resumable() {
        let (_dir, db) = test_db();
        for i in 0..3_000 {
            append(
                &db,
                Some("web"),
                LogStream::Server,
                &format!("line {i} {}", "x".repeat(200)),
            );
        }
        db.prune_logs_older_than(chrono::Utc::now() + chrono::Duration::hours(1))
            .unwrap();

        let freelist = || -> u32 {
            let conn = db.lock();
            conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))
                .unwrap()
        };
        let before = freelist();
        assert!(
            before > 30,
            "precondition: deleting 3,000 fat rows must free well over 30 pages, or the \
             budget below is never the binding constraint and this test asserts nothing \
             (got {before})"
        );

        const BUDGET: u32 = 10;
        let after_one = db.vacuum_pages(BUDGET).unwrap();
        assert!(
            before - after_one <= BUDGET,
            "one pass reclaimed {} pages against a budget of {BUDGET}",
            before - after_one
        );
        assert!(
            after_one < before,
            "a bounded pass must still make progress, not stall"
        );

        // Resumable by construction — `incremental_vacuum` picks up where it
        // left off. It converges to SQLite's own floor (pages it will not
        // relocate, e.g. a root page), which is not necessarily zero, so the
        // assertion is convergence rather than a number.
        let mut remaining = after_one;
        for _ in 0..200 {
            let next = db.vacuum_pages(BUDGET).unwrap();
            assert!(next <= remaining, "the freelist must never grow");
            if next == remaining {
                break;
            }
            remaining = next;
        }
        assert!(
            remaining < before / 2,
            "repeated bounded passes must reclaim the bulk of the freelist \
             ({before} -> {remaining})"
        );

        // And the database is still usable afterwards.
        append(&db, Some("web"), LogStream::Server, "after vacuum");
        assert_eq!(
            db.tail_logs(Path::new("/tmp/p"), "dev", &LogFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn vacuum_after_prune_keeps_db_usable() {
        let (_dir, db) = test_db();
        for i in 0..50 {
            append(&db, Some("web"), LogStream::Server, &format!("line {i}"));
        }
        db.prune_logs_older_than(chrono::Utc::now() + chrono::Duration::hours(1))
            .unwrap();
        db.vacuum().unwrap();
        append(&db, Some("web"), LogStream::Server, "after vacuum");
        assert_eq!(
            db.tail_logs(Path::new("/tmp/p"), "dev", &LogFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
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
