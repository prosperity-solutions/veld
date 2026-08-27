//! Log writing helpers on top of the central database (see [`crate::db`]).
//!
//! Every log line is one `log_lines` row, timestamped at write time. The old
//! `.veld/logs/{run}/*.log` files are gone; `veld logs` and the management UI
//! query the database instead.

use std::path::{Path, PathBuf};

use chrono::Utc;
use thiserror::Error;

use crate::db::{Db, DbError, LogStream};

#[derive(Debug, Error)]
pub enum LogError {
    #[error("failed to write log: {0}")]
    WriteFailed(#[from] DbError),
}

/// Return a temporary output file path for a command node.
///
/// Scripts write `key=value` lines to this file instead of emitting
/// `VELD_OUTPUT` on stdout, keeping sensitive values off the terminal.
/// This stays a file (not the database) because it is the IPC contract with
/// user scripts via `$VELD_OUTPUT_FILE`.
pub fn output_file(project_root: &Path, run_name: &str, node: &str, variant: &str) -> PathBuf {
    project_root
        .join(".veld")
        .join("tmp")
        .join(format!("{run_name}-{node}-{variant}.outputs"))
}

// ---------------------------------------------------------------------------
// Log writer
// ---------------------------------------------------------------------------

/// A writer that timestamps each line and stores it in the database, scoped
/// to one (run instance, node, stream).
#[derive(Clone)]
pub struct LogWriter {
    db: Db,
    project_root: PathBuf,
    run_name: String,
    /// Run instance the lines belong to. `None` only for writers created
    /// before the run exists (set via [`LogWriter::set_run_id`] as soon as it
    /// does) — such lines are reachable only under `--all-runs`.
    run_id: Option<uuid::Uuid>,
    node: Option<String>,
    variant: Option<String>,
    stream: LogStream,
}

impl LogWriter {
    /// Create a writer for a per-node stream (server/client/setup).
    #[allow(clippy::too_many_arguments)]
    pub fn for_node(
        db: Db,
        project_root: &Path,
        run_name: &str,
        run_id: uuid::Uuid,
        node: &str,
        variant: &str,
        stream: LogStream,
    ) -> Self {
        Self {
            db,
            project_root: project_root.to_path_buf(),
            run_name: run_name.to_owned(),
            run_id: Some(run_id),
            node: Some(node.to_owned()),
            variant: Some(variant.to_owned()),
            stream,
        }
    }

    /// Create a writer for a run-level stream (debug/internal). The run
    /// instance may not exist yet — stamp it with [`LogWriter::set_run_id`]
    /// as soon as it does.
    pub fn for_run(db: Db, project_root: &Path, run_name: &str, stream: LogStream) -> Self {
        Self {
            db,
            project_root: project_root.to_path_buf(),
            run_name: run_name.to_owned(),
            run_id: None,
            node: None,
            variant: None,
            stream,
        }
    }

    /// Scope subsequent lines to a run instance.
    pub fn set_run_id(&mut self, run_id: uuid::Uuid) {
        self.run_id = Some(run_id);
    }

    /// Write a single line, timestamped now.
    pub async fn write_line(&self, line: &str) -> Result<(), LogError> {
        self.write_with_ts(Utc::now(), line)
    }

    /// Write a Veld-internal annotation (e.g. process exit).
    pub async fn write_annotation(&self, message: &str) -> Result<(), LogError> {
        self.write_line(&format!("[VELD] {message}")).await
    }

    /// Write a batch of lines under one timestamp, as one transaction.
    ///
    /// The batch is what a pipe reader hands its sink; writing it line by line
    /// would take the database lock once per line (see [`Db::append_logs`]).
    /// One shared stamp is right here because the batch came out of a single
    /// read — for a batch assembled over a flush window use
    /// [`LogWriter::write_stamped_lines`] instead.
    pub fn write_lines(&self, ts: chrono::DateTime<Utc>, lines: &[String]) -> Result<(), LogError> {
        let stamped: Vec<(chrono::DateTime<Utc>, String)> =
            lines.iter().map(|l| (ts, l.clone())).collect();
        self.write_stamped_lines(&stamped)
    }

