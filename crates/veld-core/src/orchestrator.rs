#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;
use tracing;

use crate::config::{self, Outputs, StepType, VeldConfig};
use crate::graph::{self, NodeSelection};
use crate::health;
use crate::helper::HelperClient;
use crate::logging::{self, LogWriter};
use crate::port::PortAllocator;
use crate::process;
use crate::progress::ProgressEvent;
use crate::state::{
    GlobalRegistry, HealthCheckPhase, NodeState, NodeStatus, ProjectState, RegistryEntry,
    RegistryRunInfo, RunState, RunStatus,
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
    State(#[from] crate::state::StateError),

    #[error(transparent)]
    Variable(#[from] crate::variables::VariableError),

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

/// Pre-computed port and URL for a `start_server` node, resolved before
/// any node begins execution so that all nodes can reference any other
/// node's URL/port without requiring a dependency edge.
struct PrecomputedServer {
    port: u16,
    /// Raw hostname (without scheme), used for DNS/Caddy configuration.
    hostname: String,
    /// Full `https://` URL including port suffix when not 443.
    https_url: String,
    /// Held TCP listeners that reserve the port from other processes.
    /// Taken (released) right before the child process is spawned.
    reservation: Option<crate::port::PortReservation>,
}

/// Read-only context shared by all node execution tasks within a stage.
/// Cloned once per stage, then each spawned task gets its own copy.
#[derive(Clone)]
struct NodeExecutionContext {
    config: Arc<VeldConfig>,
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
    pub project_root: PathBuf,
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
}

impl Orchestrator {
    /// Create an orchestrator from a discovered config.
    pub fn new(config_path: PathBuf, config: VeldConfig) -> Self {
        let project_root = config::project_root(&config_path);
        Self {
            config,
            config_path,
            project_root,
            port_allocator: PortAllocator::new(),
            helper_client: HelperClient::default_client(),
            https_port: 443,
            children: HashMap::new(),
            precomputed_servers: HashMap::new(),
            debug: false,
            debug_writer: None,
            foreground: false,
            progress_tx: None,
        }
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

    /// Convenience: discover config from CWD and build the orchestrator.
    pub fn from_cwd() -> Result<Self, OrchestratorError> {
        let (path, cfg) = config::load_config_from_cwd()?;
        Ok(Self::new(path, cfg))
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

        let resolved = graph::resolve_selections(selections, &self.config)?;
        let plan = graph::build_execution_plan(&resolved, &self.config)?;

        // Set up debug log writer if debug mode is enabled.
        if self.debug {
            let debug_path = logging::debug_log_file(&self.project_root, run_name);
            match LogWriter::new(debug_path).await {
                Ok(writer) => {
                    let _ = writer
                        .write_line("[VELD] Debug logging enabled — orchestration trace")
                        .await;
                    self.debug_writer = Some(writer);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create debug log writer");
                }
            }
        }

        // Ensure Caddy is running before we add routes.
        if let Err(e) = self.helper_client.caddy_start().await {
            tracing::warn!(error = %e, "failed to start Caddy via helper (routes may fail)");
        }
        self.debug_log("Caddy start requested").await;

        let mut run = RunState::new(run_name, &self.config.name);
        self.debug_log(&format!(
            "Run '{}' created (id: {}), graph has {} stages",
            run_name,
            run.run_id,
            plan.len()
        ))
        .await;

        // Gather context info for URL templates.
        let branch = detect_git_branch(&self.project_root);
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
                let variant_cfg = &self.config.nodes[&sel.node].variants[&sel.variant];
                if variant_cfg.step_type != config::StepType::StartServer {
                    continue;
                }

                let reservation = self.port_allocator.allocate()?;
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
                        hostname: node_url,
                        https_url,
                        reservation: Some(reservation),
                    },
                );
            }
        }

        // Count total nodes for progress reporting.
        let total_nodes: usize = plan.iter().map(|s| s.len()).sum();
        self.emit(ProgressEvent::PlanResolved {
            total_nodes,
            stages: plan.len(),
        });

        // Wrap immutable data in Arc once for all stages.
        let shared_config = Arc::new(self.config.clone());
        let shared_project_root = Arc::new(self.project_root.clone());

        // Execute stages in order. On failure, release any remaining port
        // reservations so the ports become available again immediately.
        let mut node_index: usize = 0;
        let execute_result: Result<(), OrchestratorError> = async {
            for stage in &plan {
                let results = self
                    .execute_stage(
                        stage,
                        &run,
                        &branch,
                        &worktree,
                        &username,
                        &mut all_outputs,
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
            }
            Ok(())
        }
        .await;

        if let Err(e) = execute_result {
            // Release all remaining port reservations so the ports become
            // available to the system immediately.
            self.precomputed_servers.clear();
            return Err(e);
        }

        // All reservations have been consumed — clear the map.
        self.precomputed_servers.clear();

        run.status = RunStatus::Running;

        // Final state save with Running status.
        self.save_state(&run)?;

        Ok(run)
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
        total_nodes: usize,
        node_index: &mut usize,
        shared_config: &Arc<VeldConfig>,
        shared_project_root: &Arc<PathBuf>,
    ) -> Result<Vec<(String, NodeState)>, OrchestratorError> {
        // Build shared context (cloned once per stage).
        let ctx = NodeExecutionContext {
            config: Arc::clone(shared_config),
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
        // Reconnect to whichever helper is running (system or user socket)
        if let Ok(client) = crate::helper::HelperClient::connect().await {
            self.helper_client = client;
        }

        let mut project_state = ProjectState::load(&self.project_root)?;
        let run = project_state
            .get_run_mut(run_name)
            .ok_or_else(|| crate::state::StateError::RunNotFound(run_name.to_owned()))?;

        if run.status == RunStatus::Stopped {
            // Already stopped — clean up state and return.
            project_state.runs.remove(run_name);
            project_state.save(&self.project_root)?;
            self.remove_from_registry(run_name);
            return Ok(StopResult::AlreadyStopped);
        }

        run.status = RunStatus::Stopping;

        // Stop in reverse execution order (dependencies last). Fall back to
        // HashMap keys for runs created before execution_order was tracked.
        let node_keys: Vec<String> = if run.execution_order.is_empty() {
            run.nodes.keys().cloned().collect()
        } else {
            run.execution_order.clone()
        };

        for key in node_keys.iter().rev() {
            if let Some(node_state) = run.nodes.get_mut(key) {
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
                    self.run_on_stop_hook(run_name, node_state).await;
                }

                node_state.status = NodeStatus::Stopped;
                node_state.pid = None;
            }

            // Remove child handle.
            self.children.remove(key);
        }

        // Remove the run from project state entirely (no lingering stopped state).
        project_state.runs.remove(run_name);
        project_state.save(&self.project_root)?;

        // Remove from global registry.
        self.remove_from_registry(run_name);

        Ok(StopResult::Stopped)
    }

    /// Clean up a stale run with the given name if it exists in state.
    /// Kills any live processes, removes DNS/Caddy routes, and clears state.
    /// Errors are logged but never propagated — this is best-effort cleanup.
    async fn cleanup_stale_run(&mut self, run_name: &str) {
        // Always clear stale feedback data so a reused run name starts fresh,
        // even if the run was already removed from state.
        let feedback_dir = self
            .project_root
            .join(".veld")
            .join("feedback")
            .join(run_name);
        if feedback_dir.exists() {
            tracing::info!(run_name, "clearing stale feedback data");
            let _ = std::fs::remove_dir_all(&feedback_dir);
        }

        let project_state = match ProjectState::load(&self.project_root) {
            Ok(s) => s,
            Err(_) => return,
        };

        let run = match project_state.get_run(run_name) {
            Some(r) => r,
            None => return,
        };

        tracing::info!(run_name, "cleaning up stale run before starting");

        // Kill any processes that are still alive.
        for ns in run.nodes.values() {
            if let Some(pid) = ns.pid {
                if process::is_alive(pid) {
                    let _ = process::kill_process(pid).await;
                }
            }
            // Remove DNS + Caddy route.
            if let Some(ref url_str) = ns.url {
                let hostname = url_str.strip_prefix("https://").unwrap_or(url_str);
                // Strip port if present (e.g., "host:18443" → "host")
                let hostname = hostname.split(':').next().unwrap_or(hostname);
                let _ = self.helper_client.remove_host(hostname).await;
                let route_id = format!("veld-{}-{}-{}", run_name, ns.node_name, ns.variant);
                let _ = self.helper_client.remove_route(&route_id).await;
            }
        }

        // Remove from state and registry.
        let mut project_state = project_state;
        project_state.runs.remove(run_name);
        let _ = project_state.save(&self.project_root);
        self.remove_from_registry(run_name);
    }

    /// Clean up ALL runs in the project whose processes have died.
    /// This catches orphaned runs from previous sessions that were not
    /// properly stopped (e.g., due to a crash or `kill -9`).
    async fn cleanup_dead_runs(&mut self) {
        let project_state = match ProjectState::load(&self.project_root) {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut dead_run_names = Vec::new();

        for (run_name, run_state) in &project_state.runs {
            // Only check runs that are supposedly active.
            if run_state.status != RunStatus::Running && run_state.status != RunStatus::Starting {
                continue;
            }

            let any_alive = run_state
                .nodes
                .values()
                .any(|ns| ns.pid.is_some_and(process::is_alive));

            if !any_alive {
                dead_run_names.push(run_name.clone());
            }
        }

        for run_name in &dead_run_names {
            tracing::info!(run_name, "cleaning up dead run (all processes exited)");

            if let Some(run_state) = project_state.runs.get(run_name) {
                // Kill any stragglers and clean up routes.
                for ns in run_state.nodes.values() {
                    if let Some(pid) = ns.pid {
                        if process::is_alive(pid) {
                            let _ = process::kill_process(pid).await;
                        }
                    }
                    if let Some(ref url_str) = ns.url {
                        let hostname = url_str.strip_prefix("https://").unwrap_or(url_str);
                        let hostname = hostname.split(':').next().unwrap_or(hostname);
                        let _ = self.helper_client.remove_host(hostname).await;
                        let route_id = format!("veld-{}-{}-{}", run_name, ns.node_name, ns.variant);
                        let _ = self.helper_client.remove_route(&route_id).await;
                    }
                }
            }
        }

        // Persist the cleanup.
        if !dead_run_names.is_empty() {
            let mut project_state = project_state;
            for run_name in &dead_run_names {
                project_state.runs.remove(run_name);
                self.remove_from_registry(run_name);
            }
            let _ = project_state.save(&self.project_root);
        }
    }

    /// Run the `on_stop` hook for a node if one is defined in the config.
    async fn run_on_stop_hook(&self, run_name: &str, node_state: &NodeState) {
        let variant_cfg = match self
            .config
            .nodes
            .get(&node_state.node_name)
            .and_then(|n| n.variants.get(&node_state.variant))
        {
            Some(cfg) => cfg,
            None => return,
        };

        let on_stop_cmd = match variant_cfg.on_stop.as_deref() {
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
        ctx.set_builtin("run", run_name.to_owned());
        ctx.set_builtin("root", self.project_root.to_string_lossy().into_owned());
        ctx.set_builtin("project", self.config.name.clone());
        ctx.set_builtin(
            "worktree",
            url::slugify(
                self.project_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("default"),
            ),
        );
        ctx.set_builtin(
            "branch",
            url::slugify(&detect_git_branch(&self.project_root)),
        );
        ctx.set_builtin("username", whoami_username());

        for (k, v) in &node_state.outputs {
            ctx.set_builtin(k, v.clone());
            ctx.set_node_output(&format!("nodes.{}.{k}", node_state.node_name), v.clone());
        }
        if let Some(port) = node_state.port {
            ctx.set_builtin("port", port.to_string());
        }

        let resolved_cmd = match crate::variables::interpolate(on_stop_cmd, &ctx) {
            Ok(cmd) => cmd,
            Err(e) => {
                tracing::warn!(
                    node = node_state.node_name,
                    error = %e,
                    "failed to resolve on_stop command variables"
                );
                return;
            }
        };

        // Build env from the variant config.
        let env = match build_env(variant_cfg.env.as_ref(), &ctx) {
            Ok(env) => env,
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
        let node_cfg_opt = self.config.nodes.get(&node_state.node_name);
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

        match process::run_command(&resolved_cmd, &working_dir, &env).await {
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

    /// Remove a run from the global registry.
    fn remove_from_registry(&self, run_name: &str) {
        if let Ok(mut registry) = GlobalRegistry::load() {
            let key = self.project_root.to_string_lossy().into_owned();
            if let Some(entry) = registry.projects.get_mut(&key) {
                entry.runs.remove(run_name);
                if entry.runs.is_empty() {
                    registry.projects.remove(&key);
                }
                let _ = registry.save();
            }
        }
    }

    // -----------------------------------------------------------------------
    // State persistence
    // -----------------------------------------------------------------------

    fn save_state(&self, run: &RunState) -> Result<(), OrchestratorError> {
        let mut project_state = ProjectState::load(&self.project_root)?;
        project_state.runs.insert(run.name.clone(), run.clone());
        project_state.save(&self.project_root)?;

        // Update global registry.
        if let Ok(mut registry) = GlobalRegistry::load() {
            let mut urls = HashMap::new();
            for ns in run.nodes.values() {
                if let Some(ref u) = ns.url {
                    urls.insert(RunState::node_key(&ns.node_name, &ns.variant), u.clone());
                }
            }

            let entry = registry
                .projects
                .entry(self.project_root.to_string_lossy().into_owned())
                .or_insert_with(|| RegistryEntry {
                    project_root: self.project_root.clone(),
                    project_name: self.config.name.clone(),
                    runs: HashMap::new(),
                });

            entry.runs.insert(
                run.name.clone(),
                RegistryRunInfo {
                    run_id: run.run_id,
                    name: run.name.clone(),
                    status: run.status.clone(),
                    urls,
                },
            );

            let _ = registry.save();
        }

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

/// Build a health-check attempt notifier that sends progress events.
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
            let _ = tx.send(ProgressEvent::HealthCheckAttempt {
                node: node.clone(),
                variant: variant.clone(),
                phase,
                attempt,
            });
        }
    })
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

    let variant_cfg = &ctx.config.nodes[&sel.node].variants[&sel.variant];
    let sensitive_outputs = variant_cfg.sensitive_outputs.clone();
    let mut node_state = NodeState::new(&sel.node, &sel.variant);
    node_state.status = NodeStatus::Starting;

    // Build variable context.
    let mut var_ctx = VariableContext::new();
    var_ctx.set_builtin("run", ctx.run_name.clone());
    var_ctx.set_builtin("run_id", ctx.run_id.to_string());
    var_ctx.set_builtin("root", ctx.project_root.to_string_lossy().into_owned());
    var_ctx.set_builtin("project", ctx.config.name.clone());
    var_ctx.set_builtin("worktree", url::slugify(&ctx.worktree));
    var_ctx.set_builtin("branch", url::slugify(&ctx.branch));
    var_ctx.set_builtin("username", ctx.username.clone());

    // Populate node output references from already-executed nodes.
    for (node_key, outputs) in ctx.all_outputs.as_ref() {
        for (field, value) in outputs {
            var_ctx.set_node_output(&format!("nodes.{node_key}.{field}"), value.clone());
        }
    }

    let server_handle = match variant_cfg.step_type {
        StepType::StartServer => {
            Some(execute_start_server_isolated(&ctx, &sel, &mut var_ctx, &mut node_state, precomputed).await?)
        }
        StepType::Command => {
            execute_command_isolated(&ctx, &sel, &mut var_ctx, &mut node_state).await?;
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
    var_ctx: &mut VariableContext,
    node_state: &mut NodeState,
    precomputed: Option<PrecomputedServer>,
) -> Result<process::ServerHandle, OrchestratorError> {
    let variant_cfg = &ctx.config.nodes[&sel.node].variants[&sel.variant];
    let node_cfg = &ctx.config.nodes[&sel.node];

    let mut precomputed = precomputed
        .expect("precomputed server info missing for start_server node");
    let port = precomputed.port;
    let node_url = precomputed.hostname.clone();
    let https_url = precomputed.https_url.clone();
    let port_reservation = precomputed
        .reservation
        .take()
        .expect("port reservation already consumed — node executed twice?");

    node_state.port = Some(port);
    var_ctx.set_builtin("port", port.to_string());
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
        "route_id": format!("veld-{}-{}-{}", ctx.run_name, sel.node, sel.variant),
        "hostname": &node_url,
        "upstream": format!("localhost:{port}"),
    });
    // Resolve per-node feature flags (variant > node > project > default).
    let features = config::resolve_features(
        ctx.config.features.as_ref(),
        node_cfg.features.as_ref(),
        variant_cfg.features.as_ref(),
    );

    // Include feedback/injection config so Caddy routes /__veld__/* to the
    // daemon and selectively injects scripts into HTML responses.
    if features.feedback_overlay || features.client_logs {
        route["feedback_upstream"] = serde_json::json!("localhost:19899");
        route["run_name"] = serde_json::json!(&ctx.run_name);
        route["project_root"] = serde_json::json!(ctx.project_root.to_string_lossy());
    }

    route["inject_feedback_overlay"] = serde_json::json!(features.feedback_overlay);
    route["inject_client_logs"] = serde_json::json!(features.client_logs);

    // Resolve client log levels (variant > node > project > default).
    let client_log_levels = config::resolve_client_log_levels(
        ctx.config.client_log_levels.as_deref(),
        node_cfg.client_log_levels.as_deref(),
        variant_cfg.client_log_levels.as_deref(),
    );
    route["client_log_levels"] = serde_json::json!(client_log_levels.join(","));
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
    let command = variant_cfg.command.as_deref().unwrap_or_default();
    let resolved_cmd = crate::variables::interpolate(command, var_ctx)?;
    debug_log_free(
        &ctx.debug_writer,
        &format!(
            "{}:{} — resolved command: {} (cwd: {})",
            sel.node,
            sel.variant,
            resolved_cmd,
            working_dir.display()
        ),
    )
    .await;

    // Build env.
    let mut env = build_env(variant_cfg.env.as_ref(), var_ctx)?;
    env.insert("VELD_PORT".to_owned(), port.to_string());
    env.insert("VELD_URL".to_owned(), https_url.clone());

    // Resolve synthetic outputs.
    if let Some(Outputs::Synthetic(ref map)) = variant_cfg.outputs {
        for (okey, tmpl) in map {
            let val = crate::variables::interpolate(tmpl, var_ctx)?;
            node_state.outputs.insert(okey.clone(), val);
        }
    }

    // Start the process.
    let log_path = logging::log_file(&ctx.project_root, &ctx.run_name, &sel.node, &sel.variant);

    // Release the port reservation immediately before spawning.
    port_reservation.release();

    let handle = process::start_server(
        &resolved_cmd,
        &working_dir,
        &env,
        &log_path,
        ctx.foreground,
    )
    .await?;
    let pid = handle.pid();
    node_state.pid = Some(pid);

    // Checkpoint: persist the PID immediately so Ctrl+C during health
    // checks still allows `veld stop` to find and kill this process.
    {
        let key = RunState::node_key(&sel.node, &sel.variant);
        // Lock briefly for in-memory update only (no .await = cancellation-safe).
        let (run_snapshot, project_root) = {
            let mut checkpoint = ctx.checkpoint.lock().expect("checkpoint mutex poisoned");
            checkpoint.run.execution_order.push(key.clone());
            checkpoint.run.nodes.insert(key, node_state.clone());
            (checkpoint.run.clone(), checkpoint.project_root.clone())
        };
        // File I/O outside the lock to avoid blocking the tokio runtime.
        let mut project_state = ProjectState::load(&project_root)
            .unwrap_or_else(|_| ProjectState::default());
        project_state
            .runs
            .insert(run_snapshot.name.clone(), run_snapshot);
        let _ = project_state.save(&project_root);
    }

    // Health check — inlined to emit progress events between phases.
    debug_log_free(
        &ctx.debug_writer,
        &format!(
            "{}:{} — process started (pid {}), beginning health checks",
            sel.node, sel.variant, pid
        ),
    )
    .await;
    if let Some(ref hc) = variant_cfg.health_check {
        node_state.status = NodeStatus::HealthChecking;
        node_state.health_phases.push(HealthCheckPhase {
            phase: 1,
            passed: false,
            last_error: None,
            passed_at: None,
        });
        node_state.health_phases.push(HealthCheckPhase {
            phase: 2,
            passed: false,
            last_error: None,
            passed_at: None,
        });

        // Build attempt notifiers for health check phases.
        let phase1_notifier = make_attempt_notifier(&ctx.progress_tx, &sel.node, &sel.variant, 1);
        let phase2_notifier = make_attempt_notifier(&ctx.progress_tx, &sel.node, &sel.variant, 2);

        // Phase 1: TCP port check.
        emit_progress(
            &ctx.progress_tx,
            ProgressEvent::HealthCheckPhase {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                phase: 1,
                description: format!("waiting for port {port}"),
            },
        );

        let phase1_result = tokio::select! {
            result = health::wait_for_port(port, hc, Some(&phase1_notifier)) => result,
            _ = wait_for_process_exit(pid) => {
                Err(health::HealthError::PortCheckFailed(
                    "server process exited before binding to port".into(),
                ))
            }
        };

        if let Err(e) = phase1_result {
            let msg = format!("process did not bind to port {port}: {e}");
            node_state.status = NodeStatus::Failed;
            node_state.health_phases[0].last_error = Some(msg.clone());
            debug_log_free(
                &ctx.debug_writer,
                &format!(
                    "{}:{} — health check phase 1 FAILED: {}",
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
        node_state.health_phases[0].passed = true;
        node_state.health_phases[0].passed_at = Some(now);
        emit_progress(
            &ctx.progress_tx,
            ProgressEvent::HealthCheckPassed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                phase: 1,
            },
        );
        debug_log_free(
            &ctx.debug_writer,
            &format!(
                "{}:{} — phase 1 passed (port open)",
                sel.node, sel.variant
            ),
        )
        .await;

        // Phase 2: depends on check type.
        let phase2_desc = match hc.check_type.as_str() {
            "http" => format!("HTTP check on port {port}"),
            "command" | "bash" => "command health check".to_owned(),
            "port" => "port-only (no phase 2)".to_owned(),
            other => format!("unknown check type: {other}"),
        };
        emit_progress(
            &ctx.progress_tx,
            ProgressEvent::HealthCheckPhase {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                phase: 2,
                description: phase2_desc,
            },
        );

        let phase2_future = async {
            match hc.check_type.as_str() {
                "http" => {
                    let direct_url = format!("http://127.0.0.1:{port}");
                    health::wait_for_http(&direct_url, hc, Some(&phase2_notifier)).await
                }
                "command" | "bash" => {
                    if let Some(cmd) = &hc.command {
                        health::wait_for_command_check(
                            cmd,
                            &working_dir,
                            hc,
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
                    "server process exited during health check".into(),
                ))
            }
        };

        match phase2_result {
            Ok(()) => {
                let now = chrono::Utc::now();
                node_state.health_phases[1].passed = true;
                node_state.health_phases[1].passed_at = Some(now);
                node_state.status = NodeStatus::Healthy;
                emit_progress(
                    &ctx.progress_tx,
                    ProgressEvent::HealthCheckPassed {
                        node: sel.node.clone(),
                        variant: sel.variant.clone(),
                        phase: 2,
                    },
                );
                debug_log_free(
                    &ctx.debug_writer,
                    &format!(
                        "{}:{} — health check passed, node is healthy",
                        sel.node, sel.variant
                    ),
                )
                .await;
            }
            Err(e) => {
                node_state.status = NodeStatus::Failed;
                let msg = e.to_string();
                node_state.health_phases[1].last_error = Some(msg.clone());
                debug_log_free(
                    &ctx.debug_writer,
                    &format!(
                        "{}:{} — health check phase 2 FAILED: {}",
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
    var_ctx: &mut VariableContext,
    node_state: &mut NodeState,
) -> Result<(), OrchestratorError> {
    let variant_cfg = &ctx.config.nodes[&sel.node].variants[&sel.variant];
    let node_cfg = &ctx.config.nodes[&sel.node];

    // Resolve working directory (variant > node > project root).
    let working_dir = resolve_working_dir(
        variant_cfg.cwd.as_deref(),
        node_cfg.cwd.as_deref(),
        &ctx.project_root,
        var_ctx,
    )?;

    // Resolve command or script.
    let raw_cmd = if let Some(ref script) = variant_cfg.script {
        format!("sh {}", ctx.project_root.join(script).display())
    } else {
        variant_cfg.command.clone().unwrap_or_default()
    };
    let resolved_cmd = crate::variables::interpolate(&raw_cmd, var_ctx)?;

    let env = build_env(variant_cfg.env.as_ref(), var_ctx)?;

    // Verify step (idempotency).
    if let Some(ref verify_cmd) = variant_cfg.verify {
        let verify_resolved = crate::variables::interpolate(verify_cmd, var_ctx)?;
        let verify_result = process::run_command(&verify_resolved, &working_dir, &env).await;
        if let Ok(ref out) = verify_result {
            if out.exit_code == 0 {
                tracing::info!(
                    node = sel.node,
                    variant = sel.variant,
                    "verify passed — skipping command step"
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
    let result = process::run_command(&resolved_cmd, &working_dir, &env).await?;

    node_state
        .outputs
        .insert("exit_code".to_owned(), result.exit_code.to_string());

    // Filter outputs against declared keys.
    let declared_keys = variant_cfg
        .outputs
        .as_ref()
        .map(|o| o.declared_keys())
        .unwrap_or_default();

    for (k, v) in &result.outputs {
        if declared_keys.contains(k.as_str()) {
            node_state.outputs.insert(k.clone(), v.clone());
        } else if variant_cfg.strict_outputs {
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

    if result.exit_code == 0 {
        node_state.status = NodeStatus::Healthy;
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
/// Used to race health checks against premature process death so the
/// orchestrator can fail fast instead of waiting for the full timeout.
async fn wait_for_process_exit(pid: u32) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if !process::is_alive(pid) {
            return;
        }
    }
}

/// Build the environment map, resolving variable references in values.
fn build_env(
    env_config: Option<&HashMap<String, String>>,
    ctx: &VariableContext,
) -> Result<HashMap<String, String>, crate::variables::VariableError> {
    let mut env = HashMap::new();
    if let Some(map) = env_config {
        for (key, tmpl) in map {
            let val = crate::variables::interpolate(tmpl, ctx)?;
            env.insert(key.clone(), val);
        }
    }
    Ok(env)
}

/// Detect the current git branch, or return empty string.
fn detect_git_branch(project_root: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_owned())
            } else {
                None
            }
        })
        .unwrap_or_default()
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
