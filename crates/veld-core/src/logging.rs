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
    pub fn write_lines(&self, ts: chrono::DateTime<Utc>, lines: &[String]) -> Result<(), LogError> {
        let run_id = self.run_id.map(|id| id.to_string());
        self.db.append_logs(
            &self.project_root,
            &self.run_name,
            run_id.as_deref(),
            self.node.as_deref(),
            self.variant.as_deref(),
            self.stream,
            ts,
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
