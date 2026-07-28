#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;
use tracing;

use crate::config::{self, Outputs, StepType, VeldConfig};
use crate::db::{Db, LogFilter, LogStream};
use crate::graph::{self, NodeSelection};
use crate::health;
use crate::helper::HelperClient;
use crate::logging::{self, LogWriter};
use crate::port::PortAllocator;
use crate::process;
use crate::progress::ProgressEvent;
use crate::state::{
    EndDetail, EndReason, NodeState, NodeStatus, ReadinessPhase, RunState, RunStatus,
};
use crate::url;
use crate::variables::VariableContext;

use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    #[error(transparent)]
    Graph(#[from] graph::GraphError),

    #[error(transparent)]
    Port(#[from] crate::port::PortError),

    #[error(transparent)]
    Process(#[from] process::ProcessError),

    #[error(transparent)]
    Health(#[from] health::HealthError),

    #[error(transparent)]
    Db(#[from] crate::db::DbError),

    #[error(transparent)]
    Variable(#[from] crate::variables::VariableError),

    /// A configured value source could not be dereferenced at run start — an
    /// unset `env` var, an unreadable file, a failing or hanging helper.
    #[error(transparent)]
    Value(#[from] crate::values::ValueError),

    #[error(transparent)]
    Helper(#[from] crate::helper::HelperError),

    #[error(transparent)]
    Log(#[from] logging::LogError),

    #[error("node {node}:{variant} failed: {reason}")]
    NodeFailed {
        node: String,
        variant: String,
        reason: String,
    },

    #[error("setup step '{name}' failed: {reason}")]
    SetupFailed {
        name: String,
        reason: String,
        failure_message: Option<String>,
    },

    #[error("environment '{0}' was replaced by another `veld start` while starting")]
    Superseded(String),

    /// Semantic config problems, from `config::validate`. Reachable only from
    /// `start` — never from the loader, so `stop`/`status`/`logs` still work
    /// against a config that has since been broken.
    #[error("{0}")]
    ConfigInvalid(String),
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// The main orchestration engine.
/// Result of a stop operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopResult {
    /// The run was actively stopped (processes killed, routes removed).
    Stopped,
    /// The run was already stopped; state was cleaned up.
    AlreadyStopped,
}

/// Decide whether `cleanup_dead_runs` should reap a run as a dead orphan.
///
/// A run is now persisted as `Starting` *before* its first stage spawns any
/// process, and pure `command` stages never record a PID at all — so a live,
/// still-starting run legitimately has no alive PIDs for an unbounded window
/// (slow setup, a long `command`/build first stage). Liveness alone therefore
/// cannot distinguish such a run from an orphan, and any time-based grace would
/// eventually reap a genuinely-starting run.
///
/// So we reap only what we can prove is dead:
/// - `Running` with no live PIDs — startup finished and its processes have since
///   died (orphaned by a crash / `kill -9`), or it was a `command`-only env that
///   never held a long-lived process. Either way there is nothing alive to keep.
/// - `Starting` **that has already spawned** a process (recorded a PID) which
///   is now dead — it got underway, then its process died.
///
/// A `Starting` run that has never spawned is left alone: it leaks no processes
/// or routes, and a same-name `veld start` (`cleanup_stale_run`) or `veld stop`
/// clears it. Any other status is never reaped here: `Stopping` belongs to an
/// ender that is still tearing down (the daemon's grace-gated stale-`stopping`
/// reaper covers a SIGKILLed one), and terminal runs are history.
fn is_reapable_orphan(status: &RunStatus, any_alive: bool, ever_spawned: bool) -> bool {
    if any_alive {
        return false;
    }
    match status {
        RunStatus::Running => true,
        RunStatus::Starting => ever_spawned,
        _ => false,
    }
}

/// Best-effort kill of a set of PIDs, then a bounded wait for them to die.
/// Returns whether every PID is confirmed dead. Callers finalize a run only
/// on `true`; on `false` the run keeps a live/`stopping` status so a reaper
/// still covers the leaked process (leak-freedom never depends on the label).
async fn kill_and_confirm(pids: &[u32]) -> bool {
    for &pid in pids {
        if process::is_alive(pid) {
            let _ = process::kill_process(pid).await;
        }
    }
    for _ in 0..10 {
        if pids.iter().all(|&p| !process::is_alive(p)) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    pids.iter().all(|&p| !process::is_alive(p))
}

/// Capture what this run is being started WITH — the pre-interpolation
/// resolved graph (see [`crate::state::GraphSnapshot`]). Raw command strings
/// keep their `${...}` placeholders and env is names-only, so no resolved
/// value (port, URL, secret output) is ever persisted.
fn build_graph_snapshot(
    config: &VeldConfig,
    config_hash: String,
    plan: &[Vec<NodeSelection>],
) -> crate::state::GraphSnapshot {
    let mut nodes = std::collections::BTreeMap::new();
    // The FULL resolved graph, deliberately including the oneshot terminal
    // node (which never gets a node row of its own) — the snapshot describes
    // what was planned, not what spawned.
    for sel in plan.iter().flatten() {
        let Some(node_cfg) = config.nodes.get(&sel.node) else {
            continue;
        };
        let Some(variant_cfg) = node_cfg.variants.get(&sel.variant) else {
            continue;
        };
        // Resolved, so the snapshot records what the run actually used — a value
        // hoisted to node level would otherwise read as absent.
        let resolved = config::resolve_variant(config, node_cfg, variant_cfg);
        let command = match &resolved.script {
            Some(script) => Some(crate::state::CommandSnapshot::Script(script.clone())),
            None => resolved.command.clone().map(Into::into),
        };
        let mut env_keys: Vec<String> = resolved
            .env
            .as_ref()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        env_keys.sort();
        let url_template = (resolved.step_type == config::StepType::StartServer).then(|| {
            url::resolve_url_template(
                &config.url_template,
                node_cfg.url_template.as_deref(),
                variant_cfg.url_template.as_deref(),
            )
            .to_owned()
        });
        nodes.insert(
            RunState::node_key(&sel.node, &sel.variant),
            crate::state::NodeSnapshot {
                step_type: match resolved.step_type {
                    config::StepType::Command => "command".to_owned(),
                    config::StepType::StartServer => "start_server".to_owned(),
                },
                command,
                cwd: variant_cfg.cwd.clone().or_else(|| node_cfg.cwd.clone()),
                env_keys,
                url_template,
            },
        );
    }
    crate::state::GraphSnapshot { config_hash, nodes }
}

/// The inputs every `veld.*` builtin is derived from.
///
/// One constructor for all three interpolation paths (startup stage, `--oneshot`
/// terminal node, `on_stop` teardown) so the same `${veld.…}` string cannot
/// resolve differently — or fail — depending on which path expanded it. Before
/// this, `${veld.node}` existed only in the action context and each path
/// hand-rolled a slightly different set.
///
/// `veld.*` is a **closed** set: node outputs are NOT injected here. They live
/// in `${output.*}` (own node) and `${nodes.<node>.<field>}` (any node). Merging
/// them into builtins let an output named `port`, `url`, `run`, `node`, or
/// `branch` shadow the builtin of the same name — silently, and only on the
/// paths that did the merging.
struct BuiltinScope<'a> {
    run_name: &'a str,
    /// Stringified run-instance UUID. `None` only where no run exists yet.
    run_id: Option<String>,
    project_root: &'a Path,
    project_name: &'a str,
    /// Raw worktree directory name; slugified on the way in.
    worktree: &'a str,
    /// Raw git branch; slugified on the way in.
    branch: &'a str,
    username: &'a str,
    /// Node and variant this context belongs to, when it is node-scoped.
    /// `None` for project-level setup/teardown steps, which belong to no node.
    node: Option<(&'a str, &'a str)>,
}

impl BuiltinScope<'_> {
    fn apply(&self, ctx: &mut VariableContext) {
        ctx.set_builtin("run", self.run_name.to_owned());
        if let Some(id) = &self.run_id {
            ctx.set_builtin("run_id", id.clone());
        }
        ctx.set_builtin("root", self.project_root.to_string_lossy().into_owned());
        ctx.set_builtin("project", self.project_name.to_owned());
        ctx.set_builtin("name", self.project_name.to_owned());
        ctx.set_builtin("worktree", url::slugify(self.worktree));
        ctx.set_builtin("branch", url::slugify(self.branch));
        ctx.set_builtin("username", self.username.to_owned());
        if let Some((node, variant)) = self.node {
            ctx.set_builtin("node", node.to_owned());
            ctx.set_builtin("variant", variant.to_owned());
        }
    }
}

/// Machine-readable outcome detail for a failed start.
fn end_detail_for_error(e: &OrchestratorError) -> EndDetail {
    let mut detail = EndDetail::default();
    match e {
        OrchestratorError::NodeFailed { node, variant, .. } => {
            detail.failed_node = Some(format!("{node}:{variant}"));
        }
        OrchestratorError::SetupFailed { name, .. } => {
            detail.failed_step = Some(name.clone());
        }
        _ => {}
    }
    detail.message = Some(e.to_string().chars().take(500).collect());
    detail
}

/// Pre-computed port and URL for a `start_server` node, resolved before
/// any node begins execution so that all nodes can reference any other
/// node's URL/port without requiring a dependency edge.
struct PrecomputedServer {
    /// The primary port — what `${veld.port}` and `VELD_PORT` resolve to, and
    /// what Caddy proxies to. A node that declares no `ports` map has exactly
    /// this one, as it always did.
    port: u16,
    /// Every named port from the `ports` map, including the primary, as
    /// `${veld.ports.<name>}`. Empty when the node declares no map.
    named_ports: std::collections::BTreeMap<String, u16>,
    /// Raw hostname (without scheme), used for DNS/Caddy configuration.
    hostname: String,
    /// Full `https://` URL including port suffix when not 443.
    https_url: String,
    /// Held TCP listeners that reserve the ports from other processes.
    /// Taken (released) right before the child process is spawned.
    reservation: Option<crate::port::PortReservation>,
    /// Reservations for the non-primary named ports, released alongside it.
    extra_reservations: Vec<crate::port::PortReservation>,
}

/// Read-only context shared by all node execution tasks within a stage.
/// Cloned once per stage, then each spawned task gets its own copy.
#[derive(Clone)]
struct NodeExecutionContext {
    config: Arc<VeldConfig>,
    db: Db,
    project_root: Arc<PathBuf>,
    https_port: u16,
    foreground: bool,
    helper_client: HelperClient,
    progress_tx: Option<mpsc::UnboundedSender<ProgressEvent>>,
    debug_writer: Option<LogWriter>,
    run_name: String,
    run_id: uuid::Uuid,
    branch: String,
    worktree: String,
    username: String,
    /// Snapshot of all outputs from prior stages for variable resolution.
    all_outputs: Arc<HashMap<String, HashMap<String, String>>>,
    /// Project `vars`, resolved once for the whole run so two use sites of the
    /// same value can never disagree.
    vars: Arc<HashMap<String, String>>,
    /// Shared run state for PID checkpointing. Uses `std::sync::Mutex`
    /// (not tokio) so the lock is acquired without an `.await` point —
    /// this makes the spawn→checkpoint sequence cancellation-safe.
    checkpoint: Arc<std::sync::Mutex<CheckpointState>>,
}

/// Shared mutable state for PID checkpointing during parallel execution.
struct CheckpointState {
    run: RunState,
    project_root: PathBuf,
}

/// Result of executing a single node, collected after the task completes.
struct NodeExecutionResult {
    key: String,
    sel: NodeSelection,
    index: usize,
    node_state: NodeState,
    server_handle: Option<process::ServerHandle>,
}

pub struct Orchestrator {
    pub config: VeldConfig,
    pub config_path: PathBuf,
    /// SHA-256 of the veld.json bytes, hashed once at construction — as close
    /// to the parse as we can get without changing the config-loading API, so
    /// the snapshot's hash describes (within microseconds of) the bytes that
    /// became `config`, not whatever is on disk seconds later when `start`'s
    /// cleanup phases have finished.
    config_hash: String,
    pub project_root: PathBuf,
    pub db: Db,
    pub port_allocator: PortAllocator,
    pub helper_client: HelperClient,
    /// The HTTPS port that the helper's Caddy listens on (queried at start).
    pub https_port: u16,
    /// Active child processes keyed by `"node:variant"`.
    children: HashMap<String, process::ServerHandle>,
    /// Pre-computed ports and URLs for all `start_server` nodes, keyed by
    /// `"node:variant"`. Populated once before execution begins so that
    /// every node can reference any `start_server` node's `url`/`port`
    /// regardless of dependency order.
    precomputed_servers: HashMap<String, PrecomputedServer>,
    /// Debug mode — writes orchestration trace to `veld-debug.log`.
    debug: bool,
    /// Debug log writer (created on demand when debug is true).
    debug_writer: Option<LogWriter>,
    /// Foreground mode — pipes stdout/stderr through timestamping tasks.
    /// When false (detached), redirects directly to file so processes survive CLI exit.
    foreground: bool,
    /// Optional channel for live progress events.
    progress_tx: Option<mpsc::UnboundedSender<ProgressEvent>>,
    /// Internal log writer for the current run (liveness/recovery/lifecycle events).
    internal_log: Option<LogWriter>,
    /// When set, this `command` node is the run's terminal one-off: it is NOT
    /// executed during the normal startup stages (its dependencies are), and is
    /// instead run afterwards via [`Orchestrator::run_terminal`]. Its exit ends
    /// the run (see `veld start --oneshot`).
    terminal_node: Option<NodeSelection>,
    /// Project `vars`, resolved once during `start`. Stashed so `run_terminal` and
    /// the `on_stop` path reuse the same values rather than re-running a source
    /// command — two resolutions of a rotating credential would disagree.
    resolved_vars: Option<Arc<HashMap<String, String>>>,
    /// Dependency outputs captured at the end of `start` when a terminal node
    /// is set, so `run_terminal` can interpolate `${nodes.X.url}` etc. with the
    /// exact values the stages produced (no reconstruction drift).
    terminal_outputs: Option<HashMap<String, HashMap<String, String>>>,
}

impl Orchestrator {
    /// Create an orchestrator from a discovered config. Opens (and migrates)
    /// the central veld database.
    pub fn new(config_path: PathBuf, config: VeldConfig) -> Result<Self, OrchestratorError> {
        let project_root = config::project_root(&config_path);
        let db = Db::open()?;
        let config_hash = {
            use sha2::{Digest, Sha256};
            std::fs::read(&config_path)
                .map(|bytes| format!("{:x}", Sha256::digest(&bytes)))
                .unwrap_or_default()
        };
        Ok(Self {
            config,
            config_path,
            config_hash,
            project_root,
            db,
            port_allocator: PortAllocator::new(),
            helper_client: HelperClient::default_client(),
            https_port: 443,
            children: HashMap::new(),
            precomputed_servers: HashMap::new(),
            debug: false,
            debug_writer: None,
            foreground: false,
            progress_tx: None,
            internal_log: None,
            terminal_node: None,
            resolved_vars: None,
            terminal_outputs: None,
        })
    }

    /// Designate a `command` node as the run's terminal one-off (`--oneshot`).
    /// The node is skipped during startup stages and run afterwards; its exit
    /// terminates the run. Only its dependencies are brought up by `start`.
    pub fn set_terminal_node(&mut self, sel: Option<NodeSelection>) {
        self.terminal_node = sel;
    }

    /// Enable foreground mode (timestamped pipe for server output).
    pub fn set_foreground(&mut self, foreground: bool) {
        self.foreground = foreground;
    }

    /// Enable debug mode for orchestration trace logging.
    pub fn set_debug(&mut self, debug: bool) {
        self.debug = debug;
    }

    /// Set the progress event sender for live progress reporting.
    pub fn set_progress_sender(&mut self, tx: mpsc::UnboundedSender<ProgressEvent>) {
        self.progress_tx = Some(tx);
    }

    /// Drop the progress sender, signaling the receiver to close.
    pub fn close_progress_sender(&mut self) {
        self.progress_tx.take();
    }

    /// Emit a progress event (no-op if no sender is set).
    fn emit(&self, event: ProgressEvent) {
        if let Some(ref tx) = self.progress_tx {
            let _ = tx.send(event);
        }
    }

    /// Write a line to the debug log (no-op when debug is off).
    async fn debug_log(&self, message: &str) {
        if let Some(ref writer) = self.debug_writer {
            let _ = writer.write_line(&format!("[VELD] {message}")).await;
        }
    }

    /// Write a line to the internal log (per-run lifecycle events).
    async fn internal_log(&self, message: &str) {
        if let Some(ref writer) = self.internal_log {
            let _ = writer.write_line(message).await;
        }
    }

    /// Convenience: discover config from CWD and build the orchestrator.
    pub fn from_cwd() -> Result<Self, OrchestratorError> {
        let (path, cfg) = config::parse_config_from_cwd()?;
        Self::new(path, cfg)
    }

    // -----------------------------------------------------------------------
    // Start
    // -----------------------------------------------------------------------

    /// Start a run: resolve graph, allocate ports, configure DNS/Caddy,
    /// launch processes in dependency order, run health checks.
    pub async fn start(
        &mut self,
        selections: &[NodeSelection],
        run_name: &str,
    ) -> Result<RunState, OrchestratorError> {
        // Semantic validation runs HERE, not in `parse_config`: a typo must not
        // strand `veld stop` against a running environment (its `on_stop` hooks
        // are read from the on-disk config at stop time). `veld lint` runs the
        // same rules and additionally reports warnings.
        if let Some(msg) = config::error_summary(&config::validate(&self.config)) {
            return Err(OrchestratorError::ConfigInvalid(msg));
        }

        // Clean up any runs whose processes have all died. This catches
        // orphaned runs from previous sessions (crash, kill -9, etc.).
        self.cleanup_dead_runs().await;

        // Clean up any stale run with the same name (kills processes, removes
        // DNS/Caddy routes, clears state). This handles the case where a
        // previous run was not properly cleaned up or the user reuses a name.
        self.cleanup_stale_run(run_name).await;

        // Ensure a helper is running (auto-bootstraps if needed) and
        // query the HTTPS port so we can construct port-aware URLs.
        match crate::setup::ensure_helper().await {
            Ok(client) => {
                if let Ok(port) = client.https_port().await {
                    self.https_port = port;
                }
                self.helper_client = client;
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not ensure helper — using default client");
            }
        }

        // Create internal log writer for this run.
        self.internal_log = Some(LogWriter::for_run(
            self.db.clone(),
            &self.project_root,
            run_name,
            LogStream::Internal,
        ));

        let resolved = graph::resolve_selections(selections, &self.config)?;
        let plan = graph::build_execution_plan(&resolved, &self.config)?;

        // The terminal one-off node (`--oneshot`) is part of the graph — its
        // dependencies are brought up here — but the node itself is executed
        // afterwards by `run_terminal`, so it is skipped in the stage loop and
        // excluded from the healthy-node count. It stays seeded as `Pending`
        // below so the reverse-order teardown path can still find it.
        let terminal_key: Option<String> = self
            .terminal_node
            .as_ref()
            .map(|s| RunState::node_key(&s.node, &s.variant));

        // Set up debug log writer if debug mode is enabled.
        if self.debug {
            let writer = LogWriter::for_run(
                self.db.clone(),
                &self.project_root,
                run_name,
                LogStream::Debug,
            );
            let _ = writer
                .write_line("[VELD] Debug logging enabled — orchestration trace")
                .await;
            self.debug_writer = Some(writer);
        }

        // Ensure Caddy is running before we add routes.
        if let Err(e) = self.helper_client.caddy_start().await {
            tracing::warn!(error = %e, "failed to start Caddy via helper (routes may fail)");
        }
        self.debug_log("Caddy start requested").await;

        let mut run = RunState::new(run_name, &self.config.name);
        // Forensics: record what this run is being started with, so a later
        // `veld runs show/diff` can answer "what changed since the run that
        // worked" even after veld.json moved on.
        run.graph_snapshot = Some(build_graph_snapshot(
            &self.config,
            self.config_hash.clone(),
            &plan,
        ));
        // Scope the run-level log streams to this instance (the writers were
        // created before the run existed).
        if let Some(w) = self.internal_log.as_mut() {
            w.set_run_id(run.run_id);
        }
        if let Some(w) = self.debug_writer.as_mut() {
            w.set_run_id(run.run_id);
        }
        self.debug_log(&format!(
            "Run '{}' created (id: {}), graph has {} stages",
            run_name,
            run.run_id,
            plan.len()
        ))
        .await;

        // Run project-level setup steps before the graph executes. A setup
        // failure happens before the run is persisted, so record it as
        // `failed` history directly — this run never held the live slot.
        if let Err(e) = self.run_setup_steps(run_name).await {
            run.status = RunStatus::Failed;
            run.end_reason = Some(EndReason::Failed);
            run.end_detail = Some(end_detail_for_error(&e));
            run.ended_at = Some(chrono::Utc::now());
            let _ = self.save_state(&run);
            return Err(e);
        }

        // Gather context info for URL templates.
        let branch = url::detect_git_branch(&self.project_root);
        let worktree = self
            .project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default")
            .to_owned();
        let username = whoami_username();
        let hostname = whoami_hostname();

        // Outputs collected as we execute stages (for variable resolution).
        let mut all_outputs: HashMap<String, HashMap<String, String>> = HashMap::new();

        // Pre-compute ports and URLs for ALL start_server nodes before any
        // execution begins.  This makes ${nodes.X.url} and ${nodes.X.port}
        // available to every node regardless of dependency order — frontend
        // can reference backend's URL and vice versa without a cycle.
        self.precomputed_servers.clear();
        for stage in &plan {
            for sel in stage {
                let node_cfg = &self.config.nodes[&sel.node];
                let variant_cfg = &node_cfg.variants[&sel.variant];
                let resolved = config::resolve_variant(&self.config, node_cfg, variant_cfg);
                if resolved.step_type != config::StepType::StartServer {
                    continue;
                }

                // One allocation per declared name. A node with no `ports` map
                // gets exactly one, exactly as before F6 — the whole point is
                // that debug-adapter and multi-port container variants stop
                // needing hand-picked literal ports, which silently break
                // parallel worktrees.
                let mut named_ports = std::collections::BTreeMap::new();
                let mut extra_reservations = Vec::new();
                let mut primary_reservation = None;
                match resolved.ports.as_ref() {
                    None => {
                        primary_reservation = Some(self.port_allocator.allocate()?);
                    }
                    Some(declared) => {
                        for (name, spec) in &declared.ports {
                            let reservation = match spec {
                                config::PortSpec::Auto => self.port_allocator.allocate()?,
                                config::PortSpec::Fixed(p) => {
                                    self.port_allocator.reserve_fixed(*p)?
                                }
                            };
                            named_ports.insert(name.clone(), reservation.port);
                            if *name == declared.primary {
                                primary_reservation = Some(reservation);
                            } else {
                                extra_reservations.push(reservation);
                            }
                        }
                    }
                }
                let reservation = primary_reservation.ok_or_else(|| {
                    // `validate` rejects an ambiguous primary before we get here;
                    // this is the belt-and-braces path.
                    OrchestratorError::NodeFailed {
                        node: sel.node.clone(),
                        variant: sel.variant.clone(),
                        reason: "cannot tell which of the declared ports is the primary — \
                                 name one of them \"http\""
                            .to_owned(),
                    }
                })?;
                let port = reservation.port;

                let node_cfg = &self.config.nodes[&sel.node];
                let effective_template = url::resolve_url_template(
                    &self.config.url_template,
                    node_cfg.url_template.as_deref(),
                    variant_cfg.url_template.as_deref(),
                );
                let url_values = url::build_url_template_values(
                    &sel.node,
                    &sel.variant,
                    &run.name,
                    &self.config.name,
                    &branch,
                    &worktree,
                    &username,
                    &hostname,
                );
                let node_url = url::evaluate_url_template(effective_template, &url_values)?;
                let https_url = if self.https_port == 443 {
                    format!("https://{node_url}")
                } else {
                    format!("https://{node_url}:{}", self.https_port)
                };

                let key = RunState::node_key(&sel.node, &sel.variant);
                self.debug_log(&format!(
                    "{}:{} — pre-computed port {} → {}",
                    sel.node, sel.variant, port, https_url
                ))
                .await;

                // Pre-populate all_outputs so every node can resolve
                // ${nodes.X.url}, ${nodes.X.port}, and URL piece references.
                let mut node_out = HashMap::new();
                node_out.insert("port".to_owned(), port.to_string());
                // Named ports are referenceable across nodes too:
                // `${nodes.api.ports.debug}`.
                for (name, value) in &named_ports {
                    node_out.insert(format!("ports.{name}"), value.to_string());
                }
                node_out.insert("url".to_owned(), https_url.clone());
                // Expose individual URL location pieces (mirrors the Web URL API).
                node_out.insert("url.hostname".to_owned(), node_url.clone());
                node_out.insert(
                    "url.host".to_owned(),
                    if self.https_port == 443 {
                        node_url.clone()
                    } else {
                        format!("{}:{}", node_url, self.https_port)
                    },
                );
                node_out.insert("url.origin".to_owned(), https_url.clone());
                node_out.insert("url.scheme".to_owned(), "https".to_owned());
                node_out.insert("url.port".to_owned(), self.https_port.to_string());
                all_outputs.insert(format!("{}:{}", sel.node, sel.variant), node_out.clone());
                all_outputs
                    .entry(sel.node.clone())
                    .or_default()
                    .extend(node_out);

                self.precomputed_servers.insert(
                    key,
                    PrecomputedServer {
                        port,
                        named_ports,
                        hostname: node_url,
                        https_url,
                        reservation: Some(reservation),
                        extra_reservations,
                    },
                );
            }
        }

        // Count total nodes for progress reporting (the terminal node runs
        // separately, so it does not contribute to the startup count). Because
        // `--oneshot` allows exactly one endpoint selection, the terminal node
        // is always the sole node in the final stage, so that stage empties out
        // once it is filtered — drop it from the reported stage count too.
        let total_nodes: usize = plan
            .iter()
            .flatten()
            .filter(|sel| !is_terminal(terminal_key.as_deref(), sel))
            .count();
        let total_stages = if terminal_key.is_some() {
            plan.len().saturating_sub(1)
        } else {
            plan.len()
        };
        self.internal_log(&format!(
            "[start] starting environment '{}' — {} node(s) in {} stage(s)",
            run_name, total_nodes, total_stages
        ))
        .await;
        self.emit(ProgressEvent::PlanResolved {
            total_nodes,
            stages: total_stages,
        });

        // Persist the run *before* the first stage kicks off so it is
        // immediately visible in `veld status` and the management UI. Without
        // this, the earliest write is the per-node checkpoint (start_server
        // nodes only, after the process spawns) or the post-stage save that
        // runs only once stage 1 completes — so a run could not be observed
        // while its first stage was still starting. Seed every planned node as
        // `Pending`; each stage overwrites its own nodes with real state as it
        // executes.
        //
        // Port/URL are deliberately NOT seeded here: `url` is populated only
        // once a node actually spawns, so downstream consumers (`veld urls`,
        // the registry URL list at db/state.rs, the management UI) keep their
        // "url present ⇒ server reachable" invariant and never advertise a
        // not-yet-listening address during startup. `execution_order` is also
        // left untouched — it is appended per stage below (and a pre-seed would
        // duplicate every key); the reverse-order stop path falls back to the
        // node map when it is empty.
        for stage in &plan {
            for sel in stage {
                let key = RunState::node_key(&sel.node, &sel.variant);
                run.nodes
                    .insert(key, NodeState::new(&sel.node, &sel.variant));
            }
        }
        if let Err(e) = self.save_state(&run) {
            // Persisting failed before anything spawned — release the port
            // reservations we are holding and abort so the ports free up.
            self.precomputed_servers.clear();
            return Err(e);
        }
        self.debug_log("Run persisted as 'starting' before first stage executes")
            .await;

        // Resolve project `vars` before anything spawns: a var may be backed by a
        // file or a command, and a broken source must fail the start rather than
        // surface as an empty value inside a service.
        let shared_vars = Arc::new(
            crate::values::resolve_vars(self.config.vars.as_ref(), Some(&self.project_root))
                .await?,
        );
        self.resolved_vars = Some(Arc::clone(&shared_vars));

        // Wrap immutable data in Arc once for all stages.
        let shared_config = Arc::new(self.config.clone());
        let shared_project_root = Arc::new(self.project_root.clone());

        // Execute stages in order. On failure, release any remaining port
        // reservations so the ports become available again immediately.
        let mut node_index: usize = 0;
        let execute_result: Result<(), OrchestratorError> = async {
            for stage in &plan {
                // Drop the terminal node from its stage; only its dependencies
                // run during startup.
                let stage_nodes: Vec<NodeSelection> = stage
                    .iter()
                    .filter(|sel| !is_terminal(terminal_key.as_deref(), sel))
                    .cloned()
                    .collect();
                let results = self
                    .execute_stage(
                        &stage_nodes,
                        &run,
                        &branch,
                        &worktree,
                        &username,
                        &mut all_outputs,
                        &shared_vars,
                        total_nodes,
                        &mut node_index,
                        &shared_config,
                        &shared_project_root,
                    )
                    .await?;

                for (key, node_state) in results {
                    run.execution_order.push(key.clone());
                    run.nodes.insert(key, node_state);
                }

                // Save partial state after each stage so that Ctrl+C or crashes
                // leave enough information for `veld stop` to find and kill PIDs.
                self.save_state(&run)?;

                // A concurrent same-name `veld start` may have replaced this
                // run mid-flight (`cleanup_stale_run`: begin_ending(replaced)
                // + finalize) — the save above was then a silent no-op
                // (terminal runs are immutable) and anything this start
                // spawns from here on would be tracked by no run row and
                // covered by no reaper. Detect it, kill what we know about,
                // and abort instead of leaking.
                match self.db.run_status_by_id(&run.run_id) {
                    Ok(Some(s)) if !s.is_live() => {
                        let pids: Vec<u32> = run.nodes.values().filter_map(|ns| ns.pid).collect();
                        if !kill_and_confirm(&pids).await {
                            // Nothing can persist these PIDs (the run is
                            // terminal and immutable), so no reaper covers
                            // them — a warning is the only remaining signal.
                            tracing::warn!(
                                run_name,
                                ?pids,
                                "superseded start could not confirm killing its own \
                                 spawned processes — they may leak"
                            );
                        }
                        return Err(OrchestratorError::Superseded(run_name.to_owned()));
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        .await;

        if let Err(ref e) = execute_result {
            self.internal_log(&format!("[start] startup failed: {e}"))
                .await;
        }
        if let Err(e) = execute_result {
            // Release all remaining port reservations so the ports become
            // available to the system immediately.
            self.precomputed_servers.clear();

            // Do NOT save our in-memory `run` here. On failure it still holds the
            // seeded `Pending` placeholders for the failed stage (stage results
            // are only merged into `run` on success), while the persisted state
            // holds each spawned node's real PID from its per-node checkpoint
            // (see `execute_start_server_isolated`). Saving the in-memory copy
            // would clobber those PIDs with `None` and a later `veld stop` could
            // no longer kill the leaked process.
            //
            // Ending protocol, in order: persist the `failed` intent FIRST
            // (before any PID dies — otherwise the GC orphan sweep, which
            // includes `starting` runs, can race the kill window and record
            // this deliberate failure as `crashed`, clobbering the failure
            // detail this feature exists to preserve), then kill, then
            // finalize only over confirmed-dead processes. An unconfirmed
            // kill leaves the run `stopping` with its recorded PIDs — the
            // stale-`stopping` reaper re-kills and finalizes it later, so
            // leak-freedom never depends on the label.
            let detail = end_detail_for_error(&e);
            if let Ok(Some(persisted)) = self.db.get_run(&self.project_root, run_name) {
                if persisted.run_id == run.run_id {
                    let _ = self
                        .db
                        .begin_ending(&run.run_id, EndReason::Failed, Some(&detail));
                    let pids: Vec<u32> = persisted.nodes.values().filter_map(|ns| ns.pid).collect();
                    let confirmed = pids.is_empty() || kill_and_confirm(&pids).await;
                    // Routes for anything that spawned far enough to get one.
                    for (key, ns) in &persisted.nodes {
                        self.remove_node_routes(run_name, ns).await;
                        if confirmed && ns.pid.is_some() {
                            // Confirmed dead — a recorded PID under an ended
                            // run means "possibly alive" to the GC straggler
                            // sweep.
                            let _ = self.db.clear_node_pid(&run.run_id, key);
                        }
                    }
                    if confirmed {
                        let _ = self.db.finalize_run(&run.run_id);
                    } else {
                        tracing::warn!(
                            run_name,
                            "startup failed but a spawned process did not die — \
                             leaving the run 'stopping' for the stale-stopping reaper"
                        );
                    }
                }
            }
            return Err(e);
        }

        // All reservations have been consumed — clear the map.
        self.precomputed_servers.clear();

        // Capture the dependency outputs for a pending terminal-node run.
        if terminal_key.is_some() {
            self.terminal_outputs = Some(all_outputs.clone());
        }

        run.status = RunStatus::Running;

        // Final state save with Running status.
        self.save_state(&run)?;

        self.internal_log(&format!(
            "[start] environment '{}' is running — all {} node(s) healthy",
            run_name, total_nodes
        ))
        .await;

        Ok(run)
    }

    /// Run the terminal one-off node (`--oneshot`) after its dependencies are
    /// healthy. Streams the node's output live (and into the run log), captures
    /// its exit code, and persists its final state.
    ///
    /// A non-zero exit is the node's *result* (e.g. failing tests), not a
    /// startup error, so — unlike a `command` node inside a startup stage — it
    /// is captured and returned rather than raised as `NodeFailed`. The caller
    /// is expected to tear the run down afterwards and propagate the code.
    ///
    /// A command node's `readiness_probe` (which `execute_command_isolated`
    /// runs after a zero exit) is intentionally NOT run here — a post-run probe
    /// on the run's final node is meaningless.
    ///
    /// **Must be called after [`Orchestrator::start`] on the same instance**:
    /// `start` stashes the dependency outputs this method interpolates into the
    /// command. Calling it standalone leaves `${nodes.X.*}` references
    /// unresolved.
    pub async fn run_terminal(
        &mut self,
        run_name: &str,
        sel: &NodeSelection,
    ) -> Result<i32, OrchestratorError> {
        let node_cfg = self.config.nodes.get(&sel.node).cloned().ok_or_else(|| {
            OrchestratorError::NodeFailed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                reason: "terminal node not found in config".to_owned(),
            }
        })?;
        let variant_cfg = node_cfg
            .variants
            .get(&sel.variant)
            .cloned()
            .ok_or_else(|| OrchestratorError::NodeFailed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                reason: "terminal variant not found in config".to_owned(),
            })?;

        // A terminal node must run to completion — a start_server never exits,
        // so it can never be the thing whose exit ends the run.
        let resolved = config::resolve_variant(&self.config, &node_cfg, &variant_cfg);
        if resolved.step_type != config::StepType::Command {
            return Err(OrchestratorError::NodeFailed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                reason: "--oneshot requires a command-type node (start_server never exits)"
                    .to_owned(),
            });
        }

        // Load the run so we can persist the terminal node's result back into
        // its state and execution order (for reverse-order teardown).
        let mut run = match self.db.get_run(&self.project_root, run_name)? {
            Some(r) => r,
            None => {
                return Err(OrchestratorError::NodeFailed {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                    reason: format!("run '{run_name}' not found"),
                });
            }
        };

        // Build the variable context: same builtins as a stage node, plus the
        // dependency outputs captured by `start`.
        let branch = url::detect_git_branch(&self.project_root);
        let worktree = self
            .project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default")
            .to_owned();
        let username = whoami_username();

        let mut var_ctx = VariableContext::new();
        BuiltinScope {
            run_name,
            run_id: Some(run.run_id.to_string()),
            project_root: &self.project_root,
            project_name: &self.config.name,
            worktree: &worktree,
            branch: &branch,
            username: &username,
            node: Some((&sel.node, &sel.variant)),
        }
        .apply(&mut var_ctx);

        for (name, value) in self.resolved_vars.iter().flat_map(|v| v.iter()) {
            var_ctx.set_var(name, value.clone());
        }

        // Clone (not take) the stashed dependency outputs so a defensive
        // re-invocation still resolves `${nodes.X.*}` rather than silently
        // getting an empty map.
        let outputs_map = self.terminal_outputs.clone().unwrap_or_default();
        for (node_key, outputs) in &outputs_map {
            for (field, value) in outputs {
                var_ctx.set_node_output(&format!("nodes.{node_key}.{field}"), value.clone());
            }
        }

        // Resolve working directory, command/script, and environment.
        let working_dir = resolve_working_dir(
            variant_cfg.cwd.as_deref(),
            node_cfg.cwd.as_deref(),
            &self.project_root,
            &var_ctx,
        )?;
        let raw_cmd = match &resolved.script {
            Some(script) => config::CommandSpec::script(&self.project_root.join(script)),
            None => resolved
                .command
                .clone()
                .unwrap_or_else(|| config::CommandSpec::Shell(String::new())),
        };
        let resolved_cmd = raw_cmd.interpolate(&var_ctx)?;
        let (env, env_secret_keys) = build_env(
            resolved.env.as_ref(),
            &var_ctx,
            &format!("nodes.{}.variants.{}", sel.node, sel.variant),
            &self.project_root,
        )
        .await?;

        let key = RunState::node_key(&sel.node, &sel.variant);

        // Idempotency: if skip_if passes, skip the run entirely (exit 0).
        if let Some(ref skip_if_cmd) = resolved.skip_if {
            let skip_if_resolved = skip_if_cmd.interpolate(&var_ctx)?;
            if let Ok(out) = process::run_command(&skip_if_resolved, &working_dir, &env, None).await
            {
                if out.exit_code == 0 {
                    tracing::info!(
                        node = sel.node,
                        variant = sel.variant,
                        "skip_if passed — skipping terminal node"
                    );
                    if let Some(ns) = run.nodes.get_mut(&key) {
                        ns.status = NodeStatus::Skipped;
                        ns.outputs.insert("exit_code".to_owned(), "0".to_owned());
                    }
                    if !run.execution_order.contains(&key) {
                        run.execution_order.push(key.clone());
                    }
                    // Best-effort persist — a bookkeeping failure must not turn
                    // a skipped (exit 0) result into an error.
                    if let Err(e) = self.save_state(&run) {
                        tracing::warn!(error = %e, "failed to persist skipped terminal node");
                    }
                    // A skipped oneshot is a passing one for history purposes.
                    let detail = EndDetail {
                        exit_code: Some(0),
                        message: Some("terminal node skipped (skip_if passed)".to_owned()),
                        ..Default::default()
                    };
                    let _ = self
                        .db
                        .begin_ending(&run.run_id, EndReason::Completed, Some(&detail));
                    return Ok(0);
                }
            }
        }

        // Run the command, streaming its output live and into the run log.
        let output_file =
            logging::output_file(&self.project_root, run_name, &sel.node, &sel.variant);
        let log_target = process::LogTarget {
            db: self.db.clone(),
            project_root: self.project_root.clone(),
            run_name: run_name.to_owned(),
            run_id: run.run_id.to_string(),
            node: sel.node.clone(),
            variant: sel.variant.clone(),
        };
        let result = process::run_command_streaming(
            &resolved_cmd,
            &working_dir,
            &env,
            Some(&output_file),
            Some(log_target),
        )
        .await?;

        // Persist the terminal node's final state. Undeclared outputs are
        // ignored (not fatal): the node has already produced its result and its
        // exit code is what matters — failing the run over strict_outputs here
        // would only mask it.
        let declared_keys = resolved
            .outputs
            .as_ref()
            .map(|o| o.declared_keys())
            .unwrap_or_default();
        let mut node_state = run
            .nodes
            .get(&key)
            .cloned()
            .unwrap_or_else(|| NodeState::new(&sel.node, &sel.variant));
        node_state
            .outputs
            .insert("exit_code".to_owned(), result.exit_code.to_string());
        for (k, v) in &result.outputs {
            if declared_keys.contains(k.as_str()) {
                node_state.outputs.insert(k.clone(), v.clone());
            }
        }
        node_state.status = if result.exit_code == 0 {
            NodeStatus::Healthy
        } else {
            NodeStatus::Failed
        };
        if let Some(sensitive) = resolved.sensitive_outputs.clone() {
            node_state.sensitive_keys = sensitive;
        }
        node_state.sensitive_keys.extend(env_secret_keys);
        run.nodes.insert(key.clone(), node_state);
        // Append last so reverse-order teardown runs its on_stop hook first.
        if !run.execution_order.contains(&key) {
            run.execution_order.push(key.clone());
        }
        // Best-effort persist: the command has already run and its exit code is
        // the whole `--oneshot` contract, so a post-completion bookkeeping
        // failure must not override it (a passing run reporting 127 to CI would
        // be a false failure). Log and return the real code regardless.
        if let Err(e) = self.save_state(&run) {
            tracing::warn!(error = %e, "failed to persist terminal node result");
        }

        // Store the run's outcome intent now: zero exit → completed, non-zero
        // → failed with the code. The caller's teardown (`veld stop`) finds
        // the run already `stopping`, loses `begin_ending`, and finalizes
        // with THIS reason — so history says "completed"/"failed (exit N)",
        // not "stopped", for oneshot runs. An agent reading `end_reason =
        // completed` must be able to trust that the command passed.
        let reason = if result.exit_code == 0 {
            EndReason::Completed
        } else {
            EndReason::Failed
        };
        let detail = EndDetail {
            failed_node: (result.exit_code != 0).then(|| key.clone()),
            exit_code: Some(result.exit_code),
            ..Default::default()
        };
        if let Err(e) = self.db.begin_ending(&run.run_id, reason, Some(&detail)) {
            tracing::warn!(error = %e, "failed to record oneshot outcome");
        }

        Ok(result.exit_code)
    }

    /// Execute a single stage: all nodes run in parallel via `JoinSet`.
    async fn execute_stage(
        &mut self,
        stage: &[NodeSelection],
        run: &RunState,
        branch: &str,
        worktree: &str,
        username: &str,
        all_outputs: &mut HashMap<String, HashMap<String, String>>,
        vars: &Arc<HashMap<String, String>>,
        total_nodes: usize,
        node_index: &mut usize,
        shared_config: &Arc<VeldConfig>,
        shared_project_root: &Arc<PathBuf>,
    ) -> Result<Vec<(String, NodeState)>, OrchestratorError> {
        // Build shared context (cloned once per stage).
        let ctx = NodeExecutionContext {
            config: Arc::clone(shared_config),
            db: self.db.clone(),
            project_root: Arc::clone(shared_project_root),
            https_port: self.https_port,
            foreground: self.foreground,
            helper_client: self.helper_client.clone(),
            progress_tx: self.progress_tx.clone(),
            debug_writer: self.debug_writer.clone(),
            run_name: run.name.clone(),
            run_id: run.run_id,
            branch: branch.to_owned(),
            worktree: worktree.to_owned(),
            username: username.to_owned(),
            all_outputs: Arc::new(all_outputs.clone()),
            vars: Arc::clone(vars),
            checkpoint: Arc::new(std::sync::Mutex::new(CheckpointState {
                run: run.clone(),
                project_root: self.project_root.clone(),
            })),
        };

        // Assign indices and extract precomputed servers before spawning.
        let mut assignments: Vec<(NodeSelection, usize, Option<PrecomputedServer>)> = Vec::new();
        for sel in stage {
            *node_index += 1;
            let key = RunState::node_key(&sel.node, &sel.variant);
            let server = self.precomputed_servers.remove(&key);
            assignments.push((sel.clone(), *node_index, server));
        }

        // Spawn all nodes into a JoinSet.
        let mut join_set = tokio::task::JoinSet::new();
        for (sel, index, precomputed) in assignments {
            let task_ctx = ctx.clone();
            join_set.spawn(execute_node_isolated(
                task_ctx,
                sel,
                precomputed,
                index,
                total_nodes,
            ));
        }

        // Collect results; fail-fast on first error.
        let mut results: Vec<NodeExecutionResult> = Vec::new();
        while let Some(join_result) = join_set.join_next().await {
            let task_result = join_result.map_err(|e| OrchestratorError::NodeFailed {
                node: "unknown".into(),
                variant: "unknown".into(),
                reason: format!("task panicked: {e}"),
            })?;

            match task_result {
                Ok(node_result) => {
                    results.push(node_result);
                }
                Err(e) => {
                    // Cancel all remaining tasks.
                    join_set.abort_all();
                    // Drain: collect any already-completed Ok results so we
                    // can register their server handles for cleanup.
                    while let Some(drain_result) = join_set.join_next().await {
                        if let Ok(Ok(node_result)) = drain_result {
                            results.push(node_result);
                        }
                    }
                    // Merge handles from successful tasks into self.children
                    // so the caller's stop() can find and kill them.
                    for result in &mut results {
                        if let Some(handle) = result.server_handle.take() {
                            self.children.insert(result.key.clone(), handle);
                        }
                    }
                    return Err(e);
                }
            }
        }

        // Sort by pre-assigned index for deterministic execution_order.
        results.sort_by_key(|r| r.index);

        // Merge server handles back into self.children.
        for result in &mut results {
            if let Some(handle) = result.server_handle.take() {
                self.children.insert(result.key.clone(), handle);
            }
        }

        // Merge outputs back into all_outputs for downstream stages.
        let mut stage_results: Vec<(String, NodeState)> = Vec::new();
        for result in results {
            let mut node_out = result.node_state.outputs.clone();
            if let Some(port) = result.node_state.port {
                node_out.insert("port".to_owned(), port.to_string());
            }
            if let Some(ref u) = result.node_state.url {
                node_out.insert("url".to_owned(), u.clone());
            }
            all_outputs.insert(
                format!("{}:{}", result.sel.node, result.sel.variant),
                node_out.clone(),
            );
            all_outputs
                .entry(result.sel.node.clone())
                .or_default()
                .extend(node_out);

            stage_results.push((result.key, result.node_state));
        }

        Ok(stage_results)
    }

    // -----------------------------------------------------------------------
    // Stop
    // -----------------------------------------------------------------------

    /// Stop a run in reverse dependency order. Returns whether the run was
    /// actually stopped or was already stopped.
    pub async fn stop(&mut self, run_name: &str) -> Result<StopResult, OrchestratorError> {
        // Create internal log writer for this run (may already exist from start).
        if self.internal_log.is_none() {
            self.internal_log = Some(LogWriter::for_run(
                self.db.clone(),
                &self.project_root,
                run_name,
                LogStream::Internal,
            ));
        }
        self.internal_log(&format!("[stop] stopping environment '{run_name}'"))
            .await;

        // Reconnect to whichever helper is running (system or user socket)
        if let Ok(client) = crate::helper::HelperClient::connect().await {
            self.helper_client = client;
        }

        let mut run = match self.db.get_run(&self.project_root, run_name) {
            Ok(Some(r)) => r,
            _ => {
                // Environment unknown (e.g., setup failed before state was saved).
                // Still run teardown steps to clean up anything setup may have created.
                self.run_teardown_steps(run_name).await;
                return Ok(StopResult::AlreadyStopped);
            }
        };

        if let Some(w) = self.internal_log.as_mut() {
            w.set_run_id(run.run_id);
        }

        if !run.is_live() {
            // Latest run already ended — it is history now, never deleted here.
            // Teardown steps still run so a re-stop stays a cleanup tool.
            self.run_teardown_steps(run_name).await;
            return Ok(StopResult::AlreadyStopped);
        }

        // Phase 1 of the ending protocol: persist the intent BEFORE any PID
        // dies, so the crash detectors (which scan only starting/running)
        // cannot mislabel this deliberate stop as a crash. Losing the race
        // (already `stopping` — a SIGKILLed earlier stop, or an ending oneshot
        // that stored completed/failed) is fine: proceed with teardown and
        // finalize whatever intent is stored.
        let _ = self
            .db
            .begin_ending(&run.run_id, EndReason::Stopped, None)?;
        run.status = RunStatus::Stopping;

        // Captured before the loop borrows `run` mutably; `${veld.run_id}` must
        // resolve in `on_stop` exactly as it did in the node's own command.
        let run_id = run.run_id;

        // `on_stop` may reference `${vars.*}`. Reuse `start`'s values when this is
        // the same process; otherwise (`veld stop` as its own invocation) resolve
        // once for the whole teardown. A failing source must not abort the stop —
        // teardown running is more important than every hook interpolating — so
        // this degrades to no vars and the affected hooks report being skipped.
        let stop_vars: Arc<HashMap<String, String>> = match &self.resolved_vars {
            Some(v) => Arc::clone(v),
            None => match crate::values::resolve_vars(
                self.config.vars.as_ref(),
                Some(&self.project_root),
            )
            .await
            {
                Ok(v) => Arc::new(v),
                Err(e) => {
                    tracing::warn!(error = %e, "could not resolve vars for teardown hooks");
                    Arc::new(HashMap::new())
                }
            },
        };

        // Stop in reverse execution order (dependencies last). Fall back to
        // HashMap keys for runs created before execution_order was tracked.
        let node_keys: Vec<String> = if run.execution_order.is_empty() {
            run.nodes.keys().cloned().collect()
        } else {
            run.execution_order.clone()
        };

        for key in node_keys.iter().rev() {
            if let Some(node_state) = run.nodes.get_mut(key) {
                self.internal_log(&format!(
                    "[stop] stopping {}:{} (pid: {:?})",
                    node_state.node_name, node_state.variant, node_state.pid
                ))
                .await;

                // Kill process if running.
                if let Some(pid) = node_state.pid {
                    if process::is_alive(pid) {
                        if let Err(e) = process::kill_process(pid).await {
                            tracing::warn!(pid, error = %e, "failed to kill process");
                        }
                    }
                }

                // Remove DNS + Caddy route.
                if let Some(ref url_str) = node_state.url {
                    let hostname = url_str.strip_prefix("https://").unwrap_or(url_str);
                    // Strip port if present (e.g., "host:18443" → "host")
                    let hostname = hostname.split(':').next().unwrap_or(hostname);
                    let _ = self.helper_client.remove_host(hostname).await;
                    let route_id = format!(
                        "veld-{}-{}-{}",
                        run_name, node_state.node_name, node_state.variant
                    );
                    let _ = self.helper_client.remove_route(&route_id).await;
                }

                // Run on_stop hook if defined (skip nodes that never ran).
                if node_state.status != NodeStatus::Pending {
                    self.run_on_stop_hook(run_name, Some(run_id), &stop_vars, node_state)
                        .await;
                }

                node_state.status = NodeStatus::Stopped;
                node_state.pid = None;
            }

            // Remove child handle.
            self.children.remove(key);
        }

        // Run project-level teardown steps after all per-node on_stop hooks.
        self.run_teardown_steps(run_name).await;

        // Persist the final node states while the run is still `stopping`
        // (save_run refuses to touch terminal runs), then finalize it into
        // history with whatever end_reason `begin_ending` stored.
        self.save_state(&run)?;
        let _ = self.db.finalize_run(&run.run_id)?;

        self.internal_log(&format!("[stop] environment '{run_name}' stopped"))
            .await;

        Ok(StopResult::Stopped)
    }

    /// Clean up a stale run with the given name if it exists in state.
    /// Kills any live processes, removes DNS/Caddy routes, and clears state.
    /// Errors are logged but never propagated — this is best-effort cleanup.
    async fn cleanup_stale_run(&mut self, run_name: &str) {
        // Always clear stale feedback data so a reused run name starts fresh,
        // even if the run was already removed from state.
        let feedback =
            crate::feedback::FeedbackStore::new(self.db.clone(), &self.project_root, run_name);
        if feedback.has_data() {
            tracing::info!(run_name, "clearing stale feedback data");
            let _ = feedback.clear();
        }

        let run = match self.db.get_run(&self.project_root, run_name) {
            Ok(Some(r)) => r,
            _ => return,
        };
        if !run.is_live() {
            // Latest run already ended — history, nothing to clean up.
            return;
        }

        tracing::info!(run_name, "replacing live run before starting");

        // Ending protocol, replaced path: persist the intent BEFORE killing —
        // this moves the run out of the crash detectors' scan set, so the 5s
        // monitor can't label the deliberate replacement `crashed`. Losing the
        // race (already `stopping`) is fine; teardown continues either way.
        let _ = self.db.begin_ending(&run.run_id, EndReason::Replaced, None);

        // Kill and wait (bounded) for the old run's processes.
        let pids: Vec<u32> = run.nodes.values().filter_map(|ns| ns.pid).collect();
        let confirmed = pids.is_empty() || kill_and_confirm(&pids).await;

        for (key, ns) in &run.nodes {
            self.remove_node_routes(run_name, ns).await;
            if confirmed && ns.pid.is_some() {
                let _ = self.db.clear_node_pid(&run.run_id, key);
            }
        }

        // Finalize even on an unconfirmed kill — an unkillable old PID must
        // not block the new start (today's behavior ignores kill failures
        // entirely). The GC's terminal-run straggler sweep re-kills any PID
        // still alive under a terminal run, so leak-freedom never depends on
        // this label; the detail records what happened for the history view.
        if !confirmed {
            let detail = EndDetail {
                message: Some("kill unconfirmed at replacement".to_owned()),
                ..Default::default()
            };
            let mut ended = run.clone();
            ended.status = RunStatus::Stopping;
            ended.end_detail = Some(detail);
            let _ = self.save_state(&ended);
        }
        let _ = self.db.finalize_run(&run.run_id);
    }

    /// Clean up ALL runs in the project whose processes have died.
    /// This catches orphaned runs from previous sessions that were not
    /// properly stopped (e.g., due to a crash or `kill -9`).
    async fn cleanup_dead_runs(&mut self) {
        let project_state = match self.db.load_project_state(&self.project_root) {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut dead_run_names = Vec::new();

        for (run_name, run_state) in &project_state.runs {
            let any_alive = run_state
                .nodes
                .values()
                .any(|ns| ns.pid.is_some_and(process::is_alive));
            // A node records a PID only once its process actually spawns.
            let ever_spawned = run_state.nodes.values().any(|ns| ns.pid.is_some());

            if is_reapable_orphan(&run_state.status, any_alive, ever_spawned) {
                dead_run_names.push(run_name.clone());
            }
        }

        for run_name in &dead_run_names {
            tracing::info!(
                run_name,
                "finalizing dead run as crashed (all processes exited)"
            );

            let Some(run_state) = project_state.runs.get(run_name) else {
                continue;
            };

            // Kill any stragglers and clean up routes.
            let mut dead_node: Option<String> = None;
            for (key, ns) in &run_state.nodes {
                if ns.pid.is_some() && dead_node.is_none() {
                    dead_node = Some(key.clone());
                }
                if let Some(pid) = ns.pid {
                    if process::is_alive(pid) {
                        let _ = process::kill_process(pid).await;
                    }
                }
                self.remove_node_routes(run_name, ns).await;
            }

            // Record the final node states while the run is still live in the
            // DB, then finalize as crashed (one-step: PIDs are already dead;
            // the guard no-ops if an ender got here first).
            let mut ended = run_state.clone();
            for node in ended.nodes.values_mut() {
                if node.pid.take().is_some() {
                    node.status = NodeStatus::Stopped;
                }
            }
            let _ = self.save_state(&ended);
            let detail = EndDetail {
                failed_node: dead_node,
                ..Default::default()
            };
            let _ = self.db.finalize_crashed(&run_state.run_id, Some(&detail));
        }
    }

    /// Remove the DNS host and Caddy route for a node (best-effort).
    async fn remove_node_routes(&self, run_name: &str, node_state: &NodeState) {
        if let Some(ref url_str) = node_state.url {
            let hostname = url_str.strip_prefix("https://").unwrap_or(url_str);
            // Strip port if present (e.g., "host:18443" → "host")
            let hostname = hostname.split(':').next().unwrap_or(hostname);
            let _ = self.helper_client.remove_host(hostname).await;
            let route_id = format!(
                "veld-{}-{}-{}",
                run_name, node_state.node_name, node_state.variant
            );
            let _ = self.helper_client.remove_route(&route_id).await;
        }
    }

    /// Run the `on_stop` hook for a node if one is defined in the config.
    async fn run_on_stop_hook(
        &self,
        run_name: &str,
        run_id: Option<uuid::Uuid>,
        vars: &HashMap<String, String>,
        node_state: &NodeState,
    ) {
        let variant_cfg = match self
            .config
            .nodes
            .get(&node_state.node_name)
            .and_then(|n| n.variants.get(&node_state.variant))
        {
            Some(cfg) => cfg,
            None => return,
        };

        // Resolved, not raw: `on_stop` is hoistable to node level (F3), and reading
        // the variant directly meant a node-level teardown hook never ran — the
        // exact container-leak failure this feature exists to prevent.
        let resolved = match self
            .config
            .resolved(&node_state.node_name, &node_state.variant)
        {
            Some(r) => r,
            None => return,
        };
        let on_stop_cmd = match resolved.on_stop.as_ref() {
            Some(cmd) => cmd,
            None => return,
        };

        tracing::info!(
            node = node_state.node_name,
            variant = node_state.variant,
            "running on_stop hook"
        );

        // Build variable context matching what was available at start time.
        let mut ctx = VariableContext::new();
        BuiltinScope {
            run_name,
            run_id: run_id.map(|id| id.to_string()),
            project_root: &self.project_root,
            project_name: &self.config.name,
            worktree: self
                .project_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default"),
            branch: &url::detect_git_branch(&self.project_root),
            username: &whoami_username(),
            node: Some((&node_state.node_name, &node_state.variant)),
        }
        .apply(&mut ctx);

        for (name, value) in vars {
            ctx.set_var(name, value.clone());
        }

        // Outputs are reachable as `${output.KEY}` (this node) and
        // `${nodes.<node>.KEY}` (any node) — deliberately NOT as `${veld.KEY}`.
        // Injecting them into the builtins let an output named `port`, `url`,
        // `run`, `node`, or `branch` shadow the builtin here but nowhere else,
        // so the same string resolved differently in `command` and `on_stop`.
        for (k, v) in &node_state.outputs {
            ctx.set_output(k, v.clone());
            ctx.set_node_output(&format!("nodes.{}.{k}", node_state.node_name), v.clone());
        }
        if let Some(port) = node_state.port {
            ctx.set_builtin("port", port.to_string());
        }

        let resolved_cmd = match on_stop_cmd.interpolate(&ctx) {
            Ok(cmd) => cmd,
            Err(e) => {
                // A teardown hook that cannot be resolved does not run, and the
                // user is about to be told the environment stopped cleanly. That
                // combination is how containers leak, so say it loudly on the
                // stream the user actually reads rather than burying it in a
                // tracing warning. `veld start`/`veld lint` catch the common cause
                // (an unknown `${veld.*}` name — see `check_builtin_names`), but
                // this path stays reachable for a config edited after start.
                eprintln!(
                    "  ! teardown hook for {}:{} was SKIPPED: {e}\n    \
                     The command was: {}\n    \
                     Anything it was meant to clean up (containers, volumes, \
                     temp state) has been left behind.",
                    node_state.node_name,
                    node_state.variant,
                    on_stop_cmd.display(),
                );
                tracing::warn!(
                    node = node_state.node_name,
                    error = %e,
                    "failed to resolve on_stop command variables — hook skipped"
                );
                return;
            }
        };

        // Build env (variant > node > project).
        let node_cfg_opt = self.config.nodes.get(&node_state.node_name);
        let merged_env = resolved.env.clone();
        let env = match build_env(
            merged_env.as_ref(),
            &ctx,
            &format!(
                "nodes.{}.variants.{}",
                node_state.node_name, node_state.variant
            ),
            &self.project_root,
        )
        .await
        {
            Ok((env, _)) => env,
            Err(e) => {
                tracing::warn!(
                    node = node_state.node_name,
                    error = %e,
                    "failed to resolve on_stop env variables, using empty env"
                );
                HashMap::new()
            }
        };

        // Resolve working directory (variant > node > project root).
        let working_dir = resolve_working_dir(
            variant_cfg.cwd.as_deref(),
            node_cfg_opt.and_then(|n| n.cwd.as_deref()),
            &self.project_root,
            &ctx,
        )
        .unwrap_or_else(|e| {
            tracing::warn!(
                node = node_state.node_name,
                error = %e,
                "failed to resolve on_stop cwd, falling back to project root"
            );
            self.project_root.clone()
        });

        match process::run_command(&resolved_cmd, &working_dir, &env, None).await {
            Ok(result) => {
                if result.exit_code != 0 {
                    tracing::warn!(
                        node = node_state.node_name,
                        exit_code = result.exit_code,
                        "on_stop hook exited with non-zero code"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    node = node_state.node_name,
                    error = %e,
                    "on_stop hook failed to execute"
                );
            }
        }
    }

    /// Build the interpolation context for a project-level lifecycle step
    /// (`setup` / `teardown`). Same closed `veld.*` set as a node context minus
    /// `node`/`variant`, so `${veld.branch}` does not silently work in a node
    /// command and fail in a setup step.
    fn project_step_context(&self, run_name: &str) -> VariableContext {
        let mut ctx = VariableContext::new();
        for (name, value) in self.resolved_vars.iter().flat_map(|v| v.iter()) {
            ctx.set_var(name, value.clone());
        }
        BuiltinScope {
            run_name,
            run_id: None,
            project_root: &self.project_root,
            project_name: &self.config.name,
            worktree: self
                .project_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default"),
            branch: &url::detect_git_branch(&self.project_root),
            username: &whoami_username(),
            node: None,
        }
        .apply(&mut ctx);
        ctx
    }

    // -----------------------------------------------------------------------
    // Setup / Teardown lifecycle steps
    // -----------------------------------------------------------------------

    /// Run project-level setup steps sequentially. Returns an error if any
    /// step exits non-zero, aborting startup.
    async fn run_setup_steps(&self, run_name: &str) -> Result<(), OrchestratorError> {
        let steps = match self.config.setup.as_ref() {
            Some(steps) if !steps.is_empty() => steps,
            _ => return Ok(()),
        };

        let total = steps.len();
        let ctx = self.project_step_context(run_name);

        for (i, step) in steps.iter().enumerate() {
            self.emit(ProgressEvent::SetupStepStarting {
                name: step.name.clone(),
                index: i + 1,
                total,
            });

            let started = std::time::Instant::now();
            let Some(step_cmd) = step.cmd.spec() else {
                let reason = "step declares no command — set \"argv\" or \"shell\"".to_owned();
                self.emit(ProgressEvent::SetupStepFailed {
                    name: step.name.clone(),
                    error: reason.clone(),
                });
                return Err(OrchestratorError::SetupFailed {
                    name: step.name.clone(),
                    reason,
                    failure_message: step.failure_message.clone(),
                });
            };
            let resolved_cmd = match step_cmd.interpolate(&ctx) {
                Ok(cmd) => cmd,
                Err(e) => {
                    let reason = format!("variable resolution failed: {e}");
                    self.emit(ProgressEvent::SetupStepFailed {
                        name: step.name.clone(),
                        error: reason.clone(),
                    });
                    return Err(OrchestratorError::SetupFailed {
                        name: step.name.clone(),
                        reason,
                        failure_message: step.failure_message.clone(),
                    });
                }
            };

            let env = HashMap::new();
            match process::run_command(&resolved_cmd, &self.project_root, &env, None).await {
                Ok(result) => {
                    if result.exit_code != 0 {
                        let reason = format!("exited with code {}", result.exit_code);
                        self.emit(ProgressEvent::SetupStepFailed {
                            name: step.name.clone(),
                            error: reason.clone(),
                        });
                        return Err(OrchestratorError::SetupFailed {
                            name: step.name.clone(),
                            reason,
                            failure_message: step.failure_message.clone(),
                        });
                    }
                    let elapsed = started.elapsed().as_millis() as u64;
                    self.emit(ProgressEvent::SetupStepCompleted {
                        name: step.name.clone(),
                        elapsed_ms: elapsed,
                    });
                }
                Err(e) => {
                    let reason = format!("execution failed: {e}");
                    self.emit(ProgressEvent::SetupStepFailed {
                        name: step.name.clone(),
                        error: reason.clone(),
                    });
                    return Err(OrchestratorError::SetupFailed {
                        name: step.name.clone(),
                        reason,
                        failure_message: step.failure_message.clone(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Run project-level teardown steps sequentially. Best-effort: failures
    /// are logged but never propagated.
    async fn run_teardown_steps(&self, run_name: &str) {
        let steps = match self.config.teardown.as_ref() {
            Some(steps) if !steps.is_empty() => steps,
            _ => return,
        };

        let total = steps.len();
        let ctx = self.project_step_context(run_name);

        for (i, step) in steps.iter().enumerate() {
            self.emit(ProgressEvent::TeardownStepRunning {
                name: step.name.clone(),
                index: i + 1,
                total,
            });

            let Some(step_cmd) = step.cmd.spec() else {
                tracing::warn!(
                    step = step.name,
                    "teardown step declares no command — set \"argv\" or \"shell\""
                );
                continue;
            };
            let resolved_cmd = match step_cmd.interpolate(&ctx) {
                Ok(cmd) => cmd,
                Err(e) => {
                    tracing::warn!(
                        step = step.name,
                        error = %e,
                        "teardown step variable resolution failed"
                    );
                    continue;
                }
            };

            let env = HashMap::new();
            match process::run_command(&resolved_cmd, &self.project_root, &env, None).await {
                Ok(result) => {
                    if result.exit_code != 0 {
                        tracing::warn!(
                            step = step.name,
                            exit_code = result.exit_code,
                            "teardown step exited with non-zero code"
                        );
                    } else {
                        self.emit(ProgressEvent::TeardownStepCompleted {
                            name: step.name.clone(),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        step = step.name,
                        error = %e,
                        "teardown step failed to execute"
                    );
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // State persistence
    // -----------------------------------------------------------------------

    fn save_state(&self, run: &RunState) -> Result<(), OrchestratorError> {
        self.db
            .save_run(&self.project_root, &self.config.name, run)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Isolated node execution (free functions for parallel spawning)
// ---------------------------------------------------------------------------

/// Emit a progress event (no-op if no sender is set).
fn emit_progress(tx: &Option<mpsc::UnboundedSender<ProgressEvent>>, event: ProgressEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Write a line to the debug log (no-op when writer is None).
async fn debug_log_free(writer: &Option<LogWriter>, message: &str) {
    if let Some(writer) = writer {
        let _ = writer.write_line(&format!("[VELD] {message}")).await;
    }
}

/// Does this synthetic-output template read anything sensitive?
///
/// Reuses `config`'s reference parsers rather than re-scanning for `${...}` here:
/// two implementations of "what does this string reference" drift, and the one that
/// drifts silently is the one deciding whether a credential gets masked.
///
/// Deliberately conservative — an unrecognised reference form is treated as *not*
/// tainted, so this narrows an existing leak rather than pretending to close every
/// path. `sensitive` is matched against both the bare key (how a `shell` template
/// reads an env value, `$KEY`) and the `${output.KEY}` / `${nodes.N.KEY}` forms.
fn template_is_tainted(tmpl: &str, sensitive: &[String], secret_vars: &[String]) -> bool {
    let is_sensitive = |name: &str| sensitive.iter().any(|s| s == name);

    if crate::config::env_refs(tmpl)
        .iter()
        .any(|n| is_sensitive(n))
    {
        return true;
    }
    if crate::config::builtin_refs_in(tmpl, "output.")
        .iter()
        .any(|n| is_sensitive(n))
    {
        return true;
    }
    // `${nodes.<node>.KEY}` — the trailing field is the output key.
    if crate::config::builtin_refs_in(tmpl, "nodes.")
        .iter()
        .filter_map(|r| r.rsplit('.').next().map(str::to_owned))
        .any(|n| is_sensitive(&n))
    {
        return true;
    }
    crate::config::builtin_refs_in(tmpl, "vars.")
        .iter()
        .any(|n| secret_vars.iter().any(|v| v == n))
}

/// Build a readiness probe attempt notifier that sends progress events.
fn make_attempt_notifier(
    tx: &Option<mpsc::UnboundedSender<ProgressEvent>>,
    node: &str,
    variant: &str,
    phase: u8,
) -> health::AttemptNotifier {
    let tx = tx.clone();
    let node = node.to_owned();
    let variant = variant.to_owned();
    Box::new(move |attempt| {
        if let Some(tx) = &tx {
            let _ = tx.send(ProgressEvent::ReadinessProbeAttempt {
                node: node.clone(),
                variant: variant.clone(),
                phase,
                attempt,
            });
        }
    })
}

/// Guard that aborts a spawned task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Resolve working directory from variant > node > project root.
fn resolve_working_dir(
    variant_cwd: Option<&str>,
    node_cwd: Option<&str>,
    project_root: &Path,
    ctx: &VariableContext,
) -> Result<PathBuf, crate::variables::VariableError> {
    let raw_cwd = variant_cwd.or(node_cwd);
    match raw_cwd {
        Some(cwd_tmpl) => {
            let resolved = crate::variables::interpolate(cwd_tmpl, ctx)?;
            let p = std::path::Path::new(&resolved);
            if p.is_absolute() {
                Ok(p.to_path_buf())
            } else {
                Ok(project_root.join(p))
            }
        }
        None => Ok(project_root.to_path_buf()),
    }
}

/// Whether `sel` is the run's terminal one-off node (`--oneshot`), given the
/// precomputed terminal node key. Such a node is present in the execution plan
/// (so its dependencies resolve) but must be dropped from startup execution and
/// the healthy-node count — it runs later via [`Orchestrator::run_terminal`].
/// Every pass over the plan that executes or counts nodes routes through this
/// so the invariant "deps run at startup, terminal node runs after" holds in
/// one place. The pre-stage `Pending` seeding is the deliberate exception: the
/// terminal node IS seeded so reverse-order teardown can find it.
fn is_terminal(terminal_key: Option<&str>, sel: &NodeSelection) -> bool {
    terminal_key == Some(RunState::node_key(&sel.node, &sel.variant).as_str())
}

/// Execute a single node in isolation (no `&self`). Designed to be spawned
/// into a `JoinSet` for parallel execution within a stage.
async fn execute_node_isolated(
    ctx: NodeExecutionContext,
    sel: NodeSelection,
    precomputed: Option<PrecomputedServer>,
    index: usize,
    total: usize,
) -> Result<NodeExecutionResult, OrchestratorError> {
    let start_time = std::time::Instant::now();
    let key = RunState::node_key(&sel.node, &sel.variant);

    emit_progress(
        &ctx.progress_tx,
        ProgressEvent::NodeStarting {
            node: sel.node.clone(),
            variant: sel.variant.clone(),
            index,
            total,
        },
    );

    let node_cfg = &ctx.config.nodes[&sel.node];
    let variant_cfg = &node_cfg.variants[&sel.variant];
    // Resolved once per node execution and threaded down, so the start path
    // cannot disagree with the graph or the snapshot about what this variant is.
    let resolved = config::resolve_variant(&ctx.config, node_cfg, variant_cfg);
    let sensitive_outputs = resolved.sensitive_outputs.clone();
    let mut node_state = NodeState::new(&sel.node, &sel.variant);
    node_state.status = NodeStatus::Starting;

    // Build variable context.
    let mut var_ctx = VariableContext::new();
    BuiltinScope {
        run_name: &ctx.run_name,
        run_id: Some(ctx.run_id.to_string()),
        project_root: &ctx.project_root,
        project_name: &ctx.config.name,
        worktree: &ctx.worktree,
        branch: &ctx.branch,
        username: &ctx.username,
        node: Some((&sel.node, &sel.variant)),
    }
    .apply(&mut var_ctx);

    for (name, value) in ctx.vars.as_ref() {
        var_ctx.set_var(name, value.clone());
    }

    // Populate node output references from already-executed nodes.
    for (node_key, outputs) in ctx.all_outputs.as_ref() {
        for (field, value) in outputs {
            var_ctx.set_node_output(&format!("nodes.{node_key}.{field}"), value.clone());
        }
    }

    let server_handle = match resolved.step_type {
        StepType::StartServer => Some(
            execute_start_server_isolated(
                &ctx,
                &sel,
                &resolved,
                &mut var_ctx,
                &mut node_state,
                precomputed,
            )
            .await?,
        ),
        StepType::Command => {
            execute_command_isolated(&ctx, &sel, &resolved, &mut var_ctx, &mut node_state).await?;
            None
        }
    };

    // Mark sensitive output keys.
    if let Some(sensitive) = sensitive_outputs {
        node_state.sensitive_keys = sensitive;
    }

    // Emit completion event.
    let elapsed_ms = start_time.elapsed().as_millis() as u64;
    match node_state.status {
        NodeStatus::Healthy => {
            emit_progress(
                &ctx.progress_tx,
                ProgressEvent::NodeHealthy {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                    url: node_state.url.clone(),
                    elapsed_ms,
                },
            );
        }
        NodeStatus::Skipped => {
            emit_progress(
                &ctx.progress_tx,
                ProgressEvent::NodeSkipped {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                },
            );
        }
        _ => {}
    }

    Ok(NodeExecutionResult {
        key,
        sel,
        index,
        node_state,
        server_handle,
    })
}

/// Execute a `start_server` node without `&self`. Returns the `ServerHandle`.
async fn execute_start_server_isolated(
    ctx: &NodeExecutionContext,
    sel: &NodeSelection,
    resolved: &config::ResolvedVariant,
    var_ctx: &mut VariableContext,
    node_state: &mut NodeState,
    precomputed: Option<PrecomputedServer>,
) -> Result<process::ServerHandle, OrchestratorError> {
    let node_cfg = &ctx.config.nodes[&sel.node];
    let variant_cfg = &node_cfg.variants[&sel.variant];

    let mut precomputed =
        precomputed.expect("precomputed server info missing for start_server node");
    let port = precomputed.port;
    let node_url = precomputed.hostname.clone();
    let https_url = precomputed.https_url.clone();
    let port_reservation = precomputed
        .reservation
        .take()
        .expect("port reservation already consumed — node executed twice?");
    let extra_reservations = std::mem::take(&mut precomputed.extra_reservations);

    node_state.port = Some(port);
    var_ctx.set_builtin("port", port.to_string());
    // `${veld.port}` stays the primary; each declared name is also addressable.
    for (name, value) in &precomputed.named_ports {
        var_ctx.set_builtin(&format!("ports.{name}"), value.to_string());
        node_state
            .outputs
            .insert(format!("ports.{name}"), value.to_string());
    }
    node_state.url = Some(https_url.clone());
    var_ctx.set_builtin("url", https_url.clone());
    // Expose individual URL location pieces (mirrors the Web URL API).
    var_ctx.set_builtin("url.hostname", node_url.clone());
    var_ctx.set_builtin(
        "url.host",
        if ctx.https_port == 443 {
            node_url.clone()
        } else {
            format!("{}:{}", node_url, ctx.https_port)
        },
    );
    var_ctx.set_builtin("url.origin", https_url.clone());
    var_ctx.set_builtin("url.scheme", "https".to_owned());
    var_ctx.set_builtin("url.port", ctx.https_port.to_string());

    emit_progress(
        &ctx.progress_tx,
        ProgressEvent::PortAllocated {
            node: sel.node.clone(),
            variant: sel.variant.clone(),
            port,
        },
    );
    debug_log_free(
        &ctx.debug_writer,
        &format!(
            "{}:{} — using pre-computed port {} → {}",
            sel.node, sel.variant, port, https_url
        ),
    )
    .await;

    // Configure DNS + Caddy via helper (best-effort).
    debug_log_free(
        &ctx.debug_writer,
        &format!(
            "{}:{} — adding DNS host {} → 127.0.0.1",
            sel.node, sel.variant, node_url
        ),
    )
    .await;
    if let Err(e) = ctx.helper_client.add_host(&node_url, "127.0.0.1").await {
        tracing::warn!(error = %e, "failed to add DNS host via helper");
    }
    let mut route = serde_json::json!({
        // Route id is keyed by run NAME, which is not unique across projects —
        // two repos both on `main` collide here and in GC. Tracked as #170;
        // changing the format needs a migration for already-stored routes.
        "route_id": format!("veld-{}-{}-{}", ctx.run_name, sel.node, sel.variant),
        "hostname": &node_url,
        "upstream": format!("localhost:{port}"),
    });
    // Resolve per-node feature flags (variant > node > project > default).
    let features = resolved.features;

    // Include feedback config so Caddy routes /__veld__/* to the daemon.
    // The proxy routes are created whenever a feature is enabled, even if
    // inject is false (manual injection mode — user adds script tags themselves).
    if features.feedback_overlay || features.client_logs {
        route["feedback_upstream"] = serde_json::json!(crate::instance::daemon_upstream());
        route["run_name"] = serde_json::json!(&ctx.run_name);
        route["project_root"] = serde_json::json!(ctx.project_root.to_string_lossy());
    }

    route["inject"] = serde_json::json!(features.inject);
    route["inject_feedback_overlay"] = serde_json::json!(features.feedback_overlay);
    route["inject_client_logs"] = serde_json::json!(features.client_logs);

    // Resolve client log levels (variant > node > project > default).
    let client_log_levels = resolved.client_log_levels.clone();
    route["client_log_levels"] = serde_json::json!(client_log_levels.join(","));

    // Resolve reverse-proxy header rules (variant > node > project). Only sent
    // when non-empty — an absent `proxy` key means "no manipulation" to the
    // helper, so old behavior (Origin passes through) holds by default.
    let proxy = resolved.proxy.clone();
    if !proxy.is_empty() {
        route["proxy"] = serde_json::json!(proxy);
    }
    if let Err(e) = ctx.helper_client.add_route(route).await {
        tracing::warn!(error = %e, "failed to add Caddy route via helper");
    }

    // Resolve working directory (variant > node > project root).
    let working_dir = resolve_working_dir(
        variant_cfg.cwd.as_deref(),
        node_cfg.cwd.as_deref(),
        &ctx.project_root,
        var_ctx,
    )?;

    // Resolve command.
    let command = resolved
        .command
        .clone()
        .unwrap_or_else(|| config::CommandSpec::Shell(String::new()));
    let resolved_cmd = command.interpolate(var_ctx)?;
    debug_log_free(
        &ctx.debug_writer,
        &format!(
            "{}:{} — resolved command: {} (cwd: {})",
            sel.node,
            sel.variant,
            resolved_cmd.display(),
            working_dir.display()
        ),
    )
    .await;

    // Build env (variant > node > project).
    let (mut env, env_secret_keys) = build_env(
        resolved.env.as_ref(),
        var_ctx,
        &format!("nodes.{}.variants.{}", sel.node, sel.variant),
        &ctx.project_root,
    )
    .await?;
    // Env keys declared `secret: true` are masked and encrypted at rest just like
    // sensitive outputs — same machinery, extended rather than duplicated.
    node_state.sensitive_keys.extend(env_secret_keys);
    env.insert("VELD_PORT".to_owned(), port.to_string());
    for (name, value) in &precomputed.named_ports {
        env.insert(
            format!("VELD_PORT_{}", name.to_uppercase().replace('-', "_")),
            value.to_string(),
        );
    }
    env.insert("VELD_URL".to_owned(), https_url.clone());

    // Resolve synthetic outputs.
    //
    // Sensitivity has to travel with the value. A synthetic output is a *template*,
    // so `{"DSN": "postgres://u:${SECRET_PW}@h/db"}` resolves a value that is every
    // bit as sensitive as `SECRET_PW` — but the key `DSN` was never declared
    // sensitive, so it was persisted and displayed in the clear. Marking a secret
    // and then handing it to a template that launders it is worse than not offering
    // the flag: the author believes it is covered.
    //
    // The taint check runs *after* `sensitive_keys` already holds the declared
    // `sensitive_outputs` and the secret env keys, so both are in scope here.
    if let Some(Outputs::Synthetic(ref map)) = resolved.outputs {
        let secret_vars: Vec<String> = ctx
            .config
            .vars
            .iter()
            .flatten()
            .filter(|(_, value)| value.secret)
            .map(|(name, _)| name.clone())
            .collect();
        let mut tainted: Vec<String> = Vec::new();
        for (okey, tmpl) in map {
            let val = crate::variables::interpolate(tmpl, var_ctx)?;
            if template_is_tainted(tmpl, &node_state.sensitive_keys, &secret_vars) {
                tainted.push(okey.clone());
            }
            node_state.outputs.insert(okey.clone(), val);
        }
        node_state.sensitive_keys.extend(tainted);
    }

    // Start the process.
    let log_target = process::LogTarget {
        db: ctx.db.clone(),
        project_root: ctx.project_root.as_ref().clone(),
        run_name: ctx.run_name.clone(),
        run_id: ctx.run_id.to_string(),
        node: sel.node.clone(),
        variant: sel.variant.clone(),
    };

    // Deliver declared files before the process starts, so it can read them on
    // its first line. Failing here aborts the node rather than letting it start
    // and fail obscurely on a missing certificate.
    if let Some(files) = &resolved.files {
        crate::values::deliver_files(
            files,
            &ctx.project_root,
            &format!("nodes.{}.variants.{}", sel.node, sel.variant),
        )
        .await?;
    }

    // Release every reservation immediately before spawning, so the child can
    // bind them.
    port_reservation.release();
    for reservation in extra_reservations {
        reservation.release();
    }

    let handle = process::start_server(
        &resolved_cmd,
        &working_dir,
        &env,
        log_target,
        ctx.foreground,
    )
    .await?;
    let pid = handle.pid();
    node_state.pid = Some(pid);

    // Checkpoint: persist the PID immediately so Ctrl+C during health
    // checks still allows `veld stop` to find and kill this process.
    {
        let key = RunState::node_key(&sel.node, &sel.variant);
        // The DB write happens INSIDE the checkpoint lock: `save_run` replaces
        // the whole run (all nodes), so two parallel node tasks snapshotting
        // and writing outside the lock could interleave and the older snapshot
        // would clobber the newer one — dropping a just-spawned PID from the
        // DB right when Ctrl+C needs it. The write is a few ms of blocking
        // I/O; the lock has no `.await` inside, so it stays cancellation-safe.
        // Recover from a poisoned mutex (a sibling task panicked mid-
        // checkpoint): losing that task's partial update is fine, but
        // panicking here too would leak this task's just-spawned process.
        let mut checkpoint = ctx
            .checkpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        checkpoint.run.execution_order.push(key.clone());
        checkpoint.run.nodes.insert(key, node_state.clone());
        let _ = ctx
            .db
            .save_run(&checkpoint.project_root, &ctx.config.name, &checkpoint.run);
    }

    // Readiness probe — inlined to emit progress events between phases.
    debug_log_free(
        &ctx.debug_writer,
        &format!(
            "{}:{} — process started (pid {}), beginning readiness checks",
            sel.node, sel.variant, pid
        ),
    )
    .await;
    // Use probes.readiness if available, falling back to legacy health_check.
    if let Some(hc) = resolved.readiness.clone() {
        node_state.status = NodeStatus::HealthChecking;
        node_state.readiness_phases.push(ReadinessPhase {
            phase: 1,
            passed: false,
            last_error: None,
            passed_at: None,
        });
        node_state.readiness_phases.push(ReadinessPhase {
            phase: 2,
            passed: false,
            last_error: None,
            passed_at: None,
        });

        // Spawn a background log watcher that streams service output to the
        // progress channel after a delay.  This gives the user visibility
        // into what the service is doing when health checks are slow.
        // The `_log_watcher` guard aborts the task when it goes out of scope
        // (i.e. when the health check completes, whether success or failure).
        let _log_watcher = {
            let tx = ctx.progress_tx.clone();
            let db = ctx.db.clone();
            let project_root = ctx.project_root.as_ref().clone();
            let run_name = ctx.run_name.clone();
            let node = sel.node.clone();
            let variant = sel.variant.clone();
            AbortOnDrop(tokio::spawn(async move {
                // Give the service time to start normally before showing logs.
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;

                let filter = LogFilter {
                    node: Some(node.clone()),
                    variant: Some(variant.clone()),
                    streams: Some(vec![LogStream::Server.as_str()]),
                    run_id: None,
                };
                let mut last_id: i64 = 0;
                loop {
                    if let Ok(rows) = db.logs_after_id(&project_root, &run_name, &filter, last_id) {
                        if let Some(max) = rows.last().map(|r| r.id) {
                            last_id = max;
                        }
                        let lines: Vec<String> = rows
                            .into_iter()
                            .map(|r| format!("[{}] {}", r.ts, r.line))
                            .filter(|l| !l.is_empty())
                            .collect();
                        if !lines.is_empty() {
                            if let Some(ref tx) = tx {
                                let _ = tx.send(ProgressEvent::NodeLogLines {
                                    node: node.clone(),
                                    variant: variant.clone(),
                                    lines,
                                });
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
                }
            }))
        };

        // Build attempt notifiers for health check phases.
        let phase1_notifier = make_attempt_notifier(&ctx.progress_tx, &sel.node, &sel.variant, 1);
        let phase2_notifier = make_attempt_notifier(&ctx.progress_tx, &sel.node, &sel.variant, 2);

        // Which port this probe watches: a named one from the `ports` map, or
        // the primary. Probing the wrong port on a multi-port node reports ready
        // too early — a debugger port opens long before the app is listening.
        let probe_port = match hc.port.as_deref() {
            None => port,
            Some(name) => match precomputed.named_ports.get(name) {
                Some(p) => *p,
                None => {
                    return Err(OrchestratorError::NodeFailed {
                        node: sel.node.clone(),
                        variant: sel.variant.clone(),
                        reason: format!(
                            "readiness probe references port \"{name}\", which this node \
                             does not declare in `ports`"
                        ),
                    });
                }
            },
        };

        // Phase 1: TCP port check.
        emit_progress(
            &ctx.progress_tx,
            ProgressEvent::ReadinessProbePhase {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                phase: 1,
                description: format!("waiting for port {probe_port}"),
            },
        );

        let phase1_result = tokio::select! {
            result = health::wait_for_port(probe_port, &hc, Some(&phase1_notifier)) => result,
            _ = wait_for_process_exit(pid) => {
                Err(health::HealthError::PortCheckFailed(
                    "server process exited before binding to port".into(),
                ))
            }
        };

        if let Err(e) = phase1_result {
            let msg = format!("process did not bind to port {probe_port}: {e}");
            node_state.status = NodeStatus::Failed;
            node_state.readiness_phases[0].last_error = Some(msg.clone());
            debug_log_free(
                &ctx.debug_writer,
                &format!(
                    "{}:{} — readiness phase 1 FAILED: {}",
                    sel.node, sel.variant, msg
                ),
            )
            .await;
            emit_progress(
                &ctx.progress_tx,
                ProgressEvent::NodeFailed {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                    error: msg.clone(),
                },
            );
            return Err(OrchestratorError::NodeFailed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                reason: msg,
            });
        }

        let now = chrono::Utc::now();
        node_state.readiness_phases[0].passed = true;
        node_state.readiness_phases[0].passed_at = Some(now);
        emit_progress(
            &ctx.progress_tx,
            ProgressEvent::ReadinessProbePassed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                phase: 1,
            },
        );
        debug_log_free(
            &ctx.debug_writer,
            &format!("{}:{} — phase 1 passed (port open)", sel.node, sel.variant),
        )
        .await;

        // Phase 2: depends on check type.
        let phase2_desc = match hc.check_type.as_str() {
            "http" => format!("HTTP check on port {probe_port}"),
            "command" | "bash" => "command readiness check".to_owned(),
            "port" => "port-only (no phase 2)".to_owned(),
            other => format!("unknown check type: {other}"),
        };
        emit_progress(
            &ctx.progress_tx,
            ProgressEvent::ReadinessProbePhase {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                phase: 2,
                description: phase2_desc,
            },
        );

        let phase2_future = async {
            match hc.check_type.as_str() {
                "http" => {
                    let direct_url = format!("http://127.0.0.1:{probe_port}");
                    health::wait_for_http(&direct_url, &hc, Some(&phase2_notifier)).await
                }
                "command" | "bash" => {
                    if let Some(cmd) = hc.cmd.spec() {
                        health::wait_for_command_check(
                            &cmd,
                            &working_dir,
                            &hc,
                            Some(&phase2_notifier),
                        )
                        .await
                    } else {
                        Ok(())
                    }
                }
                _ => Ok(()), // "port" and unknown — phase 1 already covers.
            }
        };

        let phase2_result = tokio::select! {
            result = phase2_future => result,
            _ = wait_for_process_exit(pid) => {
                Err(health::HealthError::PortCheckFailed(
                    "server process exited during readiness check".into(),
                ))
            }
        };

        match phase2_result {
            Ok(()) => {
                let now = chrono::Utc::now();
                node_state.readiness_phases[1].passed = true;
                node_state.readiness_phases[1].passed_at = Some(now);
                node_state.status = NodeStatus::Healthy;
                emit_progress(
                    &ctx.progress_tx,
                    ProgressEvent::ReadinessProbePassed {
                        node: sel.node.clone(),
                        variant: sel.variant.clone(),
                        phase: 2,
                    },
                );
                debug_log_free(
                    &ctx.debug_writer,
                    &format!(
                        "{}:{} — readiness check passed, node is healthy",
                        sel.node, sel.variant
                    ),
                )
                .await;
            }
            Err(e) => {
                node_state.status = NodeStatus::Failed;
                let msg = e.to_string();
                node_state.readiness_phases[1].last_error = Some(msg.clone());
                debug_log_free(
                    &ctx.debug_writer,
                    &format!(
                        "{}:{} — readiness phase 2 FAILED: {}",
                        sel.node, sel.variant, msg
                    ),
                )
                .await;
                emit_progress(
                    &ctx.progress_tx,
                    ProgressEvent::NodeFailed {
                        node: sel.node.clone(),
                        variant: sel.variant.clone(),
                        error: msg.clone(),
                    },
                );
                return Err(OrchestratorError::NodeFailed {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                    reason: msg,
                });
            }
        }
    } else {
        node_state.status = NodeStatus::Healthy;
    }

    Ok(handle)
}

/// Execute a `command` node without `&self`.
async fn execute_command_isolated(
    ctx: &NodeExecutionContext,
    sel: &NodeSelection,
    resolved: &config::ResolvedVariant,
    var_ctx: &mut VariableContext,
    node_state: &mut NodeState,
) -> Result<(), OrchestratorError> {
    let node_cfg = &ctx.config.nodes[&sel.node];
    let variant_cfg = &node_cfg.variants[&sel.variant];

    // Resolve working directory (variant > node > project root).
    let working_dir = resolve_working_dir(
        variant_cfg.cwd.as_deref(),
        node_cfg.cwd.as_deref(),
        &ctx.project_root,
        var_ctx,
    )?;

    // Resolve command or script.
    let raw_cmd = match &resolved.script {
        Some(script) => config::CommandSpec::script(&ctx.project_root.join(script)),
        None => resolved
            .command
            .clone()
            .unwrap_or_else(|| config::CommandSpec::Shell(String::new())),
    };
    let resolved_cmd = raw_cmd.interpolate(var_ctx)?;

    // Build env (variant > node > project).
    let (env, env_secret_keys) = build_env(
        resolved.env.as_ref(),
        var_ctx,
        &format!("nodes.{}.variants.{}", sel.node, sel.variant),
        &ctx.project_root,
    )
    .await?;
    node_state.sensitive_keys.extend(env_secret_keys);

    if let Some(files) = &resolved.files {
        crate::values::deliver_files(
            files,
            &ctx.project_root,
            &format!("nodes.{}.variants.{}", sel.node, sel.variant),
        )
        .await?;
    }

    // Idempotency check (skip_if).
    if let Some(ref skip_if_cmd) = resolved.skip_if {
        let skip_if_resolved = skip_if_cmd.interpolate(var_ctx)?;
        let skip_if_result =
            process::run_command(&skip_if_resolved, &working_dir, &env, None).await;
        if let Ok(ref out) = skip_if_result {
            if out.exit_code == 0 {
                tracing::info!(
                    node = sel.node,
                    variant = sel.variant,
                    "skip_if passed — skipping command step"
                );
                node_state.status = NodeStatus::Skipped;
                node_state
                    .outputs
                    .insert("exit_code".to_owned(), "0".to_owned());
                return Ok(());
            }
        }
    }

    // Run command step.
    emit_progress(
        &ctx.progress_tx,
        ProgressEvent::CommandRunning {
            node: sel.node.clone(),
            variant: sel.variant.clone(),
        },
    );
    let output_file =
        logging::output_file(&ctx.project_root, &ctx.run_name, &sel.node, &sel.variant);
    let result =
        process::run_command(&resolved_cmd, &working_dir, &env, Some(&output_file)).await?;

    node_state
        .outputs
        .insert("exit_code".to_owned(), result.exit_code.to_string());

    // Filter outputs against declared keys.
    let declared_keys = resolved
        .outputs
        .as_ref()
        .map(|o| o.declared_keys())
        .unwrap_or_default();

    for (k, v) in &result.outputs {
        if declared_keys.contains(k.as_str()) {
            node_state.outputs.insert(k.clone(), v.clone());
        } else if resolved.strict_outputs {
            let reason = format!(
                "undeclared output \"{k}\" — add it to \"outputs\" or set \"strict_outputs\": false"
            );
            emit_progress(
                &ctx.progress_tx,
                ProgressEvent::NodeFailed {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                    error: reason.clone(),
                },
            );
            return Err(OrchestratorError::NodeFailed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                reason,
            });
        } else {
            tracing::warn!(
                node = sel.node,
                variant = sel.variant,
                key = k,
                "ignoring undeclared output"
            );
        }
    }

    // F9.3: the map form on a `command` node publishes computed values, so a build
    // step can say where its artifact landed. Interpolated *after* the command
    // ran, with its captured outputs in scope as `${output.*}`, which is the whole
    // point — the value usually depends on what the command produced.
    if let Some(Outputs::Synthetic(ref map)) = resolved.outputs {
        for (key, value) in &node_state.outputs {
            var_ctx.set_output(key, value.clone());
        }
        for (key, template) in map {
            let value = crate::variables::interpolate(template, var_ctx)?;
            node_state.outputs.insert(key.clone(), value);
        }
    }

    if result.exit_code == 0 {
        // Run readiness probe if configured (probes.readiness on command nodes).
        if let Some(hc) = resolved.readiness.clone() {
            node_state.status = NodeStatus::HealthChecking;
            emit_progress(
                &ctx.progress_tx,
                ProgressEvent::ReadinessProbePhase {
                    node: sel.node.clone(),
                    variant: sel.variant.clone(),
                    phase: 1,
                    description: "readiness probe".to_owned(),
                },
            );

            let notifier = make_attempt_notifier(&ctx.progress_tx, &sel.node, &sel.variant, 1);
            let probe_result = match hc.check_type.as_str() {
                "command" | "bash" => {
                    if let Some(cmd) = hc.cmd.spec() {
                        health::wait_for_command_check(&cmd, &working_dir, &hc, Some(&notifier))
                            .await
                    } else {
                        Ok(())
                    }
                }
                "port" => {
                    // Port check — look for a port value in outputs.
                    // Checks common key names; a future enhancement could add
                    // an explicit `port_key` field to HealthCheck.
                    let port_str = node_state
                        .outputs
                        .get("PORT")
                        .or(node_state.outputs.get("DB_PORT"))
                        .or(node_state.outputs.get("SERVICE_PORT"));
                    if let Some(port_str) = port_str {
                        if let Ok(port) = port_str.parse::<u16>() {
                            health::wait_for_port(port, &hc, Some(&notifier)).await
                        } else {
                            tracing::warn!(
                                node = sel.node,
                                variant = sel.variant,
                                "readiness port probe: output value is not a valid port number"
                            );
                            Ok(())
                        }
                    } else {
                        tracing::warn!(
                            node = sel.node,
                            variant = sel.variant,
                            "readiness port probe skipped: no PORT/DB_PORT/SERVICE_PORT output found"
                        );
                        Ok(())
                    }
                }
                "http" => {
                    // HTTP check — look for a URL value in outputs.
                    let url = node_state
                        .outputs
                        .get("URL")
                        .or(node_state.outputs.get("DATABASE_URL"))
                        .or(node_state.outputs.get("SERVICE_URL"));
                    if let Some(url) = url {
                        health::wait_for_http(url, &hc, Some(&notifier)).await
                    } else {
                        tracing::warn!(
                            node = sel.node,
                            variant = sel.variant,
                            "readiness http probe skipped: no URL/DATABASE_URL/SERVICE_URL output found"
                        );
                        Ok(())
                    }
                }
                _ => Ok(()),
            };

            match probe_result {
                Ok(()) => {
                    node_state.status = NodeStatus::Healthy;
                    emit_progress(
                        &ctx.progress_tx,
                        ProgressEvent::ReadinessProbePassed {
                            node: sel.node.clone(),
                            variant: sel.variant.clone(),
                            phase: 1,
                        },
                    );
                }
                Err(e) => {
                    node_state.status = NodeStatus::Failed;
                    let reason = format!("readiness probe failed: {e}");
                    emit_progress(
                        &ctx.progress_tx,
                        ProgressEvent::NodeFailed {
                            node: sel.node.clone(),
                            variant: sel.variant.clone(),
                            error: reason.clone(),
                        },
                    );
                    return Err(OrchestratorError::NodeFailed {
                        node: sel.node.clone(),
                        variant: sel.variant.clone(),
                        reason,
                    });
                }
            }
        } else {
            node_state.status = NodeStatus::Healthy;
        }
    } else {
        node_state.status = NodeStatus::Failed;
        let reason = format!("command step exited with code {}", result.exit_code);
        emit_progress(
            &ctx.progress_tx,
            ProgressEvent::NodeFailed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                error: reason.clone(),
            },
        );
        return Err(OrchestratorError::NodeFailed {
            node: sel.node.clone(),
            variant: sel.variant.clone(),
            reason,
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Poll until a process is no longer alive. Checks every 250ms.
/// Used to race readiness checks against premature process death so the
/// orchestrator can fail fast instead of waiting for the full timeout.
async fn wait_for_process_exit(pid: u32) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if !process::is_alive(pid) {
            return;
        }
    }
}

/// Build the environment map: interpolate inline values, and dereference every
/// configured source (F7).
///
/// Only an **inline literal** is interpolated. A value fetched from a file, the
/// environment, or a command is used verbatim — substituting `${…}` inside
/// fetched content would turn any secret store into an interpolation vector, and
/// a password that happens to contain `${` would either break or, worse, expand.
///
/// Returns the resolved values plus the keys declared `secret`, so the caller can
/// mark them sensitive without the values themselves needing a wrapper type.
async fn build_env(
    env_config: Option<&HashMap<String, config::ConfigValue>>,
    ctx: &VariableContext,
    at_prefix: &str,
    project_root: &Path,
) -> Result<(HashMap<String, String>, Vec<String>), OrchestratorError> {
    let mut env = HashMap::new();
    let mut secret_keys = Vec::new();
    let Some(map) = env_config else {
        return Ok((env, secret_keys));
    };
    // Sorted so a failure is deterministic: with two broken sources the same one
    // is always reported.
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    for key in keys {
        let value = &map[key];
        let resolved = match value.as_literal() {
            Some(tmpl) => crate::variables::interpolate(tmpl, ctx)?,
            None => {
                crate::values::resolve_value(
                    value,
                    &format!("{at_prefix}.env.{key}"),
                    Some(project_root),
                )
                .await?
            }
        };
        env.insert(key.clone(), resolved);
        if value.secret {
            secret_keys.push(key.clone());
        }
    }
    Ok((env, secret_keys))
}

fn whoami_username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn whoami_hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|| "localhost".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reap_only_proven_dead_orphans() {
        // Live processes are never reaped, whatever the status.
        assert!(!is_reapable_orphan(&RunStatus::Running, true, true));
        assert!(!is_reapable_orphan(&RunStatus::Starting, true, true));

        // Running with no live PIDs: startup finished, processes died → reap.
        assert!(is_reapable_orphan(&RunStatus::Running, false, true));
        assert!(is_reapable_orphan(&RunStatus::Running, false, false));

        // Starting that spawned then died → reap.
        assert!(is_reapable_orphan(&RunStatus::Starting, false, true));

        // Starting that never spawned → still starting (pre-spawn / slow
        // command stage); must NOT be reaped, or a concurrent `veld start`
        // would delete a run that is still coming up.
        assert!(!is_reapable_orphan(&RunStatus::Starting, false, false));

        // Terminal / transitional statuses are never reaped here.
        for status in [
            RunStatus::Stopping,
            RunStatus::Stopped,
            RunStatus::Failed,
            RunStatus::Crashed,
        ] {
            assert!(!is_reapable_orphan(&status, false, false));
            assert!(!is_reapable_orphan(&status, false, true));
        }
    }

    /// Build a minimal orchestrator backed by a throwaway database, with no
    /// helper interaction — enough to exercise `run_terminal` in isolation.
    fn test_orchestrator(project_root: &std::path::Path, config: VeldConfig) -> Orchestrator {
        let db = Db::open_at(&project_root.join("veld.db")).unwrap();
        Orchestrator {
            config,
            config_path: project_root.join("veld.json"),
            config_hash: String::new(),
            project_root: project_root.to_path_buf(),
            db,
            port_allocator: PortAllocator::new(),
            helper_client: HelperClient::default_client(),
            https_port: 443,
            children: HashMap::new(),
            precomputed_servers: HashMap::new(),
            debug: false,
            debug_writer: None,
            foreground: false,
            progress_tx: None,
            internal_log: None,
            terminal_node: None,
            resolved_vars: None,
            terminal_outputs: Some(HashMap::new()),
        }
    }

    /// F0.3: one builtin set, so `${veld.node}` (and every sibling) resolves the
    /// same way on the start, terminal, and stop paths. Before this, `node` and
    /// `variant` existed only in the action context.
    #[test]
    fn builtin_scope_is_one_closed_set() {
        let root = std::path::Path::new("/projects/app");
        let mut ctx = VariableContext::new();
        BuiltinScope {
            run_name: "dev",
            run_id: Some("abc".to_owned()),
            project_root: root,
            project_name: "app",
            worktree: "My Worktree",
            branch: "feature/Thing",
            username: "sam",
            node: Some(("api", "local")),
        }
        .apply(&mut ctx);

        let got = |k: &str| ctx.builtins.get(k).cloned().unwrap_or_default();
        assert_eq!(got("run"), "dev");
        assert_eq!(got("run_id"), "abc");
        assert_eq!(got("root"), "/projects/app");
        assert_eq!(got("project"), "app");
        assert_eq!(got("name"), "app");
        assert_eq!(got("node"), "api");
        assert_eq!(got("variant"), "local");
        assert_eq!(got("username"), "sam");
        // worktree/branch are slugified for URL safety.
        assert_eq!(got("worktree"), url::slugify("My Worktree"));
        assert_eq!(got("branch"), url::slugify("feature/Thing"));

        // A project-level step has no node, and must not invent one.
        let mut project_ctx = VariableContext::new();
        BuiltinScope {
            run_name: "dev",
            run_id: None,
            project_root: root,
            project_name: "app",
            worktree: "app",
            branch: "main",
            username: "sam",
            node: None,
        }
        .apply(&mut project_ctx);
        assert!(!project_ctx.builtins.contains_key("node"));
        assert!(!project_ctx.builtins.contains_key("variant"));
    }

    /// F0.2 exit gate: an output named like a builtin must not shadow it.
    ///
    /// The `on_stop` path used to inject every node output into the *builtins*
    /// map, so a node with an output called `run` made `${veld.run}` resolve to
    /// the output during teardown and to the run name everywhere else — the same
    /// string, two values.
    ///
    /// `run` is the probe here, not `port`: on the old code `set_builtin("port",
    /// …)` ran *after* the outputs loop and overwrote it, so `port` was never
    /// actually shadowable. The genuinely shadowable builtins were the ones set
    /// before the loop — `run`, `branch`, `worktree`, `root`, `project`, `name`,
    /// `username`. `port` is kept below as a control: it must still resolve to
    /// the allocated port, and the output must still be reachable as
    /// `${output.port}`.
    ///
    /// This also covers F0.3 on the stop path (`${veld.node}`/`${veld.variant}`).
    #[tokio::test]
    async fn output_does_not_shadow_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let marker = project_root.join("resolved");

        let config: VeldConfig = serde_json::from_str(&format!(
            r#"{{
                "schemaVersion": "3",
                "name": "testcfg",
                "nodes": {{
                    "svc": {{ "default_variant": "local", "variants": {{
                        "local": {{
                            "type": "start_server",
                            "shell": "sleep 30",
                            "on_stop": "printf '%s' \"${{veld.run}}|${{veld.port}}|${{veld.node}}|${{veld.variant}}|${{output.run}}|${{output.port}}\" > {}"
                        }}
                    }}}}
                }}
            }}"#,
            marker.display()
        ))
        .unwrap();

        let mut orch = test_orchestrator(project_root, config.clone());
        let key = RunState::node_key("svc", "local");
        let mut run = RunState::new("testrun", &config.name);
        run.status = RunStatus::Running;
        let mut ns = NodeState::new("svc", "local");
        ns.status = NodeStatus::Healthy;
        // The allocated port…
        ns.port = Some(12345);
        // …and node outputs that happen to be named like builtins. `run` was
        // genuinely shadowable before F0.2; `port` never was (see above).
        ns.outputs.insert("run".to_owned(), "SHADOW".to_owned());
        ns.outputs.insert("port".to_owned(), "9999".to_owned());
        run.nodes.insert(key.clone(), ns);
        run.execution_order.push(key);
        orch.save_state(&run).unwrap();

        orch.stop("testrun").await.expect("stop");

        let resolved = std::fs::read_to_string(&marker).expect("on_stop must have run");
        assert_eq!(
            resolved, "testrun|12345|svc|local|SHADOW|9999",
            "${{veld.run}} must stay the run name and ${{veld.port}} the allocated \
             port; the same-named outputs are reachable only as ${{output.*}}"
        );
    }

    /// F0.1 exit gate: `veld stop` must work against a running environment whose
    /// config is semantically invalid — **and its `on_stop` hooks must still
    /// execute**.
    ///
    /// This is the whole reason validation lives in `config::validate` (called
    /// from `start`) rather than in the loader: `on_stop` is read from the
    /// on-disk config at stop time, so a config error that failed the load would
    /// mean teardown commands never run and containers leak with no way to clean
    /// them up.
    #[tokio::test]
    async fn stop_succeeds_with_invalid_config() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let marker = project_root.join("on-stop-ran");

        // An invalid proxy header name: parses fine, `validate` rejects it.
        let config: VeldConfig = serde_json::from_str(&format!(
            r#"{{
                "schemaVersion": "3",
                "name": "testcfg",
                "proxy": {{ "request": {{ "remove": ["X Frame Options"] }} }},
                "nodes": {{
                    "svc": {{ "default_variant": "local", "variants": {{
                        "local": {{
                            "type": "start_server",
                            "shell": "sleep 30",
                            "on_stop": "touch {}"
                        }}
                    }}}}
                }}
            }}"#,
            marker.display()
        ))
        .unwrap();
        assert!(
            !config::validate(&config).is_empty(),
            "fixture must be semantically invalid"
        );

        let mut orch = test_orchestrator(project_root, config.clone());
        let sel = NodeSelection {
            node: "svc".to_owned(),
            variant: "local".to_owned(),
        };
        let key = RunState::node_key(&sel.node, &sel.variant);

        let mut run = RunState::new("testrun", &config.name);
        run.status = RunStatus::Running;
        let mut ns = NodeState::new(&sel.node, &sel.variant);
        // Anything other than `Pending` — a node that never ran gets no hook.
        ns.status = NodeStatus::Healthy;
        run.nodes.insert(key.clone(), ns);
        run.execution_order.push(key);
        orch.save_state(&run).unwrap();

        let result = orch.stop("testrun").await.expect("stop must not fail");
        assert_eq!(result, StopResult::Stopped);
        assert!(
            marker.exists(),
            "the on_stop hook must still run for an invalid config"
        );
    }

    /// The core `--oneshot` contract: a non-zero terminal exit is returned (not
    /// raised as an error) and the node's result is persisted, appended to the
    /// execution order so reverse-order teardown can find it.
    #[tokio::test]
    async fn run_terminal_propagates_exit_code_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "testcfg",
                "url_template": "{service}.{run}.{project}.localhost",
                "nodes": {
                    "task": { "default_variant": "local", "variants": {
                        "local": { "type": "command", "shell": "echo running; exit 7" }
                    }}
                }
            }"#,
        )
        .unwrap();

        let mut orch = test_orchestrator(project_root, config.clone());
        let sel = NodeSelection {
            node: "task".to_owned(),
            variant: "local".to_owned(),
        };
        let key = RunState::node_key(&sel.node, &sel.variant);

        let mut run = RunState::new("testrun", &config.name);
        run.status = RunStatus::Running;
        run.nodes
            .insert(key.clone(), NodeState::new(&sel.node, &sel.variant));
        orch.save_state(&run).unwrap();

        let code = orch.run_terminal("testrun", &sel).await.unwrap();
        assert_eq!(code, 7, "non-zero exit must be returned, not an error");

        let reloaded = orch.db.get_run(project_root, "testrun").unwrap().unwrap();
        let ns = reloaded.nodes.get(&key).unwrap();
        assert_eq!(ns.status, NodeStatus::Failed);
        assert_eq!(ns.outputs.get("exit_code").map(String::as_str), Some("7"));
        assert!(
            reloaded.execution_order.contains(&key),
            "terminal node must be appended to execution_order for teardown"
        );
    }

    /// A synthetic output built from a sensitive value is itself sensitive.
    ///
    /// This predicate fails *silently* if it breaks — a wrong `false` means a
    /// credential is persisted and displayed in the clear, with nothing in the
    /// output to suggest anything went wrong. So each reference form is pinned,
    /// along with the negative case, since marking everything sensitive would be
    /// an equally silent way to make masking meaningless.
    #[test]
    fn synthetic_output_inherits_sensitivity_from_what_it_reads() {
        let sensitive = vec!["DB_PASSWORD".to_owned(), "TOKEN".to_owned()];
        let secret_vars = vec!["signing_key".to_owned()];

        for tainted in [
            // A `shell`-style env read — the common case for a DSN template.
            "postgres://user:${DB_PASSWORD}@localhost/app",
            "postgres://user:$DB_PASSWORD@localhost/app",
            // This node's own declared-sensitive output.
            "Bearer ${output.TOKEN}",
            // Another node's.
            "Bearer ${nodes.vault.TOKEN}",
            // A secret var.
            "${vars.signing_key}",
        ] {
            assert!(
                template_is_tainted(tainted, &sensitive, &secret_vars),
                "must be tainted: {tainted}"
            );
        }

        for clean in [
            "http://localhost:${veld.port}/health",
            "${output.PUBLIC_URL}",
            "${nodes.api.url}",
            "${vars.region}",
            "no references at all",
        ] {
            assert!(
                !template_is_tainted(clean, &sensitive, &secret_vars),
                "must not be tainted: {clean}"
            );
        }
    }
}
