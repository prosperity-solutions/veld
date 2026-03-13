use std::path::{Path, PathBuf};

use chrono::Utc;
use thiserror::Error;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum LogError {
    #[error("failed to create log directory {path}: {source}")]
    CreateDirFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to write log {path}: {source}")]
    WriteFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to read log {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// Log paths
// ---------------------------------------------------------------------------

/// Return the log directory for a run: `.veld/logs/{run_name}/`.
pub fn log_dir(project_root: &Path, run_name: &str) -> PathBuf {
    project_root.join(".veld").join("logs").join(run_name)
}

/// Return the log file path for a node+variant.
pub fn log_file(project_root: &Path, run_name: &str, node: &str, variant: &str) -> PathBuf {
    log_dir(project_root, run_name).join(format!("{node}-{variant}.log"))
}

/// Return the setup (command step) log file path.
pub fn setup_log_file(project_root: &Path, run_name: &str, node: &str, variant: &str) -> PathBuf {
    log_dir(project_root, run_name).join(format!("{node}-{variant}-setup.log"))
}

/// Return the debug log file path for a run.
pub fn debug_log_file(project_root: &Path, run_name: &str) -> PathBuf {
    log_dir(project_root, run_name).join("veld-debug.log")
}

// ---------------------------------------------------------------------------
// Log writer
// ---------------------------------------------------------------------------

/// A writer that timestamps each line and appends to a log file.
#[derive(Clone)]
pub struct LogWriter {
    path: PathBuf,
}

impl LogWriter {
    /// Create a new log writer. Ensures the parent directory exists.
    pub async fn new(path: PathBuf) -> Result<Self, LogError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| LogError::CreateDirFailed {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
        }
        Ok(Self { path })
    }

    /// Write a single line with a timestamp prefix.
    pub async fn write_line(&self, line: &str) -> Result<(), LogError> {
        let timestamp = Utc::now().to_rfc3339();
        let formatted = format!("[{timestamp}] {line}\n");

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .map_err(|e| LogError::WriteFailed {
                path: self.path.clone(),
                source: e,
            })?;

        file.write_all(formatted.as_bytes())
            .await
            .map_err(|e| LogError::WriteFailed {
                path: self.path.clone(),
                source: e,
            })?;

        Ok(())
    }

    /// Write a Veld-internal annotation (e.g. process exit).
    pub async fn write_annotation(&self, message: &str) -> Result<(), LogError> {
        self.write_line(&format!("[VELD] {message}")).await
    }
}

// ---------------------------------------------------------------------------
// Log reader
// ---------------------------------------------------------------------------

/// Read the last `n` lines from a log file.
pub async fn tail_lines(path: &Path, n: usize) -> Result<Vec<String>, LogError> {
    let content = fs::read_to_string(path)
        .await
        .map_err(|e| LogError::ReadFailed {
            path: path.to_path_buf(),
            source: e,
        })?;

    let lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].to_vec())
}

/// Read lines since a given duration ago (based on the ISO 8601 timestamps in the log).
pub async fn lines_since(path: &Path, since: chrono::Duration) -> Result<Vec<String>, LogError> {
    let cutoff = Utc::now() - since;
    let content = fs::read_to_string(path)
        .await
        .map_err(|e| LogError::ReadFailed {
            path: path.to_path_buf(),
            source: e,
        })?;

    let mut result = Vec::new();
    for line in content.lines() {
        // Lines are formatted as `[2026-03-11T14:23:01.123456Z] ...`.
        if let Some(ts_str) = extract_timestamp(line) {
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str) {
                if ts >= cutoff {
                    result.push(line.to_owned());
                }
                continue;
            }
        }
        // If we can't parse the timestamp, include the line if we've already
        // started collecting (continuation lines).
        if !result.is_empty() {
            result.push(line.to_owned());
        }
    }

    Ok(result)
}

/// Extract the timestamp string from a log line (between first `[` and `]`).
fn extract_timestamp(line: &str) -> Option<&str> {
    let start = line.find('[')? + 1;
    let end = line[start..].find(']')? + start;
    Some(&line[start..end])
}

/// Format a log line as JSON for `--json` output.
pub fn line_to_json(line: &str, run: &str, node: &str, variant: &str) -> serde_json::Value {
    let (timestamp, content) = if let Some(ts) = extract_timestamp(line) {
        let after_bracket = line.find(']').map(|i| i + 2).unwrap_or(0);
        (
            ts.to_owned(),
            line.get(after_bracket..).unwrap_or("").to_owned(),
        )
    } else {
        (String::new(), line.to_owned())
    };

    serde_json::json!({
        "timestamp": timestamp,
        "run": run,
        "node": node,
        "variant": variant,
        "line": content,
    })
}
