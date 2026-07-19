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
/// to one (run, node, stream).
#[derive(Clone)]
pub struct LogWriter {
    db: Db,
    project_root: PathBuf,
    run_name: String,
    node: Option<String>,
    variant: Option<String>,
    stream: LogStream,
}

impl LogWriter {
    /// Create a writer for a per-node stream (server/client/setup).
    pub fn for_node(
        db: Db,
        project_root: &Path,
        run_name: &str,
        node: &str,
        variant: &str,
        stream: LogStream,
    ) -> Self {
        Self {
            db,
            project_root: project_root.to_path_buf(),
            run_name: run_name.to_owned(),
            node: Some(node.to_owned()),
            variant: Some(variant.to_owned()),
            stream,
        }
    }

    /// Create a writer for a run-level stream (debug/internal).
    pub fn for_run(db: Db, project_root: &Path, run_name: &str, stream: LogStream) -> Self {
        Self {
            db,
            project_root: project_root.to_path_buf(),
            run_name: run_name.to_owned(),
            node: None,
            variant: None,
            stream,
        }
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
        self.db.append_log(
            &self.project_root,
            &self.run_name,
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
