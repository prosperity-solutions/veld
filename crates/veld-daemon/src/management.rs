//! Browser-based management dashboard served at `veld.localhost`.
//!
//! Provides a read-only overview of all Veld environments on the machine,
//! with clickable service URLs and live status badges.

use std::collections::HashMap;

use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::warn;
use veld_core::config;
use veld_core::db::{Db, LogFilter, LogStream};
use veld_core::state::{GlobalRegistry, NodeState, NodeStatus, RunStatus};

const DASHBOARD_HTML: &str = include_str!("../assets/management-ui.html");

/// The v2 management UI (React, built by `ui/` via build.rs into a single
/// self-contained HTML file). Served under /ide (worktree mode); a future
/// runs mode reaches parity with
/// the v1 dashboard above; Veld Desktop wraps this page.
const IDE_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/management-ui-ide.html"));

/// Open the central database, mapping failures to a 500.
pub(super) fn open_db() -> Result<Db, StatusCode> {
    Db::open().map_err(|e| {
        warn!("failed to open veld database: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// Build an axum [`Router`] for the management UI (mounted into the daemon's
/// HTTP server).
pub fn routes() -> Router {
    Router::new()
        .route("/", get(dashboard))
        // Same SPA; the join ticket rides in the URL fragment (client-only).
        .route("/join", get(dashboard))
        .route("/ide", get(ide_ui))
        // Liveness + version probe. `veld update` polls this to confirm the
        // daemon actually restarted onto the new binary (not just that *some*
        // daemon is reachable), mirroring the helper's version check.
        .route("/api/health", get(health))
        .route("/api/environments", get(list_environments))
        .route("/api/stats", get(get_stats))
        .route("/api/logs/{run}", get(get_logs))
        .route("/api/open-terminal", post(open_terminal))
        .route("/api/environments/{run}/stop", post(stop_environment))
        .route("/api/environments/{run}/restart", post(restart_environment))
        .route("/api/environments/{run}/action", post(run_action))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Liveness + version. Returns the daemon's compiled version so callers can
/// confirm which binary is actually serving.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn dashboard() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        DASHBOARD_HTML,
    )
        .into_response()
}

async fn ide_ui() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        IDE_HTML,
    )
        .into_response()
}

#[derive(Serialize)]
struct EnvironmentList {
    projects: Vec<ProjectInfo>,
}

#[derive(Serialize)]
struct ProjectInfo {
    name: String,
    project_root: String,
    runs: Vec<RunInfo>,
}

#[derive(Serialize)]
struct RunInfo {
    /// Environment name (what `--name` addresses).
    name: String,
    /// Status of the environment's latest run.
    status: RunStatus,
    /// Whether the latest run occupies the live slot. Stale URLs on a
    /// non-live run must never read as reachable — `urls`/node URLs are
    /// stripped server-side when this is false.
    live: bool,
    /// Full run UUID of the latest run — `run_id` means the canonical UUID on
    /// every veld JSON surface (`veld runs --json`, `veld status --json`).
    run_id: String,
    /// Git-style short prefix of `run_id`, for display.
    short_id: String,
    /// One-line outcome of the latest run when it has ended
    /// (e.g. "crashed (api:local pid died)").
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<String>,
    urls: HashMap<String, String>,
    nodes: Vec<NodeInfo>,
    /// Ended runs, newest first (retention-bounded) — the log run picker and
    /// the history view feed from this.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    history: Vec<HistoryEntry>,
}

/// One ended run in an environment's history, with the final node states —
/// enough for the dashboard's history run selector to render the full card
/// (badge, outcome, node table) for any past run.
#[derive(Serialize)]
struct HistoryEntry {
    /// Full run UUID (same contract as `veld runs --json`).
    run_id: String,
    /// Git-style short prefix, for display.
    short_id: String,
    status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<String>,
    /// Final node states (no URLs/PIDs — the run is over).
    nodes: Vec<HistoryNode>,
}

#[derive(Serialize)]
struct HistoryNode {
    name: String,
    variant: String,
    status: NodeStatus,
    /// Exit code where one was observable (command/oneshot nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<String>,
}

#[derive(Serialize)]
struct NodeInfo {
    name: String,
    variant: String,
    status: NodeStatus,
    url: Option<String>,
    pid: Option<u32>,
    #[serde(skip_serializing_if = "is_zero")]
    recovery_count: u32,
    #[serde(skip_serializing_if = "is_zero")]
    consecutive_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_liveness_error: Option<String>,
    /// Node-defined actions currently available (required outputs satisfied).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actions: Vec<ActionInfo>,
}

/// A node action exposed to the dashboard. The command itself stays
/// server-side; the browser only ever sees the name and label.
#[derive(Serialize)]
struct ActionInfo {
    name: String,
    label: String,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// Load a project's config (veld.json) for action lookup. Returns `None` if the
/// project has no readable config — the dashboard then simply shows no actions.
fn load_project_config(project_root: &std::path::Path) -> Option<config::VeldConfig> {
    config::load_config(&project_root.join("veld.json")).ok()
}

/// Compute the actions available for a running node: every action declared on
/// the matching config node whose `requires_outputs` are satisfied by the
/// node's live outputs.
fn available_actions(cfg: Option<&config::VeldConfig>, ns: &NodeState) -> Vec<ActionInfo> {
    let Some(cfg) = cfg else {
        return Vec::new();
    };
    cfg.nodes
        .get(&ns.node_name)
        .and_then(|n| n.actions.as_ref())
        .map(|actions| {
            actions
                .iter()
                .filter(|a| a.outputs_satisfied(&ns.outputs))
                .map(|a| ActionInfo {
                    name: a.name.clone(),
                    label: a.display_label().to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn list_environments() -> Result<Json<EnvironmentList>, StatusCode> {
    let db = open_db()?;
    let registry = db.registry().map_err(|e| {
        warn!("failed to load global registry: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut projects: Vec<ProjectInfo> = registry
        .projects
        .values()
        .map(|entry| {
            // Load full project state for node-level detail.
            let project_state = db.load_project_state(&entry.project_root).ok();
            // Load config so we know which actions each node exposes.
            let project_config = load_project_config(&entry.project_root);

            let mut runs: Vec<RunInfo> = entry
                .runs
                .values()
                .map(|r| {
                    let latest = project_state.as_ref().and_then(|ps| ps.get_run(&r.name));
                    let live = r.status.is_live();
                    let mut nodes: Vec<NodeInfo> = latest
                        .map(|rs| {
                            rs.nodes
                                .values()
                                .map(|ns| NodeInfo {
                                    name: ns.node_name.clone(),
                                    variant: ns.variant.clone(),
                                    status: ns.status.clone(),
                                    // Routes die with the run — an ended
                                    // run's URLs must not render as links.
                                    url: if live { ns.url.clone() } else { None },
                                    pid: if live { ns.pid } else { None },
                                    recovery_count: ns.recovery_count,
                                    consecutive_failures: ns.consecutive_failures,
                                    last_liveness_error: ns.last_liveness_error.clone(),
                                    actions: available_actions(project_config.as_ref(), ns),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    nodes.sort_by(|a, b| a.name.cmp(&b.name));

                    // Ended runs, newest first; the latest run is shown on
                    // the card itself, so history lists only its predecessors.
                    let history: Vec<HistoryEntry> = db
                        .list_runs(&entry.project_root, Some(&r.name))
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|run| !run.is_live())
                        .filter(|run| latest.is_none_or(|l| run.run_id != l.run_id))
                        .map(|run| {
                            let mut hnodes: Vec<HistoryNode> = run
                                .nodes
                                .values()
                                .map(|ns| HistoryNode {
                                    name: ns.node_name.clone(),
                                    variant: ns.variant.clone(),
                                    status: ns.status.clone(),
                                    exit_code: ns.outputs.get("exit_code").cloned(),
                                })
                                .collect();
                            hnodes.sort_by(|a, b| a.name.cmp(&b.name));
                            HistoryEntry {
                                run_id: run.run_id.to_string(),
                                short_id: run.short_id(),
                                status: run.status,
                                outcome: Some(run.outcome_label()),
                                created_at: run.created_at.to_rfc3339(),
                                ended_at: run.ended_at.map(|t| t.to_rfc3339()),
                                nodes: hnodes,
                            }
                        })
                        .collect();

                    RunInfo {
                        name: r.name.clone(),
                        status: r.status,
                        live,
                        run_id: latest
                            .map(|l| l.run_id.to_string())
                            .unwrap_or_else(|| r.run_id.to_string()),
                        short_id: latest
                            .map(|l| l.short_id())
                            .unwrap_or_else(|| r.run_id.to_string()[..8].to_owned()),
                        outcome: latest.filter(|l| !l.is_live()).map(|l| l.outcome_label()),
                        ended_at: latest.and_then(|l| l.ended_at).map(|t| t.to_rfc3339()),
                        urls: if live { r.urls.clone() } else { HashMap::new() },
                        nodes,
                        history,
                    }
                })
                .collect();
            runs.sort_by(|a, b| a.name.cmp(&b.name));

            ProjectInfo {
                name: entry.project_name.clone(),
                project_root: entry.project_root.display().to_string(),
                runs,
            }
        })
        .collect();

    projects.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(EnvironmentList { projects }))
}

// ---------------------------------------------------------------------------
// Process stats API
// ---------------------------------------------------------------------------

/// How many recent memory samples to include per node for the UI sparkline.
/// At the 5s scan cadence this is the last ~5 minutes.
const SPARK_POINTS: usize = 60;

/// Live resource stats for every running run, keyed by project root, then run
/// name, then node key (`"node:variant"`). Served on its own endpoint (not
/// folded into `/api/environments`) so the dashboard can patch the numbers in
/// place on a fast cadence without re-rendering — and skipping its render
/// fingerprint. Keyed by project root (not bare run name) because run names
/// collide across projects — two repos both on branch `main` default to a run
/// named `main` — and the dashboard cards are likewise project-scoped.
#[derive(Serialize)]
struct StatsResponse {
    projects: HashMap<String, HashMap<String, HashMap<String, NodeStats>>>,
}

#[derive(Serialize)]
struct NodeStats {
    /// CPU percentage of a single core, summed across the process tree.
    cpu: f32,
    /// Resident memory in bytes, summed across the process tree.
    mem: u64,
    /// Number of live processes in the tree.
    procs: u32,
    /// Recent memory samples (bytes), oldest-first, for the sparkline.
    spark: Vec<u64>,
}

async fn get_stats() -> Result<Json<StatsResponse>, StatusCode> {
    let db = open_db()?;
    let registry = db.registry().map_err(|e| {
        warn!("failed to load registry for stats: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let now = chrono::Utc::now();
    let mut projects: HashMap<String, HashMap<String, HashMap<String, NodeStats>>> = HashMap::new();
    for entry in registry.projects.values() {
        for (run_name, run_info) in &entry.runs {
            if run_info.status != RunStatus::Running {
                continue;
            }
            let latest = match db.latest_node_stats(&entry.project_root, run_name) {
                Ok(l) => l,
                Err(e) => {
                    warn!("failed to load stats for run '{run_name}': {e}");
                    continue;
                }
            };
            let mut nodes = HashMap::new();
            for (node_key, s) in latest {
                // Drop stale samples so a crashed node or a stopped daemon
                // shows as absent rather than freezing its last reading.
                if !s.is_fresh(now) {
                    continue;
                }
                let spark = db
                    .node_stats_history(&entry.project_root, run_name, &node_key, SPARK_POINTS)
                    .unwrap_or_default()
                    .iter()
                    .map(|h| h.memory_bytes)
                    .collect();
                nodes.insert(
                    node_key,
                    NodeStats {
                        cpu: s.cpu_percent,
                        mem: s.memory_bytes,
                        procs: s.process_count,
                        spark,
                    },
                );
            }
            if nodes.is_empty() {
                continue;
            }
            projects
                .entry(entry.project_root.display().to_string())
                .or_default()
                .insert(run_name.clone(), nodes);
        }
    }

    Ok(Json(StatsResponse { projects }))
}

// ---------------------------------------------------------------------------
// Log API
// ---------------------------------------------------------------------------

fn default_lines() -> usize {
    200
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default = "default_lines")]
    lines: usize,
    node: Option<String>,
    /// Filter by source: "all" (default), "server", or "client".
    #[serde(default = "default_source")]
    source: String,
    /// Run instance to read (id prefix). Default: the environment's latest
    /// run. `all` reads every run under the name interleaved (incl. legacy
    /// unscoped rows).
    run_id: Option<String>,
}

fn default_source() -> String {
    "all".to_owned()
}

#[derive(Serialize)]
struct LogResponse {
    nodes: Vec<NodeLogs>,
}

#[derive(Serialize)]
struct NodeLogs {
    node: String,
    variant: String,
    source: String,
    lines: Vec<String>,
}

/// Look up a run name in the global registry, returning the project root.
fn find_project_for_run(registry: &GlobalRegistry, run_name: &str) -> Option<std::path::PathBuf> {
    registry
        .projects
        .values()
        .find(|entry| entry.runs.contains_key(run_name))
        .map(|entry| entry.project_root.clone())
}

async fn get_logs(
    Path(run_name): Path<String>,
    Query(q): Query<LogQuery>,
) -> Result<Json<LogResponse>, StatusCode> {
    validate_run_name(&run_name)?;

    let db = open_db()?;
    let registry = db.registry().map_err(|e| {
        warn!("failed to load registry for logs: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let project_root = find_project_for_run(&registry, &run_name).ok_or(StatusCode::NOT_FOUND)?;

    // Resolve the run instance: an explicit id prefix, "all" for the old
    // interleaved scope, or (default) the environment's latest run.
    let run_state = match q.run_id.as_deref() {
        Some(prefix) if prefix != "all" => db
            .get_run_by_id_prefix(&project_root, prefix)
            .map_err(|e| {
                warn!("failed to resolve run id for logs: {e}");
                StatusCode::BAD_REQUEST
            })?
            .ok_or(StatusCode::NOT_FOUND)?,
        _ => db
            .get_run(&project_root, &run_name)
            .map_err(|e| {
                warn!("failed to load run state for logs: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .ok_or(StatusCode::NOT_FOUND)?,
    };
    let run_scope: Option<String> = match q.run_id.as_deref() {
        Some("all") => None,
        _ => Some(run_state.run_id.to_string()),
    };
    // Scope log queries by the RESOLVED run's environment name — an explicit
    // run_id prefix may belong to a different environment than the path
    // segment, and a mismatched (name, run_id) pair matches zero rows.
    let run_name = run_state.name.clone();

    let lines_limit = q.lines.clamp(1, 5000);
    let include_server = q.source == "all" || q.source == "server";
    let include_client = q.source == "all" || q.source == "client";
    let include_internal = q.source == "all" || q.source == "internal" || q.source == "veld";
    let mut nodes = Vec::new();

    let tail = |node: Option<&str>, variant: Option<&str>, stream: LogStream| {
        let filter = LogFilter {
            node: node.map(str::to_owned),
            variant: variant.map(str::to_owned),
            streams: Some(vec![stream.as_str()]),
            run_id: run_scope.clone(),
        };
        db.tail_logs(&project_root, &run_name, &filter, lines_limit)
            .map(|rows| {
                rows.into_iter()
                    .map(|r| format!("[{}] {}", r.ts, r.line))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default()
    };

    // Internal (veld daemon) log — not per-node, shown as _veld:internal.
    if include_internal {
        let lines = tail(None, None, LogStream::Internal);
        if !lines.is_empty() {
            nodes.push(NodeLogs {
                node: "_veld".to_owned(),
                variant: "internal".to_owned(),
                source: "internal".to_owned(),
                lines,
            });
        }
    }

    for ns in run_state.nodes.values() {
        if let Some(ref filter) = q.node {
            if ns.node_name != *filter {
                continue;
            }
        }

        if include_server {
            nodes.push(NodeLogs {
                node: ns.node_name.clone(),
                variant: ns.variant.clone(),
                source: "server".to_owned(),
                lines: tail(Some(&ns.node_name), Some(&ns.variant), LogStream::Server),
            });
        }

        if include_client {
            nodes.push(NodeLogs {
                node: ns.node_name.clone(),
                variant: ns.variant.clone(),
                source: "client".to_owned(),
                lines: tail(Some(&ns.node_name), Some(&ns.variant), LogStream::Client),
            });
        }
    }

    nodes.sort_by(|a, b| a.node.cmp(&b.node).then_with(|| a.source.cmp(&b.source)));
    Ok(Json(LogResponse { nodes }))
}

// ---------------------------------------------------------------------------
// CSRF protection
// ---------------------------------------------------------------------------

/// Check that a mutating request has the `X-Veld-Request` header.
/// Browsers won't send custom headers in cross-origin simple requests,
/// forcing a CORS preflight that is blocked (no Access-Control-Allow-Origin).
pub(super) fn check_csrf(headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    if headers.get("x-veld-request").is_some() {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// Validate that a run name contains only safe characters.
pub(super) fn validate_run_name(name: &str) -> Result<(), StatusCode> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Open terminal
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OpenTerminalBody {
    path: String,
}

async fn open_terminal(
    headers: axum::http::HeaderMap,
    Json(body): Json<OpenTerminalBody>,
) -> StatusCode {
    if let Err(s) = check_csrf(&headers) {
        return s;
    }

    // Validate the path belongs to a registered project.
    let registry = match open_db().and_then(|db| {
        db.registry().map_err(|e| {
            warn!("failed to load registry: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
    }) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let path = std::path::Path::new(&body.path);
    if !registry.projects.values().any(|e| e.project_root == path) {
        return StatusCode::FORBIDDEN;
    }

    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-a")
            .arg("Terminal")
            .arg(&body.path)
            .spawn()
    } else {
        // Try common Linux terminal emulators with working-directory support.
        std::process::Command::new("x-terminal-emulator")
            .arg("--working-directory")
            .arg(&body.path)
            .spawn()
            .or_else(|_| {
                std::process::Command::new("gnome-terminal")
                    .arg("--working-directory")
                    .arg(&body.path)
                    .spawn()
            })
            .or_else(|_| {
                std::process::Command::new("xterm")
                    .arg("-e")
                    .arg(format!(
                        "cd '{}' && $SHELL",
                        body.path.replace('\'', "'\\''")
                    ))
                    .spawn()
            })
    };

    match result {
        Ok(mut child) => {
            // Reap child in background to avoid zombies.
            tokio::spawn(async move {
                let _ = child.wait();
            });
            StatusCode::NO_CONTENT
        }
        Err(e) => {
            warn!("failed to open terminal at {}: {e}", body.path);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

// ---------------------------------------------------------------------------
// Stop / Restart
// ---------------------------------------------------------------------------

async fn stop_environment(
    headers: axum::http::HeaderMap,
    Path(run_name): Path<String>,
) -> StatusCode {
    if let Err(s) = check_csrf(&headers) {
        return s;
    }
    if let Err(s) = validate_run_name(&run_name) {
        return s;
    }
    run_veld_command(&run_name, "stop")
}

async fn restart_environment(
    headers: axum::http::HeaderMap,
    Path(run_name): Path<String>,
) -> StatusCode {
    if let Err(s) = check_csrf(&headers) {
        return s;
    }
    if let Err(s) = validate_run_name(&run_name) {
        return s;
    }
    run_veld_command(&run_name, "restart")
}

#[derive(Deserialize)]
struct ActionBody {
    /// The action name (must match an action configured on a node).
    action: String,
    /// Optional node to disambiguate when several nodes define the action.
    #[serde(default)]
    node: Option<String>,
}

/// Run a node-defined action by delegating to `veld action <name>`, which
/// reads the live outputs and shells out. Any credentials stay server-side —
/// the daemon hands off to the CLI; the browser only ever sent a name.
async fn run_action(
    headers: axum::http::HeaderMap,
    Path(run_name): Path<String>,
    Json(body): Json<ActionBody>,
) -> StatusCode {
    if let Err(s) = check_csrf(&headers) {
        return s;
    }
    if let Err(s) = validate_run_name(&run_name) {
        return s;
    }
    if !is_safe_identifier(&body.action) {
        return StatusCode::BAD_REQUEST;
    }
    if let Some(ref node) = body.node {
        if !is_safe_identifier(node) {
            return StatusCode::BAD_REQUEST;
        }
    }

    let registry = match open_db().and_then(|db| {
        db.registry().map_err(|e| {
            warn!("failed to load registry for action: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
    }) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let project_root = match find_project_for_run(&registry, &run_name) {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND,
    };

    // Only spawn actions that actually exist in the project's config.
    let cfg = load_project_config(&project_root);
    // Confirm the action name is configured on some node (the CLI re-validates
    // the optional --node filter and output availability when it runs).
    let action_defined = cfg
        .as_ref()
        .map(|c| {
            c.nodes
                .values()
                .flat_map(|n| n.actions.iter().flatten())
                .any(|a| a.name == body.action)
        })
        .unwrap_or(false);
    if !action_defined {
        return StatusCode::NOT_FOUND;
    }

    let mut args = vec![
        "action".to_owned(),
        body.action.clone(),
        "--name".to_owned(),
        run_name.clone(),
    ];
    if let Some(node) = &body.node {
        args.push("--node".to_owned());
        args.push(node.clone());
    }
    spawn_veld(&project_root, &args)
}

/// Stop / restart helper: spawn `veld <action> --name <run>`.
fn run_veld_command(run_name: &str, action: &str) -> StatusCode {
    let registry = match open_db().and_then(|db| {
        db.registry().map_err(|e| {
            warn!("failed to load registry for {action}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
    }) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let project_root = match find_project_for_run(&registry, run_name) {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND,
    };
    spawn_veld(
        &project_root,
        &[action.to_owned(), "--name".to_owned(), run_name.to_owned()],
    )
}

/// Spawn `veld <args...>` in the project directory via a login shell. The
/// project_root is looked up from the GlobalRegistry (never supplied by the
/// client) to prevent directory traversal; every argument is shell-escaped.
pub(super) fn spawn_veld(project_root: &std::path::Path, args: &[String]) -> StatusCode {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let escaped_args: Vec<String> = args.iter().map(|a| shell_escape(a)).collect();
    // Resolve the veld binary as THIS daemon's sibling (current_exe), by
    // absolute path — a bare `veld` in the login shell resolves via PATH to
    // the INSTALLED binary, which would then operate a dev instance's
    // DB/daemon (inherited env) and fail closed on a schema-ahead dev DB.
    // The login shell stays: veld's own children need the user's full PATH.
    let veld_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("veld")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "veld".to_owned());
    let cmd = format!(
        "cd {} && {} {}",
        shell_escape(&project_root.to_string_lossy()),
        shell_escape(&veld_bin),
        escaped_args.join(" "),
    );

    match std::process::Command::new(&shell)
        .args(["-l", "-c", &cmd])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Reap child in background to avoid zombies.
            tokio::spawn(async move {
                let _ = child.wait();
            });
            StatusCode::ACCEPTED
        }
        Err(e) => {
            warn!("failed to run veld {}: {e}", args.join(" "));
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Allow only conservative identifier characters for action/node names that
/// originate from the browser, as defence in depth on top of shell escaping.
pub(super) fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Simple single-quote shell escaping.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
