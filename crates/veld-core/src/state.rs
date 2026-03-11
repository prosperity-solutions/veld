use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
        }
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
    pub fn load(project_root: &Path) -> Result<Self, StateError> {
        let path = state_file_path(project_root);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path).map_err(|e| StateError::ReadError {
            path: path.clone(),
            source: e,
        })?;
        serde_json::from_str(&data).map_err(|e| StateError::ParseError { path, source: e })
    }

    /// Persist to `.veld/state.json`.
    pub fn save(&self, project_root: &Path) -> Result<(), StateError> {
        let path = state_file_path(project_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StateError::WriteError {
                path: path.clone(),
                source: e,
            })?;
        }
        let data = serde_json::to_string_pretty(self).expect("state serialization cannot fail");
        std::fs::write(&path, data).map_err(|e| StateError::WriteError { path, source: e })
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
        std::fs::write(&path, data).map_err(|e| StateError::WriteError { path, source: e })
    }
}
