//! Run/node state data types.
//!
//! Persistence lives in [`crate::db`] — one central SQLite database replaces
//! the old per-project `.veld/state.json` and global `registry.json` files.

use std::collections::{BTreeMap, HashMap};
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
    /// The primary HTTP port's URL. `None` for a `command` node and for a
    /// `long_running` node that declares `"ports": null` or only `tcp` ports.
    pub url: Option<String>,
    /// Every routed HTTP port, keyed by port name — the primary included, so
    /// this is the complete list and `url` is a convenience view of one entry.
    ///
    /// **Teardown must iterate this, not `url`.** Each entry owns a DNS host and
    /// a Caddy route; a hostname missed at stop time leaves a permanent
    /// `/etc/hosts` line and a route that shadows that name for every later run.
    /// Absent on rows written before multi-port routing, which is exactly the
    /// single-`url` case, so `hostnames()` folds `url` back in.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub urls: BTreeMap<String, String>,
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
            urls: BTreeMap::new(),
            outputs: HashMap::new(),
            readiness_phases: Vec::new(),
            recovery_count: 0,
            consecutive_failures: 0,
            last_liveness_error: None,
            sensitive_keys: Vec::new(),
        }
    }

    /// Every URL this node serves, primary first.
    ///
    /// `None` in the first slot marks the primary — the one `${veld.url}` means
    /// and the one a single-port node has always had; `Some(port_name)` is a
    /// secondary `protocol: "http"` port. The primary is matched *by value*, so
    /// a row persisted before per-port routing (empty `urls`, populated `url`)
    /// yields exactly one entry and every display keeps its old shape.
    ///
    /// Every surface that shows a node's URL goes through here. Routing a
    /// hostname that no command prints is a hostname nobody can discover.
    pub fn routed_urls(&self) -> Vec<(Option<&str>, &str)> {
        let mut out: Vec<(Option<&str>, &str)> = Vec::new();
        if let Some(primary) = &self.url {
            out.push((None, primary.as_str()));
        }
        for (name, url) in &self.urls {
            if Some(url) == self.url.as_ref() {
                continue;
            }
            out.push((Some(name.as_str()), url.as_str()));
        }
        out
    }

    /// Every hostname this node claimed, port-stripped and deduplicated — the
    /// one list teardown, GC and the collision check must all walk.
    ///
    /// Folds `url` in rather than trusting `urls` alone, so a run persisted
    /// before multi-port routing still tears its single route down.
    pub fn hostnames(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for u in self.urls.values().chain(self.url.iter()) {
            let host = crate::url::hostname_of_url(u).to_owned();
            if !out.contains(&host) {
                out.push(host);
            }
        }
        out
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
// Graph snapshot — what a run was started WITH, captured at start time so
// "what changed between the run that worked and the run that didn't" stays
// answerable after veld.json has moved on. Deliberately PRE-interpolation:
// command strings keep their `${...}` placeholders and env is names-only, so
// no resolved value (port, URL, secret output) is ever persisted here.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSnapshot {
    /// SHA-256 of the veld.json bytes at start time — equal hashes mean the
    /// whole config file was identical, before any per-node diffing.
    pub config_hash: String,
    /// How the run was asked for. `None` on rows written before this existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_from: Option<StartOrigin>,
    /// Node keys (`"node:variant"`) → what the config said about them.
    /// BTreeMap for stable serialization (diff-friendly).
    pub nodes: std::collections::BTreeMap<String, NodeSnapshot>,
    /// Which machine-overridable vars this machine was answering, and from which
    /// scope — **names and provenance only, never values**.
    ///
    /// This exists because `config_hash` hashes the veld.json *bytes*, and a
    /// machine override changes the effective configuration without changing one
    /// of them. Two runs of the same commit that behaved differently would
    /// otherwise be reported as identical, which is the single most confusing
    /// thing this feature could do to `veld runs diff`.
    ///
    /// **No value and no hash of one.** A fingerprint was the tempting middle
    /// ground and is worse than either end: the values people override are
    /// low-entropy (`true`, `5432`, a handful of plausible hostnames), so a
    /// truncated digest over that domain is a brute-forceable oracle published
    /// into the most-copied, most-pasted artifact veld produces. The names tell
    /// you where to look, which is the honest answer.
    /// **`None` means the run predates this field; `Some([])` means the project
    /// declared no machine vars.** The distinction is carried by the type rather
    /// than by an empty vec, for the reason `presets` in the daemon's worktree
    /// view carries the same one: a consumer that treats "absent" as "none
    /// declared" reports every var as newly-appeared when you diff a run recorded
    /// before the upgrade — a difference that did not happen. Empty and absent
    /// are not the same fact, and only the type can say so.
    #[serde(default)]
    pub var_overrides: Option<Vec<VarOverrideSnapshot>>,
}

