//! Run/node state data types.
//!
//! Persistence lives in [`crate::db`] — one central SQLite database replaces
//! the old per-project `.veld/state.json` and global `registry.json` files.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Run status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Starting,
    Running,
    /// A recovery cycle is in progress for one or more nodes.
    Recovering,
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
    /// Liveness probe failed but recovery has not yet been exhausted.
    Unhealthy,
    Failed,
    Stopped,
    Skipped,
}

// ---------------------------------------------------------------------------
// Readiness phase tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessPhase {
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
    /// Readiness probe phase tracking (renamed from `health_phases` in v7).
    #[serde(default, alias = "health_phases")]
    pub readiness_phases: Vec<ReadinessPhase>,
    /// Number of recovery attempts completed for this node.
    #[serde(default)]
    pub recovery_count: u32,
    /// Current streak of consecutive liveness probe failures.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Error message from the most recent liveness probe failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_liveness_error: Option<String>,
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
            readiness_phases: Vec::new(),
            recovery_count: 0,
            consecutive_failures: 0,
            last_liveness_error: None,
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
// Project state (all runs of one project)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectState {
    pub runs: HashMap<String, RunState>,
}

impl ProjectState {
    pub fn get_run(&self, name: &str) -> Option<&RunState> {
        self.runs.get(name)
    }

    pub fn get_run_mut(&mut self, name: &str) -> Option<&mut RunState> {
        self.runs.get_mut(name)
    }
}

// ---------------------------------------------------------------------------
// Global registry — derived from the database (see `Db::registry`)
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
