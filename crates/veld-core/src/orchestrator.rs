#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;
use tracing;

use crate::config::{self, Outputs, StepType, VeldConfig};
use crate::graph::{self, NodeSelection};
use crate::health;
use crate::helper::HelperClient;
use crate::logging::{self, LogWriter};
use crate::port::PortAllocator;
use crate::process;
use crate::state::{
    GlobalRegistry, HealthCheckPhase, NodeState, NodeStatus, ProjectState, RegistryEntry,
    RegistryRunInfo, RunState, RunStatus,
};
use crate::url;
use crate::variables::VariableContext;

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

pub struct Orchestrator {
    pub config: VeldConfig,
    pub config_path: PathBuf,
    pub project_root: PathBuf,
    pub port_allocator: PortAllocator,
    pub helper_client: HelperClient,
    /// Active child processes keyed by `"node:variant"`.
    children: HashMap<String, process::ServerHandle>,
    /// Debug mode — writes orchestration trace to `veld-debug.log`.
    debug: bool,
    /// Debug log writer (created on demand when debug is true).
    debug_writer: Option<LogWriter>,
    /// Foreground mode — pipes stdout/stderr through timestamping tasks.
    /// When false (detached), redirects directly to file so processes survive CLI exit.
    foreground: bool,
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
            children: HashMap::new(),
            debug: false,
            debug_writer: None,
            foreground: false,
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
        // Clean up any stale run with the same name (kills processes, removes
        // DNS/Caddy routes, clears state). This handles the case where a
        // previous run was not properly cleaned up or the user reuses a name.
        self.cleanup_stale_run(run_name).await;

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

        // Execute stages in order.
        for stage in &plan {
            let results = self
                .execute_stage(
                    stage,
                    &run,
                    &branch,
                    &worktree,
                    &username,
                    &hostname,
                    &mut all_outputs,
                )
                .await?;

            for (key, node_state) in results {
                run.execution_order.push(key.clone());
                run.nodes.insert(key, node_state);
            }
        }

        run.status = RunStatus::Running;

        // Persist state.
        self.save_state(&run)?;