/// One machine-overridable var as it stood when a run started.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarOverrideSnapshot {
    /// The var's name, as declared in `vars`.
    pub name: String,
    /// Where the effective value came from: `"project"` or `"worktree"` for a
    /// stored answer, `"flag"` for a `--var` given to this run alone, and
    /// `"default"` when the config's own value was used.
    ///
    /// `"flag"` is not a scope and no row backs it — the distinction matters
    /// because a per-run answer is the most volatile difference two runs of the
    /// same commit can have, and calling it `"default"` would attribute it to a
    /// file that never contained it.
    pub from: String,
}

/// The invocation that started a run, recorded so a surface can say *what it
/// was started from* rather than only what it resolved to.
///
/// **Both halves are required, and that is the whole design.** A preset name on
/// its own is true for one instant: presets are re-read from disk on every use,
/// so the name can be renamed, deleted, or have its selections edited while the
/// run is live, and a bare stored name would then be a durable, authoritative-
/// looking falsehood in run history and in `--json`. Keeping the expansion the
/// name meant *at start time* is what lets a reader re-expand the preset today
/// and report `redefined since start` instead of asserting something false.
///
/// A run started from explicit tokens has `preset: None` — the absence is
/// meaningful ("this did not come from a preset"), not missing data.
///
/// **The cost of riding in the snapshot blob instead of its own column**, stated so
/// nobody rediscovers it: there is no migration and therefore no `NewerSchema`
/// downgrade cliff, but an *older* binary that loads and re-saves a run row written
/// by this one drops this field on deserialize (nothing here is
/// `deny_unknown_fields`, so it is not even an error) and writes the snapshot back
/// without it. The monitor and GC both load-then-save, so a downgrade quietly erases
/// provenance for the runs it touches. Acceptable for an advisory field that every
/// surface renders as "no line at all" when absent; not acceptable for anything a
/// decision depends on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOrigin {
    /// The preset's config name (never its `key` — a key is a display
    /// convenience that can move when presets are added).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// The `node:variant` tokens the invocation resolved to, sorted. Compare
    /// against a fresh expansion of `preset` to detect a redefined preset.
    #[serde(default)]
    pub selections: Vec<String>,
}

impl StartOrigin {
    /// Record an invocation: the preset's config name (or `None`) plus the
    /// selections it resolved to.
    ///
    /// `selections` must be what the caller *asked for*, not the
    /// dependency-closed start plan. A preset expands to the selections it
    /// names, so recording the closure would make a run look `redefined` the
    /// moment any selected node had a dependency — a false alarm on the most
    /// ordinary config there is.
    #[must_use]
    pub fn new(preset: Option<String>, selections: &[crate::graph::NodeSelection]) -> Self {
        let mut tokens: Vec<String> = selections
            .iter()
            .map(|s| RunState::node_key(&s.node, &s.variant))
            .collect();
        tokens.sort();
        tokens.dedup();
        Self {
            preset,
            selections: tokens,
        }
    }
}