    /// Write a batch of individually-stamped lines, as one transaction.
    ///
    /// What a [`LogBatch`] hands back: the lines arrived at different moments
    /// and each keeps its own arrival time, so the flush window never shows up
    /// in the data.
    pub fn write_stamped_lines(
        &self,
        lines: &[(chrono::DateTime<Utc>, String)],
    ) -> Result<(), LogError> {
        let run_id = self.run_id.map(|id| id.to_string());
        self.db.append_logs(
            &self.project_root,
            &self.run_name,
            run_id.as_deref(),
            self.node.as_deref(),
            self.variant.as_deref(),
            self.stream,
            lines,
        )?;
        Ok(())
    }

    /// Write a line with an explicit timestamp (client logs carry their own).
    pub fn write_with_ts(&self, ts: chrono::DateTime<Utc>, line: &str) -> Result<(), LogError> {
        let run_id = self.run_id.map(|id| id.to_string());
        self.db.append_log(
            &self.project_root,
            &self.run_name,
            run_id.as_deref(),
            self.node.as_deref(),
            self.variant.as_deref(),
            self.stream,
            ts,
            line,
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Batching
// ---------------------------------------------------------------------------

/// Lines a [`LogBatch`] holds before it must be flushed regardless of the clock.
///
/// Sized so the cap is what fires under load and the deadline is what fires when
/// a process goes quiet. At 512 lines the transaction is still short and the
/// per-line write amplification is already at the measured floor — the curve in
/// [`Db::append_logs`] is essentially flat past ~32.
pub const LOG_BATCH_MAX_LINES: usize = 512;

/// How long a partial batch waits for company before being written anyway.
///
/// This is the *only* latency this batching adds, and it is invisible to every
/// reader veld has: the four `--follow` loops poll at 200 ms and the `/ide` log
/// panel refetches at 2,000 ms. Do not raise it into their range to buy a bigger
/// batch — the cap above is what does that job under load, and a process that is
/// producing enough output for the window to matter is hitting the cap anyway.
pub const LOG_BATCH_FLUSH: std::time::Duration = std::time::Duration::from_millis(50);

/// A line buffer for a reader that produces one line at a time.
///
/// Turns a line-at-a-time producer into [`Db::append_logs`]'s batch, which is
/// worth 9.8× in WAL pages per line (see that method). Owns the caps and the
/// stamping; the *waiting* is the caller's, because an async pipe reader, a
/// blocking `stdin` pump and a channel consumer each have their own way to race a
/// read against a deadline.
///
/// The deadline is armed by the **first** line of a batch and never re-armed by a
/// later one. A per-line deadline is the bug to avoid here: a steady stream
/// arriving just inside the window would extend it forever, and the batch would
/// only ever flush on the size cap — turning a 50 ms bound into
/// `LOG_BATCH_MAX_LINES` × the inter-line gap, which is seconds.
#[derive(Default)]
pub struct LogBatch {
    pending: Vec<(chrono::DateTime<Utc>, String)>,
    deadline: Option<std::time::Instant>,
}

impl LogBatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Buffer a line, stamped with its arrival time, arming the flush deadline
    /// if this is the first line of a batch.
    pub fn push(&mut self, line: String) {
        if self.pending.is_empty() {
            self.deadline = Some(std::time::Instant::now() + LOG_BATCH_FLUSH);
        }
        self.pending.push((Utc::now(), line));
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Whether the size cap has been reached and the batch must be written now.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.pending.len() >= LOG_BATCH_MAX_LINES
    }

    /// The moment this batch must be written by, or `None` when it is empty.
    #[must_use]
    pub fn deadline(&self) -> Option<std::time::Instant> {
        self.deadline
    }

    /// How long is left before the deadline; `Duration::ZERO` once it has passed.
    /// `None` when the batch is empty and there is nothing to wait for.
    #[must_use]
    pub fn time_left(&self) -> Option<std::time::Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(std::time::Instant::now()))
    }

