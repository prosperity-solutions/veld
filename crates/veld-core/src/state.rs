use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum StateError {
    #[error("failed to read state file {path}: {source}")]
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to write state file {path}: {source}")]
    WriteError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse state file {path}: {source}")]
    ParseError {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[error("run \"{0}\" not found")]
    RunNotFound(String),
}

// ---------------------------------------------------------------------------
// Run status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

// ---------------------------------------------------------------------------
// Node status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Starting,
    HealthChecking,
    Healthy,
    Failed,
    Stopped,
    Skipped,
}

// ---------------------------------------------------------------------------
// Health check phase tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckPhase {
    pub phase: u8, // 1 = port, 2 = HTTPS
    pub passed: bool,
    pub last_error: Option<String>,
    #[serde(with = "chrono::serde::ts_milliseconds_option")]
    pub passed_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Node state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeState {
    pub node_name: String,
    pub variant: String,
    pub status: NodeStatus,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub url: Option<String>,
    pub outputs: HashMap<String, String>,
    pub health_phases: Vec<HealthCheckPhase>,
    /// Output keys whose values are sensitive (encrypted at rest, masked in display).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensitive_keys: Vec<String>,
}

impl NodeState {
    pub fn new(node_name: &str, variant: &str) -> Self {
        Self {
            node_name: node_name.to_owned(),
            variant: variant.to_owned(),
            status: NodeStatus::Pending,
            pid: None,
            port: None,
            url: None,
            outputs: HashMap::new(),
            health_phases: Vec::new(),
            sensitive_keys: Vec::new(),
        }
    }

    /// Encrypt sensitive output values in-place for storage at rest.
    pub fn encrypt_sensitive_outputs(&mut self) {
        for key in &self.sensitive_keys {
            if let Some(value) = self.outputs.get(key) {
                if !crate::sensitive::is_encrypted(value) {
                    let encrypted = crate::sensitive::encrypt_value(value);
                    self.outputs.insert(key.clone(), encrypted);
                }
            }
        }
    }

    /// Decrypt sensitive output values in-place after loading from storage.
    pub fn decrypt_sensitive_outputs(&mut self) {
        for key in &self.sensitive_keys {
            if let Some(value) = self.outputs.get(key) {
                if crate::sensitive::is_encrypted(value) {
                    let decrypted = crate::sensitive::decrypt_value(value);
                    self.outputs.insert(key.clone(), decrypted);
                }
            }
        }
    }

    /// Return a copy of outputs with sensitive values masked for display.
    pub fn display_outputs(&self) -> HashMap<String, String> {
        self.outputs
            .iter()
            .map(|(k, v)| {
                if self.sensitive_keys.contains(k) {
                    (k.clone(), crate::sensitive::mask_value(v))
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Run state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: Uuid,
    pub name: String,
    pub project: String,
    pub status: RunStatus,
    pub nodes: HashMap<String, NodeState>,
    /// Node keys in the order they were started (for reverse-order stop).
    #[serde(default)]
    pub execution_order: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
}

impl RunState {
    pub fn new(name: &str, project: &str) -> Self {
        Self {
            run_id: Uuid::new_v4(),
            name: name.to_owned(),
            project: project.to_owned(),
            status: RunStatus::Starting,
            nodes: HashMap::new(),
            execution_order: Vec::new(),
            created_at: Utc::now(),
            stopped_at: None,
        }
    }

    /// Key for the node state map: `"node:variant"`.
    pub fn node_key(node: &str, variant: &str) -> String {
        format!("{node}:{variant}")
    }
}

// ---------------------------------------------------------------------------
// Project state file (.veld/state.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectState {
    pub runs: HashMap<String, RunState>,
}

impl ProjectState {
    /// Load from the `.veld/state.json` file under `project_root`.
    /// Sensitive output values are decrypted after loading.
    pub fn load(project_root: &Path) -> Result<Self, StateError> {
        let path = state_file_path(project_root);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path).map_err(|e| StateError::ReadError {
            path: path.clone(),
            source: e,
        })?;
        let mut state: Self =
            serde_json::from_str(&data).map_err(|e| StateError::ParseError { path, source: e })?;

        // Decrypt sensitive outputs after loading.
        for run in state.runs.values_mut() {
            for node in run.nodes.values_mut() {
                node.decrypt_sensitive_outputs();
            }
        }

        Ok(state)
    }

    /// Persist to `.veld/state.json`.
    /// Sensitive output values are encrypted before writing.
    pub fn save(&self, project_root: &Path) -> Result<(), StateError> {
        let path = state_file_path(project_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StateError::WriteError {
                path: path.clone(),
                source: e,
            })?;
        }

        // Clone and encrypt sensitive values before serializing.
        let mut state_for_disk = self.clone();
        for run in state_for_disk.runs.values_mut() {
            for node in run.nodes.values_mut() {
                node.encrypt_sensitive_outputs();
            }
        }

        let data =
            serde_json::to_string_pretty(&state_for_disk).expect("state serialization cannot fail");

        atomic_write(&path, &data)
    }

    pub fn get_run(&self, name: &str) -> Option<&RunState> {
        self.runs.get(name)
    }

    pub fn get_run_mut(&mut self, name: &str) -> Option<&mut RunState> {
        self.runs.get_mut(name)
    }
}

fn state_file_path(project_root: &Path) -> PathBuf {
    project_root.join(".veld").join("state.json")
}

/// Write `data` to `path` atomically via a temp file + rename.
/// The temp file lives in the same directory so the rename never crosses
/// filesystem boundaries.
fn atomic_write(path: &Path, data: &str) -> Result<(), StateError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        seq,
    );
    let tmp_path = path.with_file_name(tmp_name);
    std::fs::write(&tmp_path, data).map_err(|e| StateError::WriteError {
        path: tmp_path.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        StateError::WriteError {
            path: path.to_path_buf(),
            source: e,
        }
    })
}

// ---------------------------------------------------------------------------
// Global registry (~/Library/Application Support/veld/registry.json etc.)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub project_root: PathBuf,
    pub project_name: String,
    pub runs: HashMap<String, RegistryRunInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryRunInfo {
    pub run_id: Uuid,
    pub name: String,
    pub status: RunStatus,
    pub urls: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalRegistry {
    pub projects: HashMap<String, RegistryEntry>,
}

impl GlobalRegistry {
    pub fn registry_path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("veld").join("registry.json"))
    }

    pub fn load() -> Result<Self, StateError> {
        let path = match Self::registry_path() {
            Some(p) => p,
            None => return Ok(Self::default()),
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path).map_err(|e| StateError::ReadError {
            path: path.clone(),
            source: e,
        })?;
        serde_json::from_str(&data).map_err(|e| StateError::ParseError { path, source: e })
    }

    pub fn save(&self) -> Result<(), StateError> {
        let path = match Self::registry_path() {
            Some(p) => p,
            None => return Ok(()),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StateError::WriteError {
                path: path.clone(),
                source: e,
            })?;
        }
        let data = serde_json::to_string_pretty(self).expect("registry serialization cannot fail");

        atomic_write(&path, &data)
    }
}
