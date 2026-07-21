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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Starting,
    Running,
    /// An ending is in progress: `end_reason` records the intent, teardown is
    /// still running. Set by `Db::begin_ending` *before* any PID is killed so
    /// crash detectors (which scan only `starting`/`running`) never mislabel a
    /// deliberate stop.
    Stopping,
    Stopped,
    Failed,
    /// The run's processes died without anyone asking them to.
    Crashed,
}

impl RunStatus {
    /// A run that occupies its environment's single live slot (enforced by the
    /// `idx_runs_one_live` partial unique index). Everything else is history.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }
}

// ---------------------------------------------------------------------------
// End reason — why a run left the live set. NULL/None while live; written
// once by the first ender (`begin_ending` / crash detection) and never
// changed afterwards.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// Deliberate `veld stop` (CLI or UI).
    Stopped,
    /// Startup aborted, or a `--oneshot` terminal node exited non-zero.
    Failed,
    /// A node process died without being asked to.
    Crashed,
    /// A same-name `veld start` superseded this run.
    Replaced,
    /// A `--oneshot` terminal node exited zero.
    Completed,
}

impl EndReason {
    /// The terminal `RunStatus` a run reaches when finalized with this reason.
    pub fn terminal_status(&self) -> RunStatus {
        match self {
            EndReason::Failed => RunStatus::Failed,
            EndReason::Crashed => RunStatus::Crashed,
            EndReason::Stopped | EndReason::Replaced | EndReason::Completed => RunStatus::Stopped,
        }
    }
}

/// Machine-readable outcome detail, at run level because the failing thing is
/// not always a node (a setup step has no node row).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndDetail {
    /// Project-level setup/teardown step that failed, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    /// `"node:variant"` key of the node that failed or whose PID died.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_node: Option<String>,
    /// Exit code, where one was observable (command/oneshot nodes; never
    /// crashed servers — veld does not `waitpid` detached processes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
    /// Durable instance identity — the lookup key for history, logs, and
    /// shares. One environment (`(project_root, name)`) accumulates many runs.
    pub run_id: Uuid,
    /// The environment name (`--name`). Identity of the durable slot, not of
    /// this particular execution.
    pub name: String,
    pub project: String,
    pub status: RunStatus,
    /// Why the run ended (or is ending — set at `begin_ending`, before
    /// teardown). `None` while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<EndReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_detail: Option<EndDetail>,
    pub nodes: HashMap<String, NodeState>,
    /// Node keys in the order they were started (for reverse-order stop).
    #[serde(default)]
    pub execution_order: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// When the run reached a terminal status (was `stopped_at` before v3).
    #[serde(default, alias = "stopped_at")]
    pub ended_at: Option<DateTime<Utc>>,
}

impl RunState {
    pub fn new(name: &str, project: &str) -> Self {
        Self {
            run_id: Uuid::new_v4(),
            name: name.to_owned(),
            project: project.to_owned(),
            status: RunStatus::Starting,
            end_reason: None,
            end_detail: None,
            nodes: HashMap::new(),
            execution_order: Vec::new(),
            created_at: Utc::now(),
            ended_at: None,
        }
    }

    /// Key for the node state map: `"node:variant"`.
    pub fn node_key(node: &str, variant: &str) -> String {
        format!("{node}:{variant}")
    }

    /// Whether this run occupies its environment's live slot.
    pub fn is_live(&self) -> bool {
        self.status.is_live()
    }

    /// Git-style short id: the first hex block of the UUID (8 chars), enough
    /// to address a run within the retention window.
    pub fn short_id(&self) -> String {
        let s = self.run_id.to_string();
        s[..s.find('-').unwrap_or(8)].to_owned()
    }

    /// One-line outcome for tables and status output, e.g.
    /// `failed (setup: db-migrate, exit 1)` or `crashed (api:local pid died)`.
    pub fn outcome_label(&self) -> String {
        let Some(reason) = &self.end_reason else {
            return match self.status {
                RunStatus::Starting => "starting".to_owned(),
                RunStatus::Running => "running".to_owned(),
                _ => "stopping".to_owned(),
            };
        };
        let base = match reason {
            EndReason::Stopped => "stopped",
            EndReason::Failed => "failed",
            EndReason::Crashed => "crashed",
            EndReason::Replaced => "replaced",
            EndReason::Completed => "completed",
        };
        let mut parts: Vec<String> = Vec::new();
        if let Some(d) = &self.end_detail {
            if let Some(step) = &d.failed_step {
                parts.push(format!("setup: {step}"));
            }
            if let Some(node) = &d.failed_node {
                if *reason == EndReason::Crashed {
                    parts.push(format!("{node} pid died"));
                } else {
                    parts.push(node.clone());
                }
            }
            if let Some(code) = d.exit_code {
                parts.push(format!("exit {code}"));
            }
        }
        if parts.is_empty() {
            base.to_owned()
        } else {
            format!("{base} ({})", parts.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// Project state — the latest run of each environment in one project.
// (Run history is behind `Db::list_runs` / `Db::get_run_by_id_prefix`.)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectState {
    /// Keyed by environment name; the value is that environment's latest run
    /// (live if one is live, otherwise the most recently started).
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