    /// Hand the buffered lines to a writer and clear the batch.
    ///
    /// Best-effort by design, and by the same reasoning every existing log-write
    /// call site uses (`let _ = db.append_log(...)`): the writer is draining a
    /// child's pipe, and a process that stops draining because the database was
    /// busy blocks the child on a full pipe. Losing a log line is recoverable;
    /// wedging the environment that produced it is not.
    pub fn flush(&mut self, writer: &LogWriter) {
        if self.pending.is_empty() {
            return;
        }
        let _ = writer.write_stamped_lines(&self.pending);
        self.pending.clear();
        self.deadline = None;
    }

    /// Take the buffered lines out, for a caller that writes them itself.
    pub fn take(&mut self) -> Vec<(chrono::DateTime<Utc>, String)> {
        self.deadline = None;
        std::mem::take(&mut self.pending)
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format a stored log row as JSON for `--json` output.
///
/// `timestamp` is the stored string, which is always UTC, and deliberately is not
/// affected by [`LogTimeZone`]: this is the machine-readable shape an agent or a CI
/// script parses, so it has exactly one spelling regardless of who ran the command
/// or where. Localising is [`format_ts`]'s job, at the human render sites.
pub fn row_to_json(row: &crate::db::LogRow, run: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": row.ts,
        "run": run,
        "node": row.node.as_deref().unwrap_or("_veld"),
        "variant": row.variant.as_deref().unwrap_or(&row.stream),
        "source": row.stream,
        "line": row.line,
    })
}

/// Render a stored log timestamp for a human reader.
///
/// The input is what `db::ts_to_str` wrote: RFC 3339, UTC, microseconds, `Z`.
///
/// [`LogTimeZone::Utc`] returns it untouched, so `veld logs --utc` is byte-for-byte
/// what veld printed before this function existed. [`LogTimeZone::Local`] re-renders
/// it in the machine's zone **in the same RFC 3339 shape, offset included** — same
/// field order, same precision, still parseable by anything that parsed the old
/// output, and the trailing `+02:00` instead of `Z` is what makes the two
/// distinguishable in a pasted snippet with no context.
///
/// A string that does not parse is passed through unchanged rather than replaced with
/// a placeholder: a log line's timestamp is evidence, and a row whose `ts` column
/// holds something unexpected is exactly the row you need to see as-is.
pub fn format_ts(ts: &str, tz: crate::db::LogTimeZone) -> String {
    match tz {
        crate::db::LogTimeZone::Utc => ts.to_string(),
        crate::db::LogTimeZone::Local => chrono::DateTime::parse_from_rfc3339(ts)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
            })
            .unwrap_or_else(|_| ts.to_string()),
    }
}

#[cfg(test)]
mod log_batch_tests {
    use super::{LOG_BATCH_FLUSH, LOG_BATCH_MAX_LINES, LogBatch};

    /// The trap this type exists to avoid. A deadline re-armed by every line
    /// turns a 50 ms bound into `LOG_BATCH_MAX_LINES` × the inter-line gap —
    /// seconds — because a steady producer keeps pushing it out and only the
    /// size cap ever fires. Asserted as "the deadline does not move", which is
    /// the property, rather than by timing anything.
    #[test]
    fn the_flush_deadline_is_armed_by_the_first_line_and_never_re_armed() {
        let mut batch = LogBatch::new();
        assert!(batch.deadline().is_none(), "an empty batch has no deadline");

        batch.push("first".into());
        let armed = batch
            .deadline()
            .expect("the first line must arm the deadline");

        for i in 0..10 {
            batch.push(format!("line {i}"));
            assert_eq!(
                batch.deadline(),
                Some(armed),
                "line {i} moved the deadline — a later line must never extend it"
            );
        }
    }

    /// Taking the batch disarms it, so the next line starts a fresh window
    /// rather than inheriting an already-expired one (which would flush every
    /// single line on its own — the exact shape this replaced).
    #[test]
    fn taking_a_batch_disarms_the_deadline() {
        let mut batch = LogBatch::new();
        batch.push("a".into());
        let first_deadline = batch.deadline().unwrap();

        let taken = batch.take();
        assert_eq!(taken.len(), 1);
        assert!(batch.is_empty());
        assert!(batch.deadline().is_none(), "take must disarm");

        batch.push("b".into());
        let second_deadline = batch.deadline().unwrap();
        assert!(
            second_deadline >= first_deadline,
            "a new batch must arm a new window, not reuse the old one"
        );
    }

