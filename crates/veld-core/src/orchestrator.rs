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

    #[error(
        "hostname {hostname} is already served by run '{run_name}' of {project_name} \
         ({project_root}) — two checkouts that share a project name and a run name mint \
         the same URL, and only one of them can be routed. Start this one under a \
         different name (`veld start --name <other>`), or stop the other run first \
         (`veld stop --name {run_name}` in {project_root}). If that run is actually \
         gone, `veld gc` clears the stale record."
    )]
    HostnameClaimed {
        hostname: String,
        run_name: String,
        project_name: String,
        project_root: String,
    },

    #[error(
        "hostname {hostname} is already served by run '{run_name}', started from \
         {project_root} — the same directory as this one, reached by a different path \
         (a symlink, or /tmp vs /private/tmp on macOS). Veld addresses runs by the path \
         you invoke it from, so it cannot replace that run from here: re-run from \
         {project_root}, or stop it there."
    )]
    HostnameClaimedByOtherSpelling {
        hostname: String,
        run_name: String,
        project_root: String,
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

/// Context values every URL template can interpolate, gathered once per start.
///
/// Gathered before anything is torn down, because the hostnames derived from it
/// gate the start (see [`claimed_hostname`]).
struct UrlContext {
    branch: String,
    worktree: String,
    username: String,
    hostname: String,
}

/// Every hostname a plan will serve, normalised the way route ids are.
///
/// Split out from `check_hostnames_unclaimed` so the selection rule is testable
/// without a registry: `claimed_hostname` (the decision) already had tests, but
/// *which nodes get offered to it* did not — and that is where this went wrong.
/// The step type must come from [`config::resolve_variant`], because `type` may be
/// declared once at node level (F3); reading it off the raw variant yields `None`
/// for every variant that inherits it, silently skipping the collision check for
/// exactly the configs using that feature.
fn planned_hostnames(
    config: &config::VeldConfig,
    plan: &[Vec<NodeSelection>],
    run_name: &str,
    ctx: &UrlContext,
) -> Result<Vec<String>, OrchestratorError> {
    let mut planned = Vec::new();
    for sel in plan.iter().flatten() {
        let node_cfg = &config.nodes[&sel.node];
        let variant_cfg = &node_cfg.variants[&sel.variant];
        let resolved = config::resolve_variant(config, node_cfg, variant_cfg);
        if resolved.step_type != config::StepType::StartServer {
            continue;
        }
        // Normalised the same way the route id is, so a template carrying a literal
        // port compares against the registry's stripped hostnames.
        let rendered = node_hostname(config, sel, run_name, ctx)?;
        planned.push(url::hostname_of_url(&rendered).to_owned());
    }
    Ok(planned)
}

/// The hostname a `start_server` node will be served at.
///
/// A free function taking `&VeldConfig` so both callers — the pre-start
/// collision check and the port/URL pre-compute pass — derive the hostname the
/// same way. If they diverged, a run could be checked against one hostname and
/// have its route registered under another.
fn node_hostname(
    config: &config::VeldConfig,
    sel: &NodeSelection,
    run_name: &str,
    ctx: &UrlContext,
) -> Result<String, OrchestratorError> {
    let node_cfg = &config.nodes[&sel.node];
    let variant_cfg = &node_cfg.variants[&sel.variant];
    let effective_template = url::resolve_url_template(
        &config.url_template,
        node_cfg.url_template.as_deref(),
        variant_cfg.url_template.as_deref(),
    );
    let url_values = url::build_url_template_values(
        &sel.node,
        &sel.variant,
        run_name,
        &config.name,
        &ctx.branch,
        &ctx.worktree,
        &ctx.username,
        &ctx.hostname,
    );
    Ok(url::evaluate_url_template(effective_template, &url_values)?)
}

/// Whether two registry roots resolve to the same directory while being spelled
/// differently — one checkout reached through a symlink, or `/tmp/x` versus
/// `/private/tmp/x` on macOS.
///
/// `db::state::root_key` stores the spelling the CLI was invoked with, so such a
/// checkout is two registry rows, and every lookup keyed by the path — including
/// `cleanup_stale_run`'s `get_run` — sees only its own spelling. That is why this
/// is *not* treated as "our own project" and skipped: the replace path cannot
/// reach the other row, so the two runs really would fight over one hostname.
/// [`claimed_hostname`] reports it with its own message instead.
///
/// Compared by device + inode, not by canonical path: `realpath` resolves
/// symlinks and `/tmp` → `/private/tmp`, but it does NOT fold case, so on a
/// case-insensitive volume (APFS's default) `~/Repo` and `~/repo` are one
/// directory with two different canonical strings. One `stat` each catches
/// symlinks, case and bind mounts alike.
///
/// Only ever called once a planned hostname has actually matched — it touches the
/// filesystem, and a registry row rooted on an unresponsive network mount would
/// otherwise stall every start.
#[cfg(unix)]
fn is_same_dir_other_spelling(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    if a == b {
        return false;
    }
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        // One of them isn't reachable, so they cannot be shown to be the same
        // directory. The caller still reports the conflict, just with the
        // different-project message.
        _ => false,
    }
}