/// How a run's node command was expressed, preserved **structurally**.
///
/// Not a joined string: `["psql", "a b"]` and `["psql", "a", "b"]` would be
/// diff-identical once flattened, so `veld runs diff` could not tell one from the
/// other — and those are two different commands. Keeping the argv boundaries is
/// the whole reason this is an enum.
///
/// Reads are deliberately lenient: rows written before this existed hold a plain
/// string, and a `veld runs` listing must never fail on one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSnapshot {
    /// An argument vector, spawned directly.
    Argv(Vec<String>),
    /// A shell string, run via `sh -c`.
    Shell(String),
    /// A script file, run via `sh <path>`.
    Script(String),
}

impl CommandSnapshot {
    /// Rendering for `veld runs show` / `diff`. An `argv` keeps its list form so
    /// a boundary change is visible in the diff rather than smoothed away.
    pub fn display(&self) -> String {
        match self {
            CommandSnapshot::Argv(argv) => format!("{argv:?}"),
            CommandSnapshot::Shell(s) => s.clone(),
            CommandSnapshot::Script(p) => format!("script:{p}"),
        }
    }
}

impl From<crate::config::CommandSpec> for CommandSnapshot {
    fn from(spec: crate::config::CommandSpec) -> Self {
        match spec {
            crate::config::CommandSpec::Argv(a) => CommandSnapshot::Argv(a),
            crate::config::CommandSpec::Shell(s) => CommandSnapshot::Shell(s),
        }
    }
}

impl Serialize for CommandSnapshot {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut m = s.serialize_map(Some(1))?;
        match self {
            CommandSnapshot::Argv(a) => m.serialize_entry("argv", a)?,
            CommandSnapshot::Shell(sh) => m.serialize_entry("shell", sh)?,
            CommandSnapshot::Script(p) => m.serialize_entry("script", p)?,
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for CommandSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        match serde_json::Value::deserialize(d)? {
            // Rows written before this enum existed: a plain string, with script
            // paths marked by a `script:` prefix.
            //
            // **There is no migration that rewrites these, and this arm is
            // therefore load-bearing forever — do not delete it.** A rewrite
            // migration was considered and skipped: the format change is confined
            // to one JSON column read only by `veld runs show`/`diff`, so a lenient
            // read costs nothing, while a data-rewriting migration over every
            // historical row is risk with no payoff. A database restored from an
            // older backup also has to keep working.
            serde_json::Value::String(s) => Ok(match s.strip_prefix("script:") {
                Some(path) => CommandSnapshot::Script(path.to_owned()),
                None => CommandSnapshot::Shell(s),
            }),
            serde_json::Value::Object(map) => {
                let (key, val) = map
                    .into_iter()
                    .next()
                    .ok_or_else(|| D::Error::custom("empty command snapshot"))?;
                match key.as_str() {
                    "argv" => {
                        let argv: Vec<String> =
                            serde_json::from_value(val).map_err(D::Error::custom)?;
                        Ok(CommandSnapshot::Argv(argv))
                    }
                    "shell" => Ok(CommandSnapshot::Shell(
                        val.as_str()
                            .ok_or_else(|| D::Error::custom("\"shell\" must be a string"))?
                            .to_owned(),
                    )),
                    "script" => Ok(CommandSnapshot::Script(
                        val.as_str()
                            .ok_or_else(|| D::Error::custom("\"script\" must be a string"))?
                            .to_owned(),
                    )),
                    other => Err(D::Error::custom(format!(
                        "unknown command snapshot form \"{other}\""
                    ))),
                }
            }
            _ => Err(D::Error::custom(
                "command snapshot must be a string or an { argv | shell | script } object",
            )),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSnapshot {
    /// `command` or `start_server`.
    pub step_type: String,
    /// The command with `${...}` placeholders intact, exactly as configured —
    /// never interpolated, and never flattened (see [`CommandSnapshot`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Configured env variable NAMES (sorted). Values are never stored —
    /// they can be secrets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_template: Option<String>,
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
    /// The resolved graph this run was started with (pre-interpolation; see
    /// [`GraphSnapshot`]). `None` on pre-snapshot rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_snapshot: Option<GraphSnapshot>,
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
            graph_snapshot: None,
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
