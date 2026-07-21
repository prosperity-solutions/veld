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