#[cfg(not(unix))]
fn is_same_dir_other_spelling(a: &Path, b: &Path) -> bool {
    if a == b {
        return false;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// A hostname another project's run already serves.
struct HostnameClaim {
    hostname: String,
    run_name: String,
    project_name: String,
    project_root: String,
    /// The claimant is this very directory under a different path spelling, so
    /// no lookup keyed by our own spelling can see or replace it.
    same_dir: bool,
}

/// Find a planned hostname that a run in a *different* project already serves.
///
/// Route ids are keyed by hostname ([`url::run_route_id`]), so an identical
/// hostname is an identical route: whoever writes last wins and the other
/// project's URL silently stops resolving to its own app. Two checkouts of one
/// repo hit this — they share `config.name`, so
/// `{service}.{run}.{project}.localhost` is byte-identical whenever the run
/// names match too.
///
/// Scoped deliberately:
/// - Only *other* project roots. Reusing a name inside one project is the
///   documented replace path (`cleanup_stale_run`).
/// - Only `RunStatus::Running`. `Db::registry` carries each environment's latest
///   run whatever its status and keeps node URLs as history, so `is_live()`
///   would also match a `Starting` run's partial URLs and a `Stopping` run whose
///   ports are already going away.
/// - Compared case-insensitively, because DNS and Caddy host matching are.
///
/// Not a lock: two `veld start`s racing in two checkouts of one repo are both
/// `Starting`, so neither sees the other and both register the same route id.
/// That lands on the pre-#170 behaviour for that pair — last write wins, and the
/// first `veld stop` removes the route the other still needs. Closing it needs a
/// claim registered before the check, which is a bigger change than the window
/// justifies; a sequential second start is refused normally.
///
/// Returns the lowest-sorting conflict so the same state always reports the same
/// one — registry maps iterate in arbitrary order.
///
/// Two veld *instances* (separate databases, one shared helper) cannot see each
/// other's runs, so this never fires across them — see `instance.rs`.
fn claimed_hostname(
    registry: &crate::state::GlobalRegistry,
    own_root: &Path,
    planned: &[String],
) -> Option<HostnameClaim> {
    let mut conflicts: Vec<(String, String, String, String, bool)> = Vec::new();
    for entry in registry.projects.values() {
        if entry.project_root == own_root {
            continue;
        }
        for run in entry.runs.values() {
            if run.status != RunStatus::Running {
                continue;
            }
            for claimed in run.urls.values().map(|u| url::hostname_of_url(u)) {
                if let Some(ours) = planned.iter().find(|h| h.eq_ignore_ascii_case(claimed)) {
                    conflicts.push((
                        ours.clone(),
                        run.name.clone(),
                        entry.project_name.clone(),
                        entry.project_root.display().to_string(),
                        // Resolved only now that a hostname has actually matched:
                        // it stats the filesystem, and doing it per registry entry
                        // would make one stale root on an unresponsive mount stall
                        // every start.
                        is_same_dir_other_spelling(&entry.project_root, own_root),
                    ));
                }
            }
        }
    }
    conflicts.sort();
    conflicts.into_iter().next().map(
        |(hostname, run_name, project_name, project_root, same_dir)| HostnameClaim {
            hostname,
            run_name,
            project_name,
            project_root,
            same_dir,
        },
    )
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
    started_from: Option<crate::state::StartOrigin>,
    overrides: &crate::values::VarOverrides,
    provenance: &std::collections::BTreeMap<String, String>,
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
    // Every var the config declared overridable, with where its value came from.
    // Declared-but-unanswered vars are listed too (`from: "default"`), because
    // "this machine had no override for it" is exactly as much of an explanation
    // as "it did" when two runs disagree.
    let mut var_overrides: Vec<crate::state::VarOverrideSnapshot> = config
        .vars
        .iter()
        .flatten()
        .filter(|(_, decl)| decl.machine().is_some())
        .map(|(name, _)| crate::state::VarOverrideSnapshot {
            name: name.clone(),
            from: match provenance.get(name) {
                Some(from) if overrides.contains_key(name) => from.clone(),
                _ => "default".to_owned(),
            },
        })
        .collect();
    // `config.vars` is a HashMap, so without this the list's order would change
    // between runs and `veld runs diff` would report a difference that is only
    // hash iteration order.
    var_overrides.sort_by(|a, b| a.name.cmp(&b.name));
    crate::state::GraphSnapshot {
        config_hash,
        started_from,
        nodes,
        var_overrides,
    }
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

/// The `${veld.url}` family, derived from the node's own HTTPS URL.
///
/// One owner for the derivation, because there were three callers and only one
/// of them was complete: the `on_stop` context set `port` and stopped there, so a
/// teardown hook naming its container after `${veld.url.hostname}` — the obvious
/// way to stop the name in `argv` and the name in `on_stop` from drifting —
/// failed to interpolate, and an `on_stop` that fails to interpolate is a leaked
/// container.
///
/// The URL is the input rather than `(hostname, https_port)` because that is all
/// the stop path has: `NodeState` persists the URL, not the pieces.
///
/// That is exact for every hostname veld generates, since `node_hostname`
/// slugifies to `[a-z0-9-]` and cannot produce a colon. It is deliberately
/// **not** identical for one input the old two-place construction handled badly:
/// a `url_template` carrying a literal port (`"{service}.localhost:3000"`) at
/// `https_port == 443` used to yield `url.hostname = "svc.localhost:3000"` and
/// `url.port = "443"` — the port in the wrong field, twice. Splitting the URL
/// gives `"svc.localhost"` and `"3000"`. A config that read the old values sees
/// different ones; they were wrong.
pub fn url_builtins(https_url: &str) -> Vec<(&'static str, String)> {
    let rest = https_url.strip_prefix("https://").unwrap_or(https_url);
    let host = rest.split('/').next().unwrap_or(rest);
    let (hostname, port) = match host.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, p.to_owned()),
        _ => (host, "443".to_owned()),
    };
    vec![
        ("url", https_url.to_owned()),
        ("url.hostname", hostname.to_owned()),
        ("url.host", host.to_owned()),
        ("url.origin", https_url.to_owned()),
        ("url.scheme", "https".to_owned()),
        ("url.port", port),
    ]
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
    /// What this project is called for override purposes — one value across every
    /// worktree of the repo. See [`crate::project_id`].
    project_id: crate::project_id::ProjectId,
    /// This machine's answers for machine-overridable vars, read once at
    /// construction. Empty is a legal state: it means every var falls back to its
    /// declared `default`.
    var_overrides: crate::values::VarOverrides,
    /// Where each answer above came from, kept beside the values rather than
    /// inside them so resolution stays a plain name→value lookup while the run
    /// snapshot can still record provenance. `"project"` / `"worktree"` for a
    /// stored row, `"flag"` for a `--var` answer that no row backs.
    var_provenance: std::collections::BTreeMap<String, String>,
    /// Project `vars`, resolved once during `start`. Stashed so `run_terminal` and
    /// the `on_stop` path reuse the same values rather than re-running a source
    /// command — two resolutions of a rotating credential would disagree.
    resolved_vars: Option<Arc<HashMap<String, String>>>,
    /// Which run `resolved_vars` was resolved *for*.
    ///
    /// One `Orchestrator` does not always mean one run: `veld stop --all` builds a
    /// single instance and loops `stop()` over every run name. A var may read
    /// `${veld.run}` or `${veld.run_id}`, so reusing the first run's map for the
    /// second would have its `teardown` remove the first run's container — the
    /// precise "cleans up the wrong thing" failure the teardown path exists to
    /// avoid. The cache is therefore keyed, not just present.
    resolved_vars_run: Option<String>,
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
        // Read once, here, so every path through this orchestrator sees the same
        // answers — including `stop`, which builds a fresh instance in a
        // different process. Teardown therefore reads the store and never asks:
        // a `veld stop` that prompted, or that failed because a var had no
        // answer, would leak the containers it exists to remove.
        let project_id = crate::project_id::project_id_for(&project_root);
        let stored = db
            .effective_var_overrides(&project_id, &project_root)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "could not read machine var overrides; using config defaults");
                Default::default()
            });
        let var_provenance = stored
            .iter()
            .map(|(k, v)| (k.clone(), v.scope.to_string()))
            .collect();
        let var_overrides: crate::values::VarOverrides =
            stored.into_iter().map(|(k, v)| (k, v.value)).collect();
        Ok(Self {
            config,
            config_path,
            config_hash,
            project_root,
            project_id,
            var_overrides,
            var_provenance,
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
            resolved_vars_run: None,
            terminal_outputs: None,
        })
    }

    /// What this project is called for machine-override purposes.
    pub fn project_id(&self) -> &crate::project_id::ProjectId {
        &self.project_id
    }

    /// Supply per-run var answers (`veld start --var NAME=VALUE`).
    ///
    /// These sit **above** the stored machine answers and are never written
    /// anywhere: a value that is right for one run is not an answer about the
    /// machine, and the whole point of the store is that what is in it was
    /// chosen deliberately.
    pub fn set_var_answers(&mut self, answers: crate::values::VarOverrides) {
        for (name, value) in answers {
            // Its own provenance, not `default` and not a scope. "default" would
            // claim the config supplied a value it did not, and a scope would
            // imply a stored row that does not exist — and a `--var` answer is
            // the most volatile difference two runs of one commit can have, so
            // labelling it as either is exactly the case the snapshot exists to
            // distinguish.
            self.var_provenance.insert(name.clone(), "flag".to_owned());
            self.var_overrides.insert(name, value);
        }
    }

    /// Which machine-overridable vars this start would need and this machine has
    /// not answered — checked **before anything spawns**.
    ///
    /// The selections are expanded through `build_execution_plan` exactly as
    /// `start` expands them, so a var reached only through a transitive
    /// dependency is found here rather than after three nodes are already up.
    /// Using the raw selections instead would leave precisely the case this
    /// pre-flight exists to prevent.
    pub fn unanswered_vars(
        &self,
        selections: &[NodeSelection],
    ) -> Result<Vec<crate::values::UnansweredVar>, OrchestratorError> {
        let resolved = graph::resolve_selections(selections, &self.config)?;
        let plan = graph::build_execution_plan(&resolved, &self.config)?;
        let planned: Vec<NodeSelection> = plan.into_iter().flatten().collect();
        Ok(crate::values::unanswered_machine_vars(
            &self.config,
            &self.var_overrides,
            &planned,
        ))
    }

    /// `--var` answers that a later `veld stop` will need and cannot get.
    ///
    /// A flag answer lives only in this process. `veld stop` runs in a *new* one,
    /// rebuilds the orchestrator, and reads the store — so a var answered only by
    /// `--var` and referenced from `teardown` or `on_stop` is unresolvable at
    /// stop time. That is not a small loss: one unresolvable var makes
    /// [`Self::ensure_stop_vars`] warn and skip **every** `${vars.*}` teardown
    /// step, so a container the teardown exists to remove is left running.
    ///
    /// Named at start, where the user can still choose `veld config set` instead,
    /// rather than discovered at stop when the environment is already up.
    pub fn flag_answers_needed_at_teardown(&self, selections: &[NodeSelection]) -> Vec<String> {
        let needed = config::vars_for_teardown(&self.config, selections);
        let mut out: Vec<String> = self
            .var_provenance
            .iter()
            .filter(|(name, from)| from.as_str() == "flag" && needed.contains(name.as_str()))
            // A var with a default is fine: stop falls back to it.
            .filter(|(name, _)| {
                self.config
                    .vars
                    .as_ref()
                    .and_then(|v| v.get(name.as_str()))
                    .and_then(|d| d.machine())
                    .is_some_and(|m| m.default.is_none())
            })
            .map(|(name, _)| name.clone())
            .collect();
        out.sort();
        out
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
    /// `origin` is what the caller was asked for — a preset name plus the
    /// expansion it produced (see [`crate::state::StartOrigin`]). It is recorded
    /// verbatim: the orchestrator never re-reads the preset and never validates
    /// it against the config, because a preset can be edited while the run is
    /// live and the point of the record is to make that detectable later.
    ///
    /// `None` means "not known", and is not the same as an empty origin. `veld
    /// restart` passes the previous run's origin through unchanged — including its
    /// absence, for a run recorded before provenance existed — rather than
    /// rebuilding one from node rows, which are the dependency closure and would
    /// record a wider selection set than anyone asked for.
    pub async fn start(
        &mut self,
        selections: &[NodeSelection],
        run_name: &str,
        origin: Option<crate::state::StartOrigin>,
    ) -> Result<RunState, OrchestratorError> {
        // Semantic validation runs HERE, not in `parse_config`: a typo must not
        // strand `veld stop` against a running environment (its `on_stop` hooks
        // are read from the on-disk config at stop time). `veld lint` runs the
        // same rules and additionally reports warnings.
        if let Some(msg) = config::error_summary(&config::validate(&self.config)) {
            return Err(OrchestratorError::ConfigInvalid(msg));
        }

        // Resolve the graph and the URL context up front, because the hostnames
        // this run wants must be checked against other projects BEFORE anything
        // irreversible happens (#170). Everything below this point either
        // destroys state or has side effects the user can see: replacing a live
        // same-named run kills it, unshares it and — since route ids are keyed by
        // hostname — removes the very route the other project is serving; project
        // `setup` steps run real commands; Caddy is started. Refusing after any of
        // that would leave the user worse off than before they ran the command.
        //
        // Two consequences worth naming. An invalid selection now fails before
        // the dead-run cleanup below rather than after it — cleanup is idempotent
        // and the next start does it anyway — and before the internal log writer
        // exists, so a refusal is reported to the caller (and, for a
        // daemon-spawned start, logged from its stderr) rather than to
        // `veld logs --source internal`.
        //
        // And `url_ctx` is now sampled BEFORE `setup` steps run, where it used to
        // be sampled after. A setup step that switches branch therefore no longer
        // changes `{branch}` in this run's hostnames. That is the point: the
        // hostname checked here has to be the hostname the route is registered
        // under, and a URL that depends on a side effect of its own setup step
        // could not be checked at all.
        let resolved = graph::resolve_selections(selections, &self.config)?;
        let plan = graph::build_execution_plan(&resolved, &self.config)?;
        let url_ctx = UrlContext {
            branch: url::detect_git_branch(&self.project_root),
            worktree: self
                .project_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default")
                .to_owned(),
            username: whoami_username(),
            hostname: whoami_hostname(),
        };
        self.check_hostnames_unclaimed(&plan, run_name, &url_ctx)?;

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
            origin,
            &self.var_overrides,
            &self.var_provenance,
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

        // Project `vars` are resolved in two passes with one cache: the ones a
        // `setup` step reaches for, before setup runs, and the rest below, before
        // the first node spawns. A var is still resolved at most once per run —
        // two resolutions of a rotating credential would disagree — but now only
        // if something actually reaches for it.
        //
        // Setup could not see `${vars.*}` at all before this: `project_step_context`
        // read `resolved_vars`, which was still empty this early, so a var
        // referenced in a setup step failed with "no var named …" while being
        // declared three lines up in the same file.
        let vars_ctx = self.vars_context(
            run_name,
            Some(run.run_id),
            &url_ctx.branch,
            &url_ctx.worktree,
            &url_ctx.username,
        );

        // Run project-level setup steps before the graph executes. A setup
        // failure happens before the run is persisted, so record it as
        // `failed` history directly — this run never held the live slot.
        let setup_result = match crate::values::resolve_vars(
            self.config.vars.as_ref(),
            &self.var_overrides,
            Some(&self.project_root),
            &vars_ctx,
            &config::vars_for_setup(&self.config),
            &HashMap::new(),
        )
        .await
        {
            Ok(vars) => {
                self.resolved_vars = Some(Arc::new(vars));
                self.resolved_vars_run = Some(run_name.to_owned());
                self.run_setup_steps(run_name, Some(run.run_id)).await
            }
            Err(e) => Err(e.into()),
        };
        if let Err(e) = setup_result {
            run.status = RunStatus::Failed;
            run.end_reason = Some(EndReason::Failed);
            run.end_detail = Some(end_detail_for_error(&e));
            run.ended_at = Some(chrono::Utc::now());
            let _ = self.save_state(&run);
            return Err(e);
        }

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

                // Same function the pre-start hostname check used, so the URL a
                // route is registered under cannot drift from the one that was
                // checked against other projects.
                let node_url = node_hostname(&self.config, sel, &run.name, &url_ctx)?;
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
                // `url` plus the individual location pieces (mirrors the Web URL
                // API), from the same derivation the node's own `${veld.url*}`
                // and its `on_stop` use.
                for (key, value) in url_builtins(&https_url) {
                    node_out.insert(key.to_owned(), value);
                }
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

        // Resolve the rest of the plan's `vars` before anything spawns: a var may
        // be backed by a file or a command, and a broken source must fail the
        // start rather than surface as an empty value inside a service. Only the
        // plan's — a credential helper behind a var no selected node mentions is
        // not woken up, matching what a node-level `env` source already did.
        //
        // `plan`, not `resolved`: `resolved` is the *endpoints*, and a node pulled
        // in only by `depends_on` interpolates its own `env` like any other. Asking
        // the endpoints would leave a var that only a dependency uses unresolved,
        // and the node would fail at spawn with "no var named …" for a var that is
        // declared right there in the config.
        let planned: Vec<NodeSelection> = plan.iter().flatten().cloned().collect();
        let mut all_vars: HashMap<String, String> =
            self.resolved_vars.as_deref().cloned().unwrap_or_default();
        all_vars.extend(
            crate::values::resolve_vars(
                self.config.vars.as_ref(),
                &self.var_overrides,
                Some(&self.project_root),
                &vars_ctx,
                &config::vars_for_plan(&self.config, &planned),
                &all_vars,
            )
            .await?,
        );
        let shared_vars = Arc::new(all_vars);
        self.resolved_vars = Some(Arc::clone(&shared_vars));
        self.resolved_vars_run = Some(run_name.to_owned());

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
                        &url_ctx.branch,
                        &url_ctx.worktree,
                        &url_ctx.username,
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
            // No sink: a probe's output is a predicate, not the node's output.
            let probe = process::run_command(&skip_if_resolved, &working_dir, &env, None, None)
                .await
                .inspect_err(|e| {
                    tracing::warn!(
                        node = sel.node,
                        variant = sel.variant,
                        error = %e,
                        "skip_if probe could not run — treating the node as not skippable"
                    );
                })
                .inspect(|out| {
                    if probe_could_not_run(out.exit_code) {
                        tracing::warn!(
                            node = sel.node,
                            variant = sel.variant,
                            exit_code = out.exit_code,
                            command = skip_if_resolved.display(),
                            "skip_if probe could not be executed — treating the node as not skippable"
                        );
                    }
                });
            if let Ok(out) = probe {
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
                // Still run teardown steps to clean up anything setup may have
                // created — with their vars, which they interpolate exactly like a
                // `setup` step does. No run row here, so no node selections and no
                // run id: project surfaces only.
                self.ensure_stop_vars(run_name, None, &[]).await;
                self.run_teardown_steps(run_name, None).await;
                return Ok(StopResult::AlreadyStopped);
            }
        };

        if let Some(w) = self.internal_log.as_mut() {
            w.set_run_id(run.run_id);
        }

        if !run.is_live() {
            // Latest run already ended — it is history now, never deleted here.
            // Teardown steps still run so a re-stop stays a cleanup tool.
            self.ensure_stop_vars(run_name, Some(run.run_id), &[]).await;
            self.run_teardown_steps(run_name, Some(run.run_id)).await;
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

        // `on_stop` and the project `teardown` steps may both reference
        // `${vars.*}`. Only what the stopping nodes and the project-level surfaces
        // can reach, for the same reason `start` is selective: a `veld stop` must
        // not run a credential helper for a var nothing here mentions.
        let selections: Vec<graph::NodeSelection> = run
            .nodes
            .values()
            .map(|n| graph::NodeSelection {
                node: n.node_name.clone(),
                variant: n.variant.clone(),
            })
            .collect();
        let stop_vars = self
            .ensure_stop_vars(run_name, Some(run_id), &selections)
            .await;

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
                    let hostname = url::hostname_of_url(url_str);
                    let _ = self.helper_client.remove_host(hostname).await;
                    self.remove_route_by_hostname(hostname, run_name, node_state)
                        .await;
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
        self.run_teardown_steps(run_name, Some(run.run_id)).await;

        // Persist the final node states while the run is still `stopping`
        // (save_run refuses to touch terminal runs), then finalize it into
        // history with whatever end_reason `begin_ending` stored.
        self.save_state(&run)?;
        let _ = self.db.finalize_run(&run.run_id)?;

        self.internal_log(&format!("[stop] environment '{run_name}' stopped"))
            .await;

        Ok(StopResult::Stopped)
    }

    /// Refuse to start when another *project's* running run already serves one
    /// of this run's hostnames (#170).
    ///
    /// Best-effort: a registry read failure warns and lets the start proceed.
    /// The decision itself lives in [`claimed_hostname`], which is where the
    /// scoping rules are documented and tested.
    fn check_hostnames_unclaimed(
        &self,
        plan: &[Vec<NodeSelection>],
        run_name: &str,
        ctx: &UrlContext,
    ) -> Result<(), OrchestratorError> {
        let planned = planned_hostnames(&self.config, plan, run_name, ctx)?;

        let registry = match self.db.registry() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "could not check hostnames against other projects");
                return Ok(());
            }
        };
        match claimed_hostname(&registry, &self.project_root, &planned) {
            None => Ok(()),
            // Same directory, different path spelling: the remedy is a path, not
            // a name, so it gets its own message.
            Some(claim) if claim.same_dir => {
                Err(OrchestratorError::HostnameClaimedByOtherSpelling {
                    hostname: claim.hostname,
                    run_name: claim.run_name,
                    project_root: claim.project_root,
                })
            }
            Some(claim) => Err(OrchestratorError::HostnameClaimed {
                hostname: claim.hostname,
                run_name: claim.run_name,
                project_name: claim.project_name,
                project_root: claim.project_root,
            }),
        }
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

        self.release_shares_of_replaced_run(&run.run_id).await;
    }

    /// Release the daemon-held shares of a run we just replaced (#171).
    ///
    /// Shares are keyed by `run_id`, and a replacement mints a new one — so
    /// without this the old run's peer/web share stays alive, pointing at
    /// whatever now listens on that hostname, until its TTL expires. `veld stop`
    /// and `veld restart` already do this explicitly, and the GC pass releases
    /// runs it *finds* dead; a deliberate replacement is neither.
    ///
    /// Same shape as `veld restart`'s call: bounded by 5s so an unresponsive
    /// daemon cannot stall a start, and silent on `NotRunning` — a daemon that
    /// isn't up holds no shares, so there is nothing to report.
    async fn release_shares_of_replaced_run(&self, run_id: &uuid::Uuid) {
        let client = crate::share::DaemonClient::new();
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.unshare_run(&run_id.to_string()),
        )
        .await
        {
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => {
                self.emit(ProgressEvent::Notice {
                    message: format!(
                        "Released {n} share(s) of the replaced run — re-share if you still \
                         need the URL."
                    ),
                });
            }
            Ok(Err(crate::share::DaemonError::NotRunning)) => {}
            Ok(Err(e)) => {
                self.emit(ProgressEvent::Notice {
                    message: format!(
                        "Could not release the replaced run's shares ({e}) — any share of it \
                         now points at the new run. Check `veld shares` and stop it with \
                         `veld unshare <id>`."
                    ),
                });
            }
            Err(_) => {
                self.emit(ProgressEvent::Notice {
                    message: "Timed out releasing the replaced run's shares — any share of it \
                              now points at the new run. Check `veld shares` and stop it with \
                              `veld unshare <id>`."
                        .to_owned(),
                });
            }
        }
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
            let hostname = url::hostname_of_url(url_str);
            let _ = self.helper_client.remove_host(hostname).await;
            self.remove_route_by_hostname(hostname, run_name, node_state)
                .await;
        }
    }

    /// Remove a node's Caddy route: the hostname-keyed id this build writes,
    /// plus the pre-#170 id, in case the route was stored by an older helper
    /// that is still running after a `veld update` (see
    /// `url::legacy_run_route_id`). Both deletes are best-effort — an id that
    /// isn't there makes the helper answer with an error we ignore.
    async fn remove_route_by_hostname(
        &self,
        hostname: &str,
        run_name: &str,
        node_state: &NodeState,
    ) {
        let route_id = url::run_route_id(hostname);
        let _ = self.helper_client.remove_route(&route_id).await;
        let legacy = url::legacy_run_route_id(run_name, &node_state.node_name, &node_state.variant);
        let _ = self.helper_client.remove_route(&legacy).await;
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
            // A named port is an output *and* a builtin at start time; it has to
            // be both here too, or `${veld.ports.debug}` resolves in the command
            // that created the container and not in the one that removes it.
            if k.starts_with("ports.") {
                ctx.set_builtin(k, v.clone());
            }
        }
        if let Some(port) = node_state.port {
            ctx.set_builtin("port", port.to_string());
        }
        // `${veld.url}` and its pieces were missing here while `${veld.port}` was
        // present — an asymmetry with no reason behind it, and the URL is the
        // half a teardown hook is more likely to want.
        if let Some(url) = &node_state.url {
            for (key, value) in url_builtins(url) {
                ctx.set_builtin(key, value);
            }
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

        // An `on_stop` hook is that node's teardown, so its output goes to that
        // node's own `server` stream — the container it removed is the last
        // thing in the node's log, which is where someone debugging a leak
        // looks. Without a run instance there is nothing to attach rows to.
        let sink = step_line_sink(
            run_id.map(|id| {
                LogWriter::for_node(
                    self.db.clone(),
                    &self.project_root,
                    run_name,
                    id,
                    &node_state.node_name,
                    &node_state.variant,
                    LogStream::Server,
                )
            }),
            &self.progress_tx,
            (node_state.node_name.clone(), node_state.variant.clone()),
        );
        match process::run_command(&resolved_cmd, &working_dir, &env, None, Some(sink)).await {
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

    /// Build the interpolation context a `vars` **value** is resolved in.
    ///
    /// Run-scoped builtins and nothing else, deliberately: a var is one value for
    /// the whole run, so `${veld.port}` or `${veld.node}` in one could only mean
    /// some arbitrary node's. `config::BuiltinScopeKind::Vars` is the lint-side
    /// statement of the same rule, and `check_builtin_names` refuses the
    /// per-node names here rather than letting a literal `${veld.port}` reach a
    /// process as text.
    fn vars_context(
        &self,
        run_name: &str,
        run_id: Option<uuid::Uuid>,
        branch: &str,
        worktree: &str,
        username: &str,
    ) -> VariableContext {
        let mut ctx = VariableContext::new();
        BuiltinScope {
            run_name,
            // `None` only on the stop paths that run after the run row is gone —
            // `${veld.run_id}` in a var is then unavailable, exactly as it is in a
            // project step.
            run_id: run_id.map(|id| id.to_string()),
            project_root: &self.project_root,
            project_name: &self.config.name,
            worktree,
            branch,
            username,
            node: None,
        }
        .apply(&mut ctx);
        ctx
    }

    /// Resolve the `${vars.*}` the stop path needs, **into `self.resolved_vars`**.
    ///
    /// Into the field, not a local, because that is the only thing
    /// [`Self::project_step_context`] can see — and that is what a project
    /// `teardown` step interpolates against. A local map reaches
    /// `run_on_stop_hook` and nothing else, which is how `${vars.*}` in a
    /// `teardown` step came to fail with "no var named …" on a standalone
    /// `veld stop` while working when start and stop shared a process. It is the
    /// mirror of the `setup` bug, on the other end of the run.
    ///
    /// Extends rather than replaces, so a var already resolved by `start` in this
    /// process is not resolved twice — two readings of a rotating credential would
    /// disagree. A failing source must not abort the stop: teardown running
    /// matters more than every step interpolating, so this degrades to whatever is
    /// already cached and the affected steps report being skipped.
    async fn ensure_stop_vars(
        &mut self,
        run_name: &str,
        run_id: Option<uuid::Uuid>,
        selections: &[graph::NodeSelection],
    ) -> Arc<HashMap<String, String>> {
        // Only reuse a cache that belongs to *this* run — see `resolved_vars_run`.
        let mut merged: HashMap<String, String> =
            if self.resolved_vars_run.as_deref() == Some(run_name) {
                self.resolved_vars.as_deref().cloned().unwrap_or_default()
            } else {
                HashMap::new()
            };
        let needed = config::vars_for_teardown(&self.config, selections);
        if !needed.iter().all(|n| merged.contains_key(n)) {
            let ctx = self.vars_context(
                run_name,
                run_id,
                &url::detect_git_branch(&self.project_root),
                self.project_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("default"),
                &whoami_username(),
            );
            match crate::values::resolve_vars(
                self.config.vars.as_ref(),
                &self.var_overrides,
                Some(&self.project_root),
                &ctx,
                &needed,
                &merged,
            )
            .await
            {
                Ok(v) => merged.extend(v),
                // One failing source blanks the whole map, so the per-step warning
                // that follows says `no var named …` about a var that *is*
                // declared. Name the real cause here.
                Err(e) => tracing::warn!(
                    error = %e,
                    "could not resolve project vars for teardown — any teardown step \
                     or on_stop hook using ${{vars.*}} will be skipped"
                ),
            }
        }
        let shared = Arc::new(merged);
        self.resolved_vars = Some(Arc::clone(&shared));
        self.resolved_vars_run = Some(run_name.to_owned());
        shared
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
    async fn run_setup_steps(
        &self,
        run_name: &str,
        run_id: Option<uuid::Uuid>,
    ) -> Result<(), OrchestratorError> {
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
            let sink = self.project_step_sink(run_name, run_id, "setup", &step.name);
            match process::run_command(&resolved_cmd, &self.project_root, &env, None, Some(sink))
                .await
            {
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

    /// Sink for a project-level `setup`/`teardown` step's output.
    ///
    /// These steps belong to no node, so their lines go to the run-level
    /// `setup` stream (`veld logs` reads it by default) and are attributed in
    /// the UI to a pseudo-node — `setup:<step name>` — which is how a reader
    /// tells two steps apart in one interleaved stream. A `setup` step runs
    /// before the run row is written; the rows still carry the run id it is
    /// about to get, so they are in scope the moment it exists.
    fn project_step_sink(
        &self,
        run_name: &str,
        run_id: Option<uuid::Uuid>,
        kind: &str,
        step_name: &str,
    ) -> process::LineSink {
        // `setup` rows are read run-level but carry a node, which is what makes
        // two steps distinguishable in one interleaved stream: `veld logs`
        // labels them `setup:<step name>`.
        //
        // With no run instance (a `veld stop` on an environment veld has no row
        // for) nothing is written. Such rows would carry a NULL `run_id`, which
        // every run-scoped read filters out — `veld logs --all-runs` drops the
        // `run_id` predicate and would surface them, but that is the only way to
        // reach them, and a line you can read only under a flag you did not know
        // to pass is not worth keeping forever. Whoever ran the `stop` sees the
        // output live: the sink still reports it.
        let writer = run_id.map(|id| {
            LogWriter::for_node(
                self.db.clone(),
                &self.project_root,
                run_name,
                id,
                kind,
                step_name,
                LogStream::Setup,
            )
        });
        step_line_sink(
            writer,
            &self.progress_tx,
            (kind.to_owned(), step_name.to_owned()),
        )
    }

    /// Run project-level teardown steps sequentially. Best-effort: failures
    /// are logged but never propagated.
    async fn run_teardown_steps(&self, run_name: &str, run_id: Option<uuid::Uuid>) {
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
            let sink = self.project_step_sink(run_name, run_id, "teardown", &step.name);
            match process::run_command(&resolved_cmd, &self.project_root, &env, None, Some(sink))
                .await
            {
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

/// Whether a probe's exit code means the shell could not run it at all, rather
/// than that it ran and answered "no".
///
/// A `skip_if` is a predicate, so its output is deliberately not logged — but a
/// typo'd or missing binary is not an answer, and since the probe's stderr no
/// longer reaches the terminal, `sh: docker: command not found` would otherwise
/// be silent in every channel. 126/127 are the shell's conventional
/// "found but not executable" / "not found".
fn probe_could_not_run(exit_code: i32) -> bool {
    exit_code == 126 || exit_code == 127
}

/// Build the line sink for a step run through [`process::run_command`].
///
/// Every line the step prints goes to up to two places: the log stream `writer`
/// belongs to, so `veld logs` and the management UI can show it after the fact,
/// and the progress channel, so it is visible while the step runs. Either may be
/// absent — a step with no run instance to attach rows to has no writer, and a
/// `veld stop` has no progress channel (see below) — but never both silently:
/// with no channel the lines go to stderr. It goes to the
/// progress channel rather than straight to the terminal because the CLI draws
/// spinners there — a child writing to the inherited stderr (what `command`
/// steps used to do) scribbles over them.
///
/// With no progress channel there are no spinners to protect and nothing else
/// to show the user the step's output live, so the lines go to stderr instead.
/// That is the `veld stop` path: teardown steps and `on_stop` hooks used to
/// print straight to the terminal, and a `docker compose down` that takes
/// twenty seconds must not become a silent pause.
///
/// `label` is the `node:variant` pair the line is attributed to in the UI; for
/// project-level steps it is a pseudo-node (`setup`, `teardown`) and the step
/// name, since those have no node.
fn step_line_sink(
    writer: Option<LogWriter>,
    tx: &Option<mpsc::UnboundedSender<ProgressEvent>>,
    label: (String, String),
) -> process::LineSink {
    let tx = tx.clone();
    let (node, variant) = label;
    Arc::new(move |lines: &[String]| {
        if let Some(ref w) = writer {
            let _ = w.write_lines(chrono::Utc::now(), lines);
        }
        match tx {
            Some(ref tx) => {
                let _ = tx.send(ProgressEvent::NodeLogLines {
                    node: node.clone(),
                    variant: variant.clone(),
                    lines: lines.to_vec(),
                });
            }
            None => {
                for line in lines {
                    eprintln!("  {node}:{variant} {line}");
                }
            }
        }
    })
}

/// Write a line to the debug log (no-op when writer is None).
async fn debug_log_free(writer: &Option<LogWriter>, message: &str) {
    if let Some(writer) = writer {
        let _ = writer.write_line(&format!("[VELD] {message}")).await;
    }
}

/// Record sensitive keys **without discarding the ones already there**.
///
/// The whole bug class this replaces was an `=` where an extend belonged: three
/// separate places contribute keys (declared `sensitive_outputs`, secret `env`
/// values, tainted synthetic outputs) and whichever ran last silently won. A
/// command node using both a secret `env` value and `sensitive_outputs` stopped
/// masking the env value. Additive-and-idempotent is the only safe shape here, so
/// it gets a name and a test rather than being open-coded at each site.
fn mark_sensitive(keys: &mut Vec<String>, add: impl IntoIterator<Item = String>) {
    for key in add {
        if !keys.contains(&key) {
            keys.push(key);
        }
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
///
/// Known limit: taint is not transitive within one `outputs` map. Each template is
/// judged against the sensitivity known *before* the map is resolved, so a synthetic
/// output deriving from another synthetic output that is itself only tainted-by-
/// derivation is not marked. That is currently unreachable — the interpolation
/// context holds the *captured* outputs, not siblings being computed in the same
/// pass — but it is the thing to fix first if sibling references ever resolve.
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
    //
    // Extend, never assign. Both branches above already put keys here — the
    // `start_server` path adds secret `env` keys, and `execute_command_isolated`
    // adds those plus any tainted synthetic output — so assigning discarded them.
    // For a `command` node that used both a secret `env` value and
    // `sensitive_outputs`, the env value silently stopped being masked.
    if let Some(sensitive) = sensitive_outputs {
        mark_sensitive(&mut node_state.sensitive_keys, sensitive);
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
    // `url` plus the individual location pieces (mirrors the Web URL API).
    for (key, value) in url_builtins(&https_url) {
        var_ctx.set_builtin(key, value);
    }

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
    // Normalised, because every removal path removes the DNS host by the
    // port-stripped name: a `urlTemplate` carrying a literal port would otherwise
    // add `host:PORT` and leave it in `/etc/hosts` forever.
    if let Err(e) = ctx
        .helper_client
        .add_host(url::hostname_of_url(&node_url), "127.0.0.1")
        .await
    {
        tracing::warn!(error = %e, "failed to add DNS host via helper");
    }
    let mut route = serde_json::json!({
        // Keyed by hostname (#170) — see `url::run_route_id` for why that, and
        // not the run name or the run id. Normalised through `hostname_of_url`
        // because a `urlTemplate` can carry a literal port or path
        // (`app.localhost:3000`) that the removal sides strip; without this the
        // two would derive different ids and the route would be unremovable.
        // Deliberately no legacy-id delete here: the legacy id may belong to
        // another project's live run, which is the very collision this fixes.
        // Teardown handles the legacy entry.
        "route_id": url::run_route_id(url::hostname_of_url(&node_url)),
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
            .filter(|(_, value)| value.secret())
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
        mark_sensitive(&mut node_state.sensitive_keys, tainted);
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
                            .map(|r| r.line)
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

    // Idempotency check (skip_if). No sink: a probe's output is a predicate,
    // not the node's output — logging it would put a "not installed" message in
    // the log of a node that then installed it. A probe that cannot even be
    // spawned is a different thing and must not be silent, since its output no
    // longer reaches the terminal either.
    if let Some(ref skip_if_cmd) = resolved.skip_if {
        let skip_if_resolved = skip_if_cmd.interpolate(var_ctx)?;
        let skip_if_result =
            process::run_command(&skip_if_resolved, &working_dir, &env, None, None).await;
        match skip_if_result {
            Err(ref e) => tracing::warn!(
                node = sel.node,
                variant = sel.variant,
                error = %e,
                "skip_if probe could not run — treating the node as not skippable"
            ),
            Ok(ref out) if probe_could_not_run(out.exit_code) => tracing::warn!(
                node = sel.node,
                variant = sel.variant,
                exit_code = out.exit_code,
                command = skip_if_resolved.display(),
                "skip_if probe could not be executed — treating the node as not skippable"
            ),
            Ok(_) => {}
        }
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
    // A command node's output belongs to that node's `server` stream, exactly
    // like a `start_server` node's — same rows, so `veld logs --node <n>`, the
    // failure tail and the management UI all show it without knowing which kind
    // of node produced it.
    let sink = step_line_sink(
        Some(LogWriter::for_node(
            ctx.db.clone(),
            &ctx.project_root,
            &ctx.run_name,
            ctx.run_id,
            &sel.node,
            &sel.variant,
            LogStream::Server,
        )),
        &ctx.progress_tx,
        (sel.node.clone(), sel.variant.clone()),
    );
    let result = process::run_command(
        &resolved_cmd,
        &working_dir,
        &env,
        Some(&output_file),
        Some(sink),
    )
    .await?;

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
        // Taint propagation, as on the `start_server` path — and this is the sharper
        // case: these templates are interpolated *after* the command ran, with its
        // captured outputs in scope, so `${output.TOKEN}` where `TOKEN` is declared
        // sensitive is not hypothetical, it is the intended usage.
        //
        // `resolved.sensitive_outputs` is consulted directly because the caller
        // applies it to `node_state` only after this function returns.
        let mut sensitive: Vec<String> = node_state.sensitive_keys.clone();
        sensitive.extend(resolved.sensitive_outputs.iter().flatten().cloned());
        let secret_vars: Vec<String> = ctx
            .config
            .vars
            .iter()
            .flatten()
            .filter(|(_, value)| value.secret())
            .map(|(name, _)| name.clone())
            .collect();
        let mut tainted: Vec<String> = Vec::new();
        for (key, template) in map {
            let value = crate::variables::interpolate(template, var_ctx)?;
            if template_is_tainted(template, &sensitive, &secret_vars) {
                tainted.push(key.clone());
            }
            node_state.outputs.insert(key.clone(), value);
        }
        mark_sensitive(&mut node_state.sensitive_keys, tainted);
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

/// The user's name for `${veld.username}`.
///
/// `pub(crate)` because a pane command resolves the same builtin
/// (`veld-daemon`'s `pane_context`) and a second copy drifted immediately — its
/// fallback was `""`, which turns an unset `USER` into an *empty argument*
/// rather than a visible placeholder.
pub fn whoami_username() -> String {
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

    /// A registry holding one run of one project, at the given status, serving
    /// `hostname` — the shape `Db::registry` produces.
    fn registry_with(
        root: &str,
        project_name: &str,
        run_name: &str,
        status: RunStatus,
        hostname: &str,
    ) -> crate::state::GlobalRegistry {
        let mut runs = HashMap::new();
        runs.insert(
            run_name.to_owned(),
            crate::state::RegistryRunInfo {
                run_id: uuid::Uuid::new_v4(),
                name: run_name.to_owned(),
                status,
                urls: HashMap::from([(
                    "web:local".to_owned(),
                    format!("https://{hostname}:18443"),
                )]),
            },
        );
        let mut projects = HashMap::new();
        projects.insert(
            root.to_owned(),
            crate::state::RegistryEntry {
                project_root: PathBuf::from(root),
                project_name: project_name.to_owned(),
                runs,
            },
        );
        crate::state::GlobalRegistry { projects }
    }

    #[test]
    fn another_projects_running_run_claims_the_hostname() {
        let reg = registry_with(
            "/repos/clone-a",
            "app",
            "main",
            RunStatus::Running,
            "web.main.app.localhost",
        );
        let planned = vec!["web.main.app.localhost".to_owned()];

        let claim = claimed_hostname(&reg, Path::new("/repos/clone-b"), &planned)
            .expect("clone B must be refused — one hostname cannot route to two apps");
        assert_eq!(claim.hostname, "web.main.app.localhost");
        assert_eq!(claim.run_name, "main");
        assert_eq!(claim.project_root, "/repos/clone-a");

        // Hostnames are case-insensitive in DNS and in Caddy's host matcher.
        let shouty = vec!["WEB.MAIN.APP.LOCALHOST".to_owned()];
        assert!(claimed_hostname(&reg, Path::new("/repos/clone-b"), &shouty).is_some());
    }

    #[test]
    fn our_own_project_never_claims_against_itself() {
        // Reusing a name inside one project is the replace path, not a conflict —
        // `cleanup_stale_run` handles it, and this check must not pre-empt it.
        let reg = registry_with(
            "/repos/clone-a",
            "app",
            "main",
            RunStatus::Running,
            "web.main.app.localhost",
        );
        let planned = vec!["web.main.app.localhost".to_owned()];
        assert!(claimed_hostname(&reg, Path::new("/repos/clone-a"), &planned).is_none());
    }

    #[test]
    fn only_a_running_run_claims_a_hostname() {
        // `Db::registry` reports each environment's LATEST run whatever its
        // status, and node rows keep their URL as history — so anything but
        // `Running` would block a start on a hostname nobody is serving.
        // `is_live()` is the wrong predicate here: it admits `Starting` and
        // `Stopping` too.
        for status in [
            RunStatus::Starting,
            RunStatus::Stopping,
            RunStatus::Stopped,
            RunStatus::Failed,
            RunStatus::Crashed,
        ] {
            let reg = registry_with(
                "/repos/clone-a",
                "app",
                "main",
                status,
                "web.main.app.localhost",
            );
            let planned = vec!["web.main.app.localhost".to_owned()];
            assert!(
                claimed_hostname(&reg, Path::new("/repos/clone-b"), &planned).is_none(),
                "{status:?} must not claim a hostname"
            );
        }
    }

    #[test]
    fn a_different_hostname_is_never_a_conflict() {
        // The everyday case: two unrelated repos both running `main`. Their
        // `{project}` labels differ, so the URLs differ and both must start.
        let reg = registry_with(
            "/repos/other",
            "other",
            "main",
            RunStatus::Running,
            "web.main.other.localhost",
        );
        let planned = vec!["web.main.app.localhost".to_owned()];
        assert!(claimed_hostname(&reg, Path::new("/repos/app"), &planned).is_none());
    }

    #[test]
    fn one_checkout_reached_by_two_paths_is_reported_as_such() {
        // `root_key` stores the spelling the CLI was invoked with, so a symlinked
        // checkout is two registry rows — and `cleanup_stale_run`'s `get_run`,
        // keyed by our own spelling, cannot see the other one. Skipping it as
        // "our own project" would let both runs claim one hostname; the claim is
        // reported with `same_dir` so the caller can name the right remedy.
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("checkout");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("linked");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let reg = registry_with(
            real.to_str().unwrap(),
            "app",
            "main",
            RunStatus::Running,
            "web.main.app.localhost",
        );
        let planned = vec!["web.main.app.localhost".to_owned()];

        let claim = claimed_hostname(&reg, &link, &planned)
            .expect("the run under the other spelling must be reported, not skipped");
        assert!(
            claim.same_dir,
            "must be recognised as this same directory, not a different project"
        );

        // The identical spelling is still our own project and never a conflict.
        assert!(claimed_hostname(&reg, &real, &planned).is_none());
    }

    #[test]
    fn the_reported_conflict_is_stable_across_runs() {
        // Registry maps iterate in arbitrary order; the same state must always
        // name the same conflict, or the error text flickers between runs.
        let mut reg = registry_with(
            "/repos/zzz",
            "app",
            "zulu",
            RunStatus::Running,
            "web.zulu.app.localhost",
        );
        let other = registry_with(
            "/repos/aaa",
            "app",
            "alpha",
            RunStatus::Running,
            "web.alpha.app.localhost",
        );
        reg.projects.extend(other.projects);
        let planned = vec![
            "web.zulu.app.localhost".to_owned(),
            "web.alpha.app.localhost".to_owned(),
        ];

        for _ in 0..8 {
            let claim = claimed_hostname(&reg, Path::new("/repos/mine"), &planned).unwrap();
            assert_eq!(claim.hostname, "web.alpha.app.localhost");
        }
    }

    /// A project `teardown` step interpolates `${vars.*}` on a standalone
    /// `veld stop`, where nothing on the start path has populated the cache.
    ///
    /// `run_teardown_steps` reads `self.resolved_vars` through
    /// `project_step_context`, so a stop path that resolved its vars into a
    /// *local* left teardown with none: the step was skipped with a warning. The
    /// mirror of the `setup` bug, and invisible in the common case because a
    /// foreground `start` leaves the cache populated in the same process.
    #[tokio::test]
    async fn teardown_steps_see_vars_on_a_standalone_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let marker = project_root.join("teardown-ran.txt");

        let config: VeldConfig = serde_json::from_str(&format!(
            r#"{{
                "schemaVersion": "3",
                "name": "testcfg",
                "vars": {{ "marker": {} }},
                "teardown": [
                    {{ "name": "write", "shell": "echo saw-the-var > ${{vars.marker}}" }}
                ],
                "nodes": {{
                    "task": {{ "default_variant": "local", "variants": {{
                        "local": {{ "type": "command", "shell": "true" }}
                    }}}}
                }}
            }}"#,
            serde_json::to_string(&marker.to_string_lossy()).unwrap()
        ))
        .unwrap();

        // A fresh orchestrator, exactly as `veld stop` builds one: no `start` ran
        // in this process, so `resolved_vars` is None.
        let mut orch = test_orchestrator(project_root, config);
        assert!(orch.resolved_vars.is_none());

        orch.ensure_stop_vars("dev", None, &[]).await;
        orch.run_teardown_steps("dev", None).await;

        assert_eq!(
            std::fs::read_to_string(&marker).unwrap_or_default().trim(),
            "saw-the-var",
            "the teardown step must have interpolated its var and run"
        );

        // The `None` run id path writes no rows on purpose: they would carry a
        // NULL `run_id`, reachable only by `veld logs --all-runs`. The step's
        // output is reported live instead.
        let all = LogFilter {
            streams: Some(vec![LogStream::Setup.as_str()]),
            ..Default::default()
        };
        assert!(
            orch.db
                .tail_logs(project_root, "dev", &all, 100)
                .unwrap()
                .is_empty(),
            "a stop with no run instance must not persist unreachable rows"
        );
    }

    /// A project step's output lands on the run's `setup` stream, labelled with
    /// the step it came from.
    ///
    /// The label is the whole point of the pseudo-node: `setup` is read
    /// run-level, so without `node`/`variant` two steps' lines interleave into
    /// one anonymous blob. Both pipes are covered — stderr is where a build tool
    /// actually talks, and it is the one that used to be inherited.
    #[tokio::test]
    async fn teardown_step_output_is_recorded_under_its_step_name() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "testcfg",
                "teardown": [
                    { "name": "compose-down", "shell": "echo on-stdout; echo on-stderr >&2" }
                ],
                "nodes": {
                    "task": { "default_variant": "local", "variants": {
                        "local": { "type": "command", "shell": "true" }
                    }}
                }
            }"#,
        )
        .unwrap();

        let mut orch = test_orchestrator(project_root, config);
        let run_id = uuid::Uuid::new_v4();
        orch.ensure_stop_vars("dev", Some(run_id), &[]).await;
        orch.run_teardown_steps("dev", Some(run_id)).await;

        let filter = LogFilter {
            streams: Some(vec![LogStream::Setup.as_str()]),
            run_id: Some(run_id.to_string()),
            ..Default::default()
        };
        let rows = orch
            .db
            .tail_logs(project_root, "dev", &filter, 100)
            .unwrap();
        let lines: Vec<&str> = rows.iter().map(|r| r.line.as_str()).collect();
        assert!(lines.contains(&"on-stdout"), "got {lines:?}");
        assert!(lines.contains(&"on-stderr"), "got {lines:?}");
        for row in &rows {
            assert_eq!(row.node.as_deref(), Some("teardown"));
            assert_eq!(row.variant.as_deref(), Some("compose-down"));
        }
    }

    /// `veld stop --all` loops one `Orchestrator` over every run, so the var
    /// cache has to be keyed by run, not merely present.
    ///
    /// A var may read `${veld.run}`. Reusing the first run's map for the second
    /// would have run B's `teardown` act on run A's resources — `docker rm` the
    /// wrong container, which is exactly the failure teardown exists to prevent.
    #[tokio::test]
    async fn stopping_two_runs_on_one_orchestrator_does_not_reuse_the_first_runs_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "testcfg",
                "vars": { "container": "app-${veld.run}" },
                "teardown": [ { "name": "rm", "shell": "echo ${vars.container}" } ],
                "nodes": {
                    "task": { "default_variant": "local", "variants": {
                        "local": { "type": "command", "shell": "true" }
                    }}
                }
            }"#,
        )
        .unwrap();

        let mut orch = test_orchestrator(project_root, config);
        let first = orch.ensure_stop_vars("alpha", None, &[]).await;
        assert_eq!(
            first.get("container").map(String::as_str),
            Some("app-alpha")
        );

        // Same orchestrator, second run — as `veld stop --all` does.
        let second = orch.ensure_stop_vars("bravo", None, &[]).await;
        assert_eq!(
            second.get("container").map(String::as_str),
            Some("app-bravo"),
            "run bravo must not inherit alpha's resolved vars"
        );
    }

    /// **The pre-flight must see the whole plan, not the selections.**
    ///
    /// A var reached only through a transitive dependency is the exact case the
    /// pre-flight exists for: without expansion it is discovered at resolution
    /// time, after the dependency is already running, and the choice then is
    /// between tearing down a half-started environment and prompting while it
    /// runs. `unanswered_vars` therefore expands through `build_execution_plan`
    /// exactly as `start` does — and the second half of the test is the other
    /// side of the same coin: a var no selected node reaches must NOT be asked
    /// for, or `veld start docs` demands the database password.
    #[tokio::test]
    async fn the_preflight_sees_dependencies_and_ignores_unrelated_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "testcfg",
                "vars": {
                    "db_secret": { "machine": { "prompt": "DB password?" } },
                    "docs_only":  { "machine": { "prompt": "Docs token?" } },
                    "answered":   { "machine": { "default": "fine" } }
                },
                "nodes": {
                    "db": { "default_variant": "local", "variants": {
                        "local": { "type": "command", "shell": "true",
                                   "env": { "PW": "${vars.db_secret}" } }
                    }},
                    "api": { "default_variant": "local",
                             "depends_on": { "db": "local" },
                             "variants": { "local": { "type": "command", "shell": "true" } } },
                    "docs": { "default_variant": "local", "variants": {
                        "local": { "type": "command", "shell": "true",
                                   "env": { "T": "${vars.docs_only}" } }
                    }}
                }
            }"#,
        )
        .unwrap();

        let orch = test_orchestrator(tmp.path(), config);
        let api = vec![NodeSelection {
            node: "api".to_owned(),
            variant: "local".to_owned(),
        }];
        let missing = orch.unanswered_vars(&api).expect("plan expands");
        let names: Vec<&str> = missing.iter().map(|v| v.name.as_str()).collect();

        assert!(
            names.contains(&"db_secret"),
            "a var used only by a dependency must be found before anything spawns, got {names:?}"
        );
        assert!(
            !names.contains(&"docs_only"),
            "a var outside the plan must not be demanded, got {names:?}"
        );
        assert!(
            !names.contains(&"answered"),
            "a var with a default is answered, got {names:?}"
        );
        assert_eq!(
            missing
                .iter()
                .find(|v| v.name == "db_secret")
                .map(|v| v.question()),
            Some("DB password?".to_owned()),
            "the declared prompt is what the human is asked"
        );
    }

    /// A stored answer the config no longer allows is caught by the pre-flight
    /// too, so `choices` changing under an override surfaces before the run.
    #[tokio::test]
    async fn the_preflight_flags_an_answer_the_choices_no_longer_allow() {
        let tmp = tempfile::tempdir().unwrap();
        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "testcfg",
                "vars": {
                    "runtime": { "machine": { "default": "docker",
                                              "choices": ["docker", "podman"] } }
                },
                "nodes": {
                    "task": { "default_variant": "local", "variants": {
                        "local": { "type": "command", "shell": "true",
                                   "env": { "R": "${vars.runtime}" } }
                    }}
                }
            }"#,
        )
        .unwrap();

        let mut orch = test_orchestrator(tmp.path(), config);
        let sels = vec![NodeSelection {
            node: "task".to_owned(),
            variant: "local".to_owned(),
        }];
        assert!(
            orch.unanswered_vars(&sels).expect("expands").is_empty(),
            "the default is legal, so nothing is missing"
        );

        orch.set_var_answers(
            [(
                "runtime".to_owned(),
                crate::config::ConfigValue::literal("containerd"),
            )]
            .into(),
        );
        let missing = orch.unanswered_vars(&sels).expect("expands");
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].name, "runtime");
        assert_eq!(
            missing[0].stale.as_deref(),
            Some("containerd"),
            "a non-secret stale literal is shown, so the human can see what to replace"
        );
    }

    /// Build a minimal orchestrator backed by a throwaway database, with no
    /// helper interaction — enough to exercise `run_terminal` in isolation.
    fn test_orchestrator(project_root: &std::path::Path, config: VeldConfig) -> Orchestrator {
        let db = Db::open_at(&project_root.join("veld.db")).unwrap();
        Orchestrator {
            config,
            // A synthetic path for a test Orchestrator; no file is read through it.
            config_path: project_root.join("veld.json"), // root-config-gate-ok
            config_hash: String::new(),
            project_root: project_root.to_path_buf(),
            project_id: crate::project_id::ProjectId::from_stored(project_root.to_string_lossy()),
            var_overrides: Default::default(),
            var_provenance: Default::default(),
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
            resolved_vars_run: None,
            terminal_outputs: Some(HashMap::new()),
        }
    }

    /// The `${veld.url*}` family is derived from the URL alone, so the stop path
    /// — which has only `NodeState.url` — produces exactly what the start path
    /// built from `(hostname, https_port)`.
    #[test]
    fn url_builtins_round_trip_both_port_forms() {
        let by_key = |url: &str| -> HashMap<String, String> {
            url_builtins(url)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect()
        };

        // The non-443 form the local helper actually serves.
        let local = by_key("https://web.dev.veld.localhost:8443");
        assert_eq!(local["url"], "https://web.dev.veld.localhost:8443");
        assert_eq!(local["url.hostname"], "web.dev.veld.localhost");
        assert_eq!(local["url.host"], "web.dev.veld.localhost:8443");
        assert_eq!(local["url.origin"], "https://web.dev.veld.localhost:8443");
        assert_eq!(local["url.scheme"], "https");
        assert_eq!(local["url.port"], "8443");

        // Port 443 is elided from the URL, exactly as the start path elides it,
        // and `url.port` still reports the real one.
        let standard = by_key("https://web.dev.veld.localhost");
        assert_eq!(standard["url.hostname"], "web.dev.veld.localhost");
        assert_eq!(standard["url.host"], "web.dev.veld.localhost");
        assert_eq!(standard["url.port"], "443");

        // A `url_template` may carry a literal port, which `node_hostname` passes
        // through. The old two-place construction put it in the wrong field at
        // `https_port == 443` — `url.hostname` kept the `:3000` and `url.port`
        // said `443`. Pinned because it is the one input where this is a
        // deliberate behaviour change rather than an extraction.
        let templated = by_key("https://svc.localhost:3000");
        assert_eq!(templated["url.hostname"], "svc.localhost");
        assert_eq!(templated["url.port"], "3000");
        assert_eq!(templated["url.host"], "svc.localhost:3000");

        // A hostname veld generates can never contain a colon, so the split is
        // unambiguous for everything except the templated case above.
        assert!(!crate::url::slugify("weird:name").contains(':'));
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

    /// A node that inherits `type` from the node level is still hostname-checked.
    ///
    /// #170's collision check read `step_type` off the raw variant. That predates
    /// node-level defaults (F3), where `type` is declared once on the node and the
    /// variant omits it — so the raw read saw `None`, decided the node was not a
    /// `start_server`, and skipped it. The result was a *safety* check that quietly
    /// stopped covering the configs using the newer feature: two checkouts could
    /// both claim one hostname and only one of them would route.
    ///
    /// The plain-variant case is asserted alongside it, because "resolve everything"
    /// must not have broken the shape that already worked.
    #[test]
    fn node_level_type_is_still_hostname_checked() {
        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "app",
                "url_template": "{service}.{run}.{project}.localhost",
                "nodes": {
                    "inherits": {
                        "type": "start_server",
                        "variants": { "dev": { "shell": "serve" } }
                    },
                    "explicit": {
                        "variants": { "dev": { "type": "start_server", "shell": "serve" } }
                    },
                    "task": {
                        "variants": { "dev": { "type": "command", "shell": "true" } }
                    }
                }
            }"#,
        )
        .expect("fixture must parse");

        let plan = vec![vec![
            NodeSelection {
                node: "inherits".to_owned(),
                variant: "dev".to_owned(),
            },
            NodeSelection {
                node: "explicit".to_owned(),
                variant: "dev".to_owned(),
            },
            NodeSelection {
                node: "task".to_owned(),
                variant: "dev".to_owned(),
            },
        ]];
        let ctx = UrlContext {
            branch: "main".to_owned(),
            worktree: "app".to_owned(),
            username: "dev".to_owned(),
            hostname: "box".to_owned(),
        };

        let planned = planned_hostnames(&config, &plan, "main", &ctx).expect("must render");

        assert!(
            planned.contains(&"inherits.main.app.localhost".to_owned()),
            "a node inheriting `type` from the node level must be checked: {planned:?}"
        );
        assert!(
            planned.contains(&"explicit.main.app.localhost".to_owned()),
            "{planned:?}"
        );
        // A `command` node serves no hostname, so offering one would invent a
        // collision that cannot happen.
        assert!(
            !planned.iter().any(|h| h.starts_with("task.")),
            "a command node must not claim a hostname: {planned:?}"
        );
    }

    /// Recording sensitive keys is additive and idempotent.
    ///
    /// This existed as `node_state.sensitive_keys = sensitive`, so for a `command`
    /// node the declared `sensitive_outputs` overwrote the secret `env` keys the
    /// execution path had already recorded — the env value simply stopped being
    /// masked, with nothing to indicate it. Three independent sources contribute
    /// keys, so the only correct shape is union.
    #[test]
    fn marking_sensitive_keys_never_drops_existing_ones() {
        let mut keys = vec!["ENV_SECRET".to_owned()];
        mark_sensitive(&mut keys, ["DECLARED_OUTPUT".to_owned()]);
        assert!(
            keys.contains(&"ENV_SECRET".to_owned()),
            "a later source must not discard an earlier one: {keys:?}"
        );
        assert!(keys.contains(&"DECLARED_OUTPUT".to_owned()));

        // Idempotent: the same key arriving twice must not accumulate.
        mark_sensitive(&mut keys, ["ENV_SECRET".to_owned()]);
        assert_eq!(
            keys.iter().filter(|k| *k == "ENV_SECRET").count(),
            1,
            "{keys:?}"
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