        Ok(run)
    }

    /// Execute a single stage (parallel nodes).
    async fn execute_stage(
        &mut self,
        stage: &[NodeSelection],
        run: &RunState,
        branch: &str,
        worktree: &str,
        username: &str,
        hostname: &str,
        all_outputs: &mut HashMap<String, HashMap<String, String>>,
    ) -> Result<Vec<(String, NodeState)>, OrchestratorError> {
        let mut results = Vec::new();

        // Nodes within a stage are independent. We run them sequentially here;
        // the CLI layer can wrap in tokio::spawn for true parallelism.
        for sel in stage {
            let key = RunState::node_key(&sel.node, &sel.variant);
            let node_state = self
                .execute_node(sel, run, branch, worktree, username, hostname, all_outputs)
                .await?;

            // Store this node's outputs for downstream resolution.
            let mut node_out = HashMap::new();
            for (k, v) in &node_state.outputs {
                node_out.insert(k.clone(), v.clone());
            }
            if let Some(port) = node_state.port {
                node_out.insert("port".to_owned(), port.to_string());
            }
            if let Some(ref u) = node_state.url {
                node_out.insert("url".to_owned(), u.clone());
            }
            // Store under both qualified and unqualified keys.
            all_outputs.insert(format!("{}:{}", sel.node, sel.variant), node_out.clone());
            all_outputs
                .entry(sel.node.clone())
                .or_default()
                .extend(node_out);

            results.push((key, node_state));
        }

        Ok(results)
    }

    /// Execute a single node: allocate port, resolve variables, start process,
    /// run health checks.
    async fn execute_node(
        &mut self,
        sel: &NodeSelection,
        run: &RunState,
        branch: &str,
        worktree: &str,
        username: &str,
        hostname: &str,
        all_outputs: &HashMap<String, HashMap<String, String>>,
    ) -> Result<NodeState, OrchestratorError> {
        let variant_cfg = &self.config.nodes[&sel.node].variants[&sel.variant];
        let sensitive_outputs = variant_cfg.sensitive_outputs.clone();
        let mut node_state = NodeState::new(&sel.node, &sel.variant);
        node_state.status = NodeStatus::Starting;

        // Build variable context.
        let mut ctx = VariableContext::new();
        ctx.set_builtin("run", run.name.clone());
        ctx.set_builtin("run_id", run.run_id.to_string());
        ctx.set_builtin("root", self.project_root.to_string_lossy().into_owned());
        ctx.set_builtin("project", self.config.name.clone());
        ctx.set_builtin("worktree", url::slugify(worktree));
        ctx.set_builtin("branch", url::slugify(branch));
        ctx.set_builtin("username", username.to_owned());

        // Populate node output references from already-executed nodes.
        for (node_key, outputs) in all_outputs {
            for (field, value) in outputs {
                ctx.set_node_output(&format!("nodes.{node_key}.{field}"), value.clone());
            }
        }

        match variant_cfg.step_type {
            StepType::StartServer => {
                self.execute_start_server(
                    sel,
                    run,
                    branch,
                    worktree,
                    username,
                    hostname,
                    &mut ctx,
                    &mut node_state,
                )
                .await?;
            }
            StepType::Bash => {
                self.execute_bash(sel, &mut ctx, &mut node_state).await?;
            }
        }

        // Mark sensitive output keys so they are encrypted at rest and masked
        // in display. The list comes from the variant config.
        if let Some(sensitive) = sensitive_outputs {
            node_state.sensitive_keys = sensitive;
        }

        Ok(node_state)
    }

    /// Execute a `start_server` node.
    async fn execute_start_server(
        &mut self,
        sel: &NodeSelection,
        run: &RunState,
        branch: &str,
        worktree: &str,
        username: &str,
        hostname: &str,
        ctx: &mut VariableContext,
        node_state: &mut NodeState,
    ) -> Result<(), OrchestratorError> {
        let variant_cfg = &self.config.nodes[&sel.node].variants[&sel.variant];

        // Allocate port.
        let port = self.port_allocator.allocate()?;
        node_state.port = Some(port);
        ctx.set_builtin("port", port.to_string());
        self.debug_log(&format!(
            "{}:{} — allocated port {}",
            sel.node, sel.variant, port
        ))
        .await;

        // Build URL using the most specific template (variant > node > project).
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
            branch,
            worktree,
            username,
            hostname,
        );
        let node_url = url::evaluate_url_template(effective_template, &url_values)?;
        let https_url = format!("https://{node_url}");
        node_state.url = Some(https_url.clone());

        // Configure DNS + Caddy via helper (best-effort).
        self.debug_log(&format!(
            "{}:{} — adding DNS host {} → 127.0.0.1",
            sel.node, sel.variant, node_url
        ))
        .await;
        if let Err(e) = self.helper_client.add_host(&node_url, "127.0.0.1").await {
            tracing::warn!(error = %e, "failed to add DNS host via helper");
        }
        let route = serde_json::json!({
            "route_id": format!("veld-{}-{}-{}", run.name, sel.node, sel.variant),
            "hostname": &node_url,
            "upstream": format!("localhost:{port}"),
        });
        if let Err(e) = self.helper_client.add_route(route).await {
            tracing::warn!(error = %e, "failed to add Caddy route via helper");
        }

        // Resolve command.
        let command = variant_cfg.command.as_deref().unwrap_or_default();
        let resolved_cmd = crate::variables::interpolate(command, ctx)?;
        self.debug_log(&format!(
            "{}:{} — resolved command: {}",
            sel.node, sel.variant, resolved_cmd
        ))
        .await;

        // Build env.
        let mut env = build_env(variant_cfg.env.as_ref(), ctx)?;
        env.insert("VELD_PORT".to_owned(), port.to_string());
        env.insert("VELD_URL".to_owned(), https_url.clone());

        // Resolve synthetic outputs.
        if let Some(Outputs::Synthetic(ref map)) = variant_cfg.outputs {
            for (key, tmpl) in map {
                let val = crate::variables::interpolate(tmpl, ctx)?;
                node_state.outputs.insert(key.clone(), val);
            }
        }

        // Start the process. stdout/stderr are redirected to the log file at
        // the OS level so the process survives after the CLI exits.
        let log_path = logging::log_file(&self.project_root, &run.name, &sel.node, &sel.variant);

        let handle = process::start_server(
            &resolved_cmd,
            &self.project_root,
            &env,
            &log_path,
            self.foreground,
        )
        .await?;
        let pid = handle.pid();
        node_state.pid = Some(pid);

        self.children
            .insert(RunState::node_key(&sel.node, &sel.variant), handle);

        // Health check.
        self.debug_log(&format!(
            "{}:{} — process started (pid {}), beginning health checks",
            sel.node, sel.variant, pid
        ))
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

            match health::run_health_check(port, Some(&https_url), &self.project_root, hc).await {
                Ok(()) => {
                    let now = chrono::Utc::now();
                    for phase in &mut node_state.health_phases {
                        phase.passed = true;
                        phase.passed_at = Some(now);
                    }
                    node_state.status = NodeStatus::Healthy;
                    self.debug_log(&format!(
                        "{}:{} — health check passed, node is healthy",
                        sel.node, sel.variant
                    ))
                    .await;
                }
                Err(e) => {
                    node_state.status = NodeStatus::Failed;
                    let msg = e.to_string();
                    self.debug_log(&format!(
                        "{}:{} — health check FAILED: {}",
                        sel.node, sel.variant, msg
                    ))
                    .await;
                    if let Some(phase) = node_state.health_phases.last_mut() {
                        phase.last_error = Some(msg.clone());
                    }
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

        Ok(())
    }

    /// Execute a `bash` node.
    async fn execute_bash(
        &mut self,
        sel: &NodeSelection,
        ctx: &mut VariableContext,
        node_state: &mut NodeState,
    ) -> Result<(), OrchestratorError> {
        let variant_cfg = &self.config.nodes[&sel.node].variants[&sel.variant];

        // Resolve command or script.
        let raw_cmd = if let Some(ref script) = variant_cfg.script {
            format!("bash {}", self.project_root.join(script).display())
        } else {
            variant_cfg.command.clone().unwrap_or_default()
        };
        let resolved_cmd = crate::variables::interpolate(&raw_cmd, ctx)?;

        let env = build_env(variant_cfg.env.as_ref(), ctx)?;

        // Verify step (idempotency).
        if let Some(ref verify_cmd) = variant_cfg.verify {
            let verify_resolved = crate::variables::interpolate(verify_cmd, ctx)?;
            let verify_result = process::run_bash(&verify_resolved, &self.project_root, &env).await;
            if let Ok(ref out) = verify_result {
                if out.exit_code == 0 {
                    tracing::info!(
                        node = sel.node,
                        variant = sel.variant,
                        "verify passed — skipping bash step"
                    );
                    node_state.status = NodeStatus::Skipped;
                    node_state
                        .outputs
                        .insert("exit_code".to_owned(), "0".to_owned());
                    return Ok(());
                }
            }
        }

        // Run bash step.
        let result = process::run_bash(&resolved_cmd, &self.project_root, &env).await?;

        node_state
            .outputs
            .insert("exit_code".to_owned(), result.exit_code.to_string());
        for (k, v) in result.outputs {
            node_state.outputs.insert(k, v);
        }

        if result.exit_code == 0 {
            node_state.status = NodeStatus::Healthy;
        } else {
            node_state.status = NodeStatus::Failed;
            return Err(OrchestratorError::NodeFailed {
                node: sel.node.clone(),
                variant: sel.variant.clone(),
                reason: format!("bash step exited with code {}", result.exit_code),
            });
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Stop
    // -----------------------------------------------------------------------

    /// Stop a run in reverse dependency order. Returns whether the run was
    /// actually stopped or was already stopped.
    pub async fn stop(&mut self, run_name: &str) -> Result<StopResult, OrchestratorError> {
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

        match process::run_bash(&resolved_cmd, &self.project_root, &env).await {
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
// Helpers
// ---------------------------------------------------------------------------

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