    #[test]
    fn the_size_cap_is_what_fires_under_load() {
        let mut batch = LogBatch::new();
        for i in 0..LOG_BATCH_MAX_LINES - 1 {
            batch.push(format!("line {i}"));
            assert!(!batch.is_full());
        }
        batch.push("the one that fills it".into());
        assert!(batch.is_full());
        assert_eq!(batch.take().len(), LOG_BATCH_MAX_LINES);
    }

    /// Lines are stamped as they arrive, not as they are written — the whole
    /// reason `Db::append_logs` takes per-row timestamps.
    #[test]
    fn lines_are_stamped_on_arrival() {
        let mut batch = LogBatch::new();
        batch.push("early".into());
        std::thread::sleep(std::time::Duration::from_millis(2));
        batch.push("late".into());

        let taken = batch.take();
        assert!(
            taken[0].0 < taken[1].0,
            "a line pushed later must carry a later timestamp, not the flush time"
        );
    }

    /// The flush window has to stay well under what every reader polls at, or
    /// the batching becomes visible as lag in `--follow`. The four follow loops
    /// poll at 200 ms and the `/ide` panel at 2,000 ms.
    #[test]
    fn the_flush_window_stays_invisible_to_every_reader() {
        assert!(
            LOG_BATCH_FLUSH <= std::time::Duration::from_millis(100),
            "a flush window in a reader's polling range makes batching visible as lag"
        );
    }

    /// The size cap has a ceiling too, and for a different reason than the
    /// window: it is how long the process-wide connection mutex is held by one
    /// transaction, and every pipe consumer in veld shares that mutex. "Raise it
    /// to reduce write load" is the natural next edit and the write-amplification
    /// curve is already flat past ~32 rows (see `Db::append_logs`), so there is
    /// nothing to buy above this and a wedged environment to lose.
    #[test]
    fn the_batch_size_cap_stays_small_enough_to_hold_a_lock_briefly() {
        assert!(
            (32..=4096).contains(&LOG_BATCH_MAX_LINES),
            "below ~32 the batching stops paying for itself; far above this one \
             transaction holds the shared connection long enough to matter to every \
             other veld process"
        );
    }
}

#[cfg(test)]
mod format_ts_tests {
    use super::format_ts;
    use crate::db::LogTimeZone;

    #[test]
    fn utc_is_the_stored_string_untouched() {
        // The compatibility promise of `--utc`: not "UTC re-rendered", the same bytes.
        let stored = "2026-08-06T09:12:33.123456Z";
        assert_eq!(format_ts(stored, LogTimeZone::Utc), stored);
    }

    #[test]
    fn local_keeps_the_instant_and_says_which_zone_it_is_in() {
        // Asserted against the machine's own zone rather than a fixed offset: CI runs
        // in UTC and a developer does not, and pinning either would make this test a
        // statement about the runner instead of about the conversion.
        let stored = "2026-08-06T09:12:33.123456Z";
        let local = format_ts(stored, LogTimeZone::Local);
        let reparsed = chrono::DateTime::parse_from_rfc3339(&local).expect("still RFC 3339");
        assert_eq!(
            reparsed.to_utc(),
            chrono::DateTime::parse_from_rfc3339(stored)
                .unwrap()
                .to_utc(),
            "conversion must move the label, never the instant"
        );
        // Microseconds survive: dropping precision would silently lose the ordering
        // information the interleave sort depends on being visible.
        assert!(local.contains(".123456"), "{local}");
    }

    #[test]
    fn an_unparseable_timestamp_is_passed_through_in_both_zones() {
        for tz in [LogTimeZone::Local, LogTimeZone::Utc] {
            assert_eq!(format_ts("not-a-timestamp", tz), "not-a-timestamp");
            assert_eq!(format_ts("", tz), "");
        }
    }
}
