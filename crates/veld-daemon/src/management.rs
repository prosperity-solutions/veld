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
use veld_core::user_path::cached_user_path;

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
        // Every route below that takes a `{run}` segment MUST also take the
        // project scope — a run name alone is ambiguous across projects. See
        // [`RunScope`]; `handler_guards` pins that each one 400s without it.
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

/// Load a project's root config for action lookup. Returns `None` if the
/// project has no readable config — the dashboard then simply shows no actions.
fn load_project_config(project_root: &std::path::Path) -> Option<config::VeldConfig> {
    config::parse_config(&config::root_config_in(project_root)?).ok()
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
    /// See [`RunScope`] — same parameter, folded into this struct so the
    /// handler takes a single `Query` extractor. Two `Query`s over one query
    /// string only work while neither struct denies unknown fields, and this
    /// daemon deliberately uses `deny_unknown_fields` elsewhere; don't
    /// reintroduce the coupling.
    project_root: String,
    #[serde(default = "default_lines")]
    lines: usize,
    node: Option<String>,
    /// Filter by source: "all" (default), "server" (node output), "client",
    /// "setup" (project setup/teardown steps, alias "teardown"), or "internal"
    /// (alias "veld"). Mirrors `veld logs --source`.
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

/// The project half of a run address, taken as a query parameter by every
/// name-addressed endpoint.
///
/// **A run name alone does not identify a run.** Environments are unique per
/// project (`UNIQUE(project_root, name)`), not globally: two repos both checked
/// out on `main` each default to an environment named `main`, and the desktop
/// start endpoint derives the run name from the worktree alias, which
/// `unique_alias` de-duplicates only within one repo. Resolving a bare name
/// against the whole registry therefore picked whichever project a `HashMap`
/// iteration happened to yield first — stopping one repo's `main` could tear
/// down another's. Callers send the `project_root` they read from
/// `/api/environments`; it is the same value `/api/stats` keys by.
#[derive(Deserialize)]
pub(super) struct RunScope {
    project_root: String,
}

/// Resolve a `(project_root, run_name)` pair to the project root as the
/// registry records it, or 404 when that project does not run that name.
///
/// The returned path is the registry's own `PathBuf`, never the caller's
/// string — it becomes the working directory of a spawned `veld` process, so
/// only a project the daemon already tracks may be named (the rule
/// `open_terminal` enforces for its own path argument).
pub(super) fn resolve_run_project(
    registry: &GlobalRegistry,
    project_root: &str,
    run_name: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    let requested = std::path::Path::new(project_root);
    registry
        .projects
        .values()
        .find(|entry| entry.project_root == requested && entry.runs.contains_key(run_name))
        .map(|entry| entry.project_root.clone())
        .ok_or_else(|| {
            // The handlers answer with bare status codes, so a 404 alone can't
            // distinguish "project not tracked" from "project doesn't run that
            // name" — and a stale client sending the wrong root would otherwise
            // fail completely silently.
            warn!("no run '{run_name}' in project '{project_root}'");
            StatusCode::NOT_FOUND
        })
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

    let project_root = resolve_run_project(&registry, &q.project_root, &run_name)?;

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
    // Project setup/teardown steps: run-level rows that carry a pseudo-node
    // (`setup`/`teardown`) rather than a real one, so they are not reachable
    // through the per-node loop below.
    let include_setup = q.source == "all" || q.source == "setup" || q.source == "teardown";
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

    // Project setup/teardown output — shown as _veld:setup, like the internal
    // stream, since it belongs to the run rather than to any one node.
    // Not gated on `q.node`: `setup` is a run-level stream, so a node filter
    // does not apply to it — the same rule `veld logs` follows for the run-level
    // streams (see `crates/veld/src/commands/logs.rs`, run-level filter branch).
    if include_setup {
        // Labelled per step: the rows of every step share one section, and
        // `setup:install` vs `teardown:compose-down` is the only thing telling
        // a reader which lines came from where.
        let filter = LogFilter {
            node: None,
            variant: None,
            streams: Some(vec![LogStream::Setup.as_str()]),
            run_id: run_scope.clone(),
        };
        let lines: Vec<String> = db
            .tail_logs(&project_root, &run_name, &filter, lines_limit)
            .map(|rows| {
                rows.into_iter()
                    .map(|r| match (&r.node, &r.variant) {
                        (Some(node), Some(variant)) => {
                            format!("[{}] [{node}:{variant}] {}", r.ts, r.line)
                        }
                        // Every writer of this stream labels its rows; this is
                        // for a row some other version of veld left behind.
                        _ => format!("[{}] {}", r.ts, r.line),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !lines.is_empty() {
            nodes.push(NodeLogs {
                node: "_veld".to_owned(),
                variant: "setup".to_owned(),
                source: "setup".to_owned(),
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

    // tokio::process (NOT std): the reaper must await the exit instead of
    // blocking a core runtime worker — a Linux terminal emulator child lives
    // as long as the terminal window.
    let result = if cfg!(target_os = "macos") {
        tokio::process::Command::new("open")
            .arg("-a")
            .arg("Terminal")
            .arg(&body.path)
            .spawn()
    } else {
        // Try common Linux terminal emulators with working-directory support.
        tokio::process::Command::new("x-terminal-emulator")
            .arg("--working-directory")
            .arg(&body.path)
            .spawn()
            .or_else(|_| {
                tokio::process::Command::new("gnome-terminal")
                    .arg("--working-directory")
                    .arg(&body.path)
                    .spawn()
            })
            .or_else(|_| {
                tokio::process::Command::new("xterm")
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
            // Reap child in background to avoid zombies (async — never
            // blocks a worker).
            tokio::spawn(async move {
                let _ = child.wait().await;
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
    Query(scope): Query<RunScope>,
) -> StatusCode {
    if let Err(s) = check_csrf(&headers) {
        return s;
    }
    if let Err(s) = validate_run_name(&run_name) {
        return s;
    }
    run_veld_command(&scope, &run_name, "stop").await
}

async fn restart_environment(
    headers: axum::http::HeaderMap,
    Path(run_name): Path<String>,
    Query(scope): Query<RunScope>,
) -> StatusCode {
    if let Err(s) = check_csrf(&headers) {
        return s;
    }
    if let Err(s) = validate_run_name(&run_name) {
        return s;
    }
    run_veld_command(&scope, &run_name, "restart").await
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
    Query(scope): Query<RunScope>,
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
    let project_root = match resolve_run_project(&registry, &scope.project_root, &run_name) {
        Ok(p) => p,
        Err(code) => return code,
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
    spawn_veld(&project_root, &args).await
}

/// Stop / restart helper: spawn `veld <action> --name <run>` in the project
/// the caller scoped the run to. The `--name` argument stays name-based (that
/// is the CLI's own contract); it is the *working directory* that disambiguates
/// which project's `main` gets stopped.
async fn run_veld_command(scope: &RunScope, run_name: &str, action: &str) -> StatusCode {
    let registry = match open_db().and_then(|db| {
        db.registry().map_err(|e| {
            warn!("failed to load registry for {action}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
    }) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let project_root = match resolve_run_project(&registry, &scope.project_root, run_name) {
        Ok(p) => p,
        Err(code) => return code,
    };
    spawn_veld(
        &project_root,
        &[action.to_owned(), "--name".to_owned(), run_name.to_owned()],
    )
    .await
}

/// Longest stderr tail kept from a failed spawned command. A `veld start`
/// streams its whole progress to stderr and only the end of it says why the
/// command failed.
const STDERR_TAIL_BYTES: u64 = 4096;

/// Create the file a spawned command's stderr is redirected to.
///
/// Under `~/.veld/spawn-logs` (owner-only), never a world-writable temp
/// directory, and with `create_new` so an existing path — including a symlink
/// another local user planted — is a hard failure rather than a write-through.
/// `None` means "no capture": the caller falls back to `Stdio::null()`, which is
/// what this always did before, so a failure here never blocks the command.
fn spawn_stderr_file() -> Option<(std::fs::File, std::path::PathBuf)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let dir = spawn_log_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    // Timestamp as well as pid+counter: a daemon killed between spawn and reap
    // leaves its file behind (the reaper died with it), and a pid+counter alone
    // would collide with that leftover after pid reuse — `create_new` would then
    // fail and capture would be silently off for the rest of that daemon's life.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!(
        "{}-{}-{}.err",
        std::process::id(),
        stamp,
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    // Owner-only: a spawned command's stderr can carry whatever its output
    // contains, and the database next to it is 0o600 for the same reason.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        opts.mode(0o600);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let file = opts.open(&path).ok()?;
    Some((file, path))
}

/// Where a spawned command's stderr is captured. Owner-only, alongside the
/// daemon's socket and database rather than in a world-writable temp directory.
fn spawn_log_dir() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".veld").join("spawn-logs"))
}

/// Delete leftover capture files at daemon start.
///
/// The reaper task unlinks its own file, but it dies with the daemon — a SIGKILL
/// or a `veld update` between spawn and reap leaves the file named on disk, and
/// nothing else in veld collects this directory (`veld gc` prunes per-project
/// logs, not this).
///
/// Only files whose owning daemon is **gone** are removed. `$HOME` is shared by
/// every veld instance (a source-built dev daemon runs alongside the installed
/// one — see `veld_core::instance`), so a blanket sweep would delete the other
/// daemon's in-flight capture and lose its diagnostics. The pid is read back out
/// of the filename for exactly this.
pub fn sweep_spawn_logs() {
    let Some(dir) = spawn_log_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut swept = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "err") {
            continue;
        }
        // `<pid>-<stamp>-<seq>.err`. An unparseable name is not ours to judge.
        let owner = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('-').next())
            .and_then(|pid| pid.parse::<i32>().ok());
        let Some(pid) = owner else { continue };
        if pid > 0 && unsafe { libc::kill(pid, 0) } == 0 {
            continue; // that daemon is still running — its file, not ours.
        }
        if std::fs::remove_file(&path).is_ok() {
            swept += 1;
        }
    }
    if swept > 0 {
        warn!(count = swept, "removed orphaned spawn-log files");
    }
}

/// The last [`STDERR_TAIL_BYTES`] of a spawned command's captured stderr.
fn stderr_tail(path: &std::path::Path) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut file = file.take(STDERR_TAIL_BYTES);
    if len > STDERR_TAIL_BYTES
        && file
            .get_mut()
            .seek(SeekFrom::End(-(STDERR_TAIL_BYTES as i64)))
            .is_err()
    {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    // Lossy: a tail can start mid-character, and this is a diagnostic.
    String::from_utf8_lossy(&buf).trim().to_owned()
}

/// Spawn `veld <args...>` in the project directory with the user's login-shell
/// `PATH`. The project_root is looked up from the GlobalRegistry (never
/// supplied by the client) to prevent directory traversal.
pub(super) async fn spawn_veld(project_root: &std::path::Path, args: &[String]) -> StatusCode {
    // Resolve the veld binary as THIS daemon's sibling (current_exe), by
    // absolute path — a bare `veld` would resolve via PATH to the INSTALLED
    // binary, which would then operate a dev instance's DB/daemon (inherited
    // env) and fail closed on a schema-ahead dev DB.
    let veld_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("veld")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "veld".to_owned());
    // AGENTS.md: a daemon-spawned command must inherit the user's login-shell
    // PATH, and `veld start` runs every command a node declares. This used to
    // rely on `$SHELL -l -c`, which does not get there: a login shell sources
    // `.zprofile` but NOT `.zshrc`, and `.zshrc` is where version managers
    // (nvm, fnm, rbenv) and `brew shellenv` put their bin directories on most
    // machines. Under launchd the daemon's own PATH is the bare service one,
    // so a UI- or Desktop-initiated `veld start` resolved node commands
    // against `/usr/bin:/bin:/usr/sbin:/sbin` plus whatever `.zprofile` added
    // and reported `sh: npx: command not found` for a config that works in the
    // user's terminal.
    //
    // The login shell is gone rather than kept alongside the injection: on
    // Debian `/etc/profile` *overwrites* PATH unconditionally, so a login
    // shell would discard the injected value and leave this broken on Linux,
    // and `export PATH=…` inside the command string is not fish syntax. Only
    // PATH is inherited, never the rest of the login environment — the same
    // contract the monitor's liveness probes and `SecretSource::Command` hold.
    // The visible consequence: a node command that needs a variable exported
    // only from `.zprofile`/`.profile` used to see it here and no longer does.
    // That is a real asymmetry with the CLI path, where a macOS terminal's zsh
    // *is* a login shell and those exports are simply in the environment veld
    // inherits — so a node needing one must declare it in its `env` rather than
    // rely on who started the run.
    //
    // Dropping the shell also drops the shell-escaping of a client-supplied
    // run name; arguments now reach the binary as argv.
    //
    // Cached (60s) rather than resolved per call: this runs inside the
    // stop/restart/action/start handlers, whose UI `fetch` calls carry no
    // timeout, and a stalled rc file would otherwise hang the click for the
    // full 10s resolution budget.
    let path_env = cached_user_path().await;
    spawn_veld_in(&veld_bin, &path_env, project_root, args)
}

/// The spawn itself, with every ambient input (the veld binary, the resolved
/// `PATH`) passed in so a test can supply its own. Separate from
/// [`spawn_veld`] only for that reason.
fn spawn_veld_in(
    veld_bin: &str,
    path_env: &str,
    project_root: &std::path::Path,
    args: &[String],
) -> StatusCode {
    // tokio::process (NOT std): `veld stop` can call back into THIS daemon's
    // HTTP API (share teardown) — a synchronous child.wait() here parks a
    // core runtime worker until the child exits, and with the child waiting
    // on the daemon that's a circular wait: the daemon plays dead until the
    // child is killed (observed live, 2026-07-27).
    // stderr is captured, not discarded: the reply is always `202 ACCEPTED`
    // (the child outlives this request), so a command that fails outright —
    // `veld start` refusing a hostname another project already serves, a
    // missing preset, an unparseable veld.json — would otherwise leave no
    // trace anywhere and the UI would just never show a run.
    //
    // Captured to a FILE, not a pipe. A pipe would have to be drained to keep
    // from blocking the child, would not reach EOF when the CLI exits
    // (`veld action` inherits stderr, so anything it backgrounds holds the
    // write end open — `process::run_command` now captures both of a step's own
    // pipes, but the CLI's stderr, which this file is, is still inherited by
    // whatever a step leaves running), and closing the read end on such a
    // survivor would hand it EPIPE/SIGPIPE and kill it — a process the user
    // started. A file blocks nobody, stays writable for any descendant that
    // inherited it, and needs no reader at all.
    let (stderr_sink, stderr_path) = match spawn_stderr_file() {
        Some((file, path)) => (std::process::Stdio::from(file), Some(path)),
        None => (std::process::Stdio::null(), None),
    };
    match tokio::process::Command::new(veld_bin)
        .args(args)
        .current_dir(project_root)
        .env("PATH", path_env)
        .stdout(std::process::Stdio::null())
        .stderr(stderr_sink)
        .spawn()
    {
        Ok(mut child) => {
            let label = args.join(" ");
            // Owned: the reaper outlives this call, so it cannot borrow the
            // caller's PATH.
            let path_env = path_env.to_owned();
            // Reap the child in the background so nothing waits on it here (a
            // synchronous wait would deadlock against a `veld stop` that calls
            // back into this daemon). The tail read and unlink below are blocking
            // `std::fs`, but bounded to one 4 KiB read of a local file.
            tokio::spawn(async move {
                let waited = child.wait().await;
                // Logged whether or not capture is available: an exit code with no
                // stderr is still the difference between a visible failure and a
                // `202 ACCEPTED` that leaves no trace anywhere.
                if let Ok(status) = &waited {
                    if !status.success() {
                        warn!(
                            command = %label,
                            code = status.code().unwrap_or(-1),
                            // The bug class this spawn path exists for is
                            // "command not found", so the PATH the child was
                            // given is the first thing worth knowing.
                            path = %path_env,
                            stderr = %stderr_path.as_deref().map(stderr_tail).unwrap_or_default(),
                            "spawned veld command failed"
                        );
                    }
                }
                if let Some(path) = stderr_path {
                    // A descendant still holding the fd keeps writing to the
                    // now-nameless inode; its blocks are reclaimed when that
                    // descendant exits, so the growth is bounded by its lifetime
                    // and invisible to the directory. A daemon killed before this
                    // runs leaves the file named — `sweep_spawn_logs` collects
                    // those at the next start.
                    let _ = std::fs::remove_file(&path);
                }
            });
            StatusCode::ACCEPTED
        }
        // Without a shell in between, this arm now catches what the shell used
        // to report as a nonzero exit with captured stderr: a project root that
        // vanished since the registry lookup, and a veld binary that isn't
        // executable. Structured like the exit-code warn above so both failure
        // classes are one query, not two log shapes.
        Err(e) => {
            warn!(
                command = %args.join(" "),
                bin = %veld_bin,
                cwd = %project_root.display(),
                path = %path_env,
                error = %e,
                "failed to spawn veld command"
            );
            // No child means no reaper task, so unlink here or the capture file
            // leaks for the daemon's whole uptime: `sweep_spawn_logs` spares
            // files whose owning pid is still alive, and that pid is ours. The
            // old shell-wrapped spawn never reached this arm — a failed `cd`
            // happened inside a successfully spawned shell — so this cleanup
            // path is new with the direct spawn.
            if let Some(path) = stderr_path {
                let _ = std::fs::remove_file(&path);
            }
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Allow only conservative identifier characters for action/node names that
/// originate from the browser. Kept as defence in depth now that arguments
/// reach the CLI as argv rather than through a shell string: no shell parses
/// these, but the charset still keeps whitespace, quotes and `=` out of an
/// argv element. It does **not** reject a leading `-` — `--force` passes —
/// which is unchanged from the shell-escaping era (single-quoting produced the
/// same argv element); what stops a flag-shaped value is clap refusing it as
/// the value of `--name`, plus the action-exists check above.
pub(super) fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use veld_core::state::{RegistryEntry, RegistryRunInfo, RunStatus};

    /// Two projects that each run an environment called `main` — the state the
    /// desktop UI produces from two repos both checked out on `main`.
    fn colliding_registry() -> GlobalRegistry {
        let entry = |root: &str, name: &str, run: &str| {
            let mut runs = HashMap::new();
            runs.insert(
                run.to_owned(),
                RegistryRunInfo {
                    run_id: uuid::Uuid::new_v4(),
                    name: run.to_owned(),
                    status: RunStatus::Running,
                    urls: HashMap::new(),
                },
            );
            (
                root.to_owned(),
                RegistryEntry {
                    project_root: std::path::PathBuf::from(root),
                    project_name: name.to_owned(),
                    runs,
                },
            )
        };
        GlobalRegistry {
            projects: HashMap::from([
                entry("/repos/alpha", "alpha", "main"),
                entry("/repos/beta", "beta", "main"),
            ]),
        }
    }

    #[test]
    fn run_is_resolved_within_the_named_project_only() {
        let reg = colliding_registry();
        // The whole point: `main` resolves to whichever project the caller
        // named, deterministically. A registry-order-dependent first match
        // would let a stop on beta tear down alpha.
        assert_eq!(
            resolve_run_project(&reg, "/repos/alpha", "main").unwrap(),
            std::path::PathBuf::from("/repos/alpha")
        );
        assert_eq!(
            resolve_run_project(&reg, "/repos/beta", "main").unwrap(),
            std::path::PathBuf::from("/repos/beta")
        );
    }

    /// Router-level guards. These paths reject during extraction or on the
    /// CSRF check, i.e. before any database access, so they run against the
    /// real router with no test DB.
    mod handler_guards {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        fn get(uri: &str) -> Request<Body> {
            Request::builder().uri(uri).body(Body::empty()).unwrap()
        }

        fn post(uri: &str, csrf: bool) -> Request<Body> {
            let mut b = Request::builder().method("POST").uri(uri);
            if csrf {
                b = b.header("x-veld-request", "1");
            }
            b.body(Body::empty()).unwrap()
        }

        async fn status(req: Request<Body>) -> StatusCode {
            super::super::routes().oneshot(req).await.unwrap().status()
        }

        #[tokio::test]
        async fn run_addressed_routes_require_a_project_scope() {
            // A name-only request must not fall back to "whichever project
            // holds this name" — that is the bug. It is rejected during
            // extraction, so nothing is spawned and no DB is touched.
            // Keep in sync with the name-addressed routes in routes().
            assert_eq!(
                status(post("/api/environments/main/stop", true)).await,
                StatusCode::BAD_REQUEST
            );
            assert_eq!(
                status(post("/api/environments/main/restart", true)).await,
                StatusCode::BAD_REQUEST
            );
            assert_eq!(
                status(post("/api/environments/main/action", true)).await,
                StatusCode::BAD_REQUEST
            );
            assert_eq!(
                status(get("/api/logs/main?lines=500")).await,
                StatusCode::BAD_REQUEST
            );
        }

        #[tokio::test]
        async fn the_csrf_gate_still_applies_with_a_scope_present() {
            // The scope is extracted before the handler body runs, so a
            // request that satisfies extraction must still fail the CSRF
            // check rather than reaching the spawn.
            for uri in [
                "/api/environments/main/stop?project_root=/repos/alpha",
                "/api/environments/main/restart?project_root=/repos/alpha",
            ] {
                assert_eq!(
                    status(post(uri, false)).await,
                    StatusCode::FORBIDDEN,
                    "{uri}"
                );
            }
        }

        #[tokio::test]
        async fn logs_still_validates_its_other_query_fields() {
            // `project_root` lives in `LogQuery` alongside the pre-existing
            // fields rather than in a second `Query` extractor; folding it in
            // must not have made the rest of the struct optional.
            assert_eq!(
                status(get(
                    "/api/logs/main?project_root=/repos/alpha&lines=notanumber"
                ))
                .await,
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn unknown_project_or_run_is_not_found() {
        let reg = colliding_registry();
        // A project the daemon doesn't track can't become a spawn cwd, even
        // when the run name exists elsewhere.
        assert_eq!(
            resolve_run_project(&reg, "/etc", "main"),
            Err(StatusCode::NOT_FOUND)
        );
        // Known project, but it doesn't run that name.
        assert_eq!(
            resolve_run_project(&reg, "/repos/alpha", "release"),
            Err(StatusCode::NOT_FOUND)
        );
        // Empty scope is not a wildcard.
        assert_eq!(
            resolve_run_project(&reg, "", "main"),
            Err(StatusCode::NOT_FOUND)
        );
    }

    /// Pins the load-bearing `.env("PATH", path_env)` in `spawn_veld_in`, the
    /// whole point of this spawn path: a stand-in `veld` records the PATH and
    /// cwd it was invoked with, and both must be the injected ones — not the
    /// daemon's own (bare, under launchd) environment, which is what produced
    /// `sh: npx: command not found` for node commands. The binary path and PATH
    /// are arguments rather than process env so the test never touches
    /// `set_var` (an env data race under multithreaded `cargo test`).
    /// Serialises the two tests that spawn through `spawn_veld_in`. They share
    /// one real directory (`~/.veld/spawn-logs`) and identify their own capture
    /// file by set difference, which only holds if no sibling is creating one
    /// concurrently — `cargo test` runs them on parallel threads by default.
    #[cfg(unix)]
    fn capture_dir_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// Names of this process's own capture files currently in the shared
    /// `~/.veld/spawn-logs` (the filename carries the creating pid, so a real
    /// daemon writing to the same directory is excluded).
    #[cfg(unix)]
    fn own_capture_files() -> std::collections::HashSet<String> {
        let mine = format!("{}-", std::process::id());
        spawn_log_dir()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with(&mine))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_veld_gets_the_injected_path_and_cwd() {
        use std::os::unix::fs::PermissionsExt;

        let _serialised = capture_dir_lock().lock().await;
        let before = own_capture_files();
        let bin_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let receipt = bin_dir.path().join("receipt");

        let fake_veld = bin_dir.path().join("veld");
        std::fs::write(
            &fake_veld,
            format!(
                "#!/bin/sh\nprintf '%s\\n%s\\n%s' \"$PATH\" \"$(pwd)\" \"$*\" > {}\n",
                receipt.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_veld, std::fs::Permissions::from_mode(0o755)).unwrap();

        let injected = format!("{}:/usr/bin:/bin", bin_dir.path().display());
        assert_eq!(
            spawn_veld_in(
                &fake_veld.to_string_lossy(),
                &injected,
                project.path(),
                &["stop".to_owned(), "--name".to_owned(), "main".to_owned()],
            ),
            StatusCode::ACCEPTED
        );

        // Fire-and-forget: the handler returns before the child runs, so poll
        // for the receipt rather than assuming it is already there.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let recorded = loop {
            match std::fs::read_to_string(&receipt) {
                Ok(s) if s.lines().count() >= 3 => break s,
                _ => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "spawned veld never wrote its receipt"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        };
        let mut lines = recorded.lines();
        assert_eq!(
            lines.next().unwrap(),
            injected,
            "child PATH is not injected"
        );
        // macOS hands out /var symlinks for temp dirs; `pwd` in the child
        // reports the resolved path, so compare canonicalised.
        assert_eq!(
            std::fs::canonicalize(lines.next().unwrap()).unwrap(),
            std::fs::canonicalize(project.path()).unwrap(),
            "child cwd is not the project root"
        );
        // Arguments reach the CLI as argv, unquoted — no shell in between.
        assert_eq!(lines.next().unwrap(), "stop --name main");

        // Wait for this spawn's capture file to be unlinked before returning.
        // The reaper that does it is a `tokio::spawn` on this test's runtime,
        // which is dropped when the test function returns — so without waiting,
        // the task is simply cancelled and the file is orphaned in the real
        // `~/.veld/spawn-logs` on every run.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !own_capture_files().is_subset(&before) {
            assert!(
                std::time::Instant::now() < deadline,
                "spawn stderr capture file was never reaped"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// A project root that no longer exists must fail the request, not answer
    /// `202 ACCEPTED` for a run that will never appear. The old shell-wrapped
    /// spawn always started successfully and let `cd … &&` short-circuit
    /// inside the child, so this failure was visible only in the daemon's own
    /// stderr capture; `current_dir` makes it synchronous.
    #[cfg(unix)]
    #[tokio::test]
    async fn vanished_project_root_is_an_error_not_an_accept() {
        let _serialised = capture_dir_lock().lock().await;
        let gone = tempfile::tempdir().unwrap();
        let path = gone.path().to_path_buf();
        drop(gone);

        let before = own_capture_files();

        assert_eq!(
            spawn_veld_in("/bin/echo", "/usr/bin:/bin", &path, &["stop".to_owned()]),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        // The capture file created for this spawn must be unlinked: there is no
        // child, so no reaper task will do it, and `sweep_spawn_logs`
        // deliberately spares files belonging to a live pid — ours. Checked
        // once, not polled: the unlink on that arm is synchronous, so anything
        // new here is a leak. By set difference rather than by count, so the
        // sibling test's concurrent file (same pid prefix, different name)
        // neither fails this nor masks a real leak.
        assert!(
            own_capture_files().difference(&before).next().is_none(),
            "spawn stderr capture file leaked on the error path"
        );
    }
}
