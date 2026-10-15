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
use veld_core::state::{GlobalRegistry, NodeState, NodeStatus, RunStatus, StartOrigin};

const DASHBOARD_HTML: &str = include_str!("../assets/management-ui.html");

/// The v2 management UI (React, built by `ui/` via build.rs into a single
/// self-contained HTML file). Served under /ide (worktree mode); a future
/// runs mode reaches parity with
/// the v1 dashboard above; Veld Desktop wraps this page.
const IDE_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/management-ui-ide.html"));

/// Open the central database, mapping failures to a 500.
///
/// **Every router's "cannot open the database" path funnels here**, and the error
/// is classified on the way past — the same reasoning as `desktop::db_err`'s
/// downcast, at the other choke point. These call sites discard the error (they
/// map it straight to a status), so without this a database that has stopped
/// opening at all would be invisible to `dbhealth` except through the probe, and
/// the whole point of folding in what other subsystems trip over is that they
/// trip over it *sooner* than the next probe.
pub(crate) fn open_db() -> Result<Db, StatusCode> {
    Db::open().map_err(|e| {
        warn!("failed to open veld database: {e}");
        crate::dbhealth::note_error(&e);
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
        // Liveness + version probe. `veld self update` polls this to confirm the
        // daemon actually restarted onto the new binary (not just that *some*
        // daemon is reachable), mirroring the helper's version check.
        .route("/api/health", get(health))
        .route("/api/environments", get(list_environments))
        .route("/api/stats", get(get_stats))
        // Scrubbable history for one node. Project-scoped like every
        // run-addressed route, but via required query fields rather than a path
        // segment — the node key (`"node:variant"`) is not URL-path-safe.
        .route("/api/stats/history", get(get_stats_history))
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
        // The database this daemon's engine drives. A CLI built against a
        // different one refuses to use this daemon rather than start and stop
        // runs it cannot see (#306's class of bug: a cargo-built CLI on the dev
        // database talking to the installed daemon).
        "db_path": veld_core::db::Db::default_path().ok(),
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
    /// every veld JSON surface (`veld orchestrator runs list --json`, `veld orchestrator status --json`).
    run_id: String,
    /// Git-style short prefix of `run_id`, for display.
    short_id: String,
    /// One-line outcome of the latest run when it has ended
    /// (e.g. "crashed (api:local pid died)").
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<String>,
    /// When this environment's latest run started (`created_at`), RFC 3339.
    /// The client sorts the Active tab by start time, so it is part of the
    /// wire payload (not just an ordering key).
    created_at: String,
    urls: HashMap<String, String>,
    nodes: Vec<NodeInfo>,
    /// What the latest run was started from — a preset name plus the expansion
    /// that name meant at start time. `None` for runs recorded before this
    /// existed. A client that wants to know whether the named preset still means
    /// the same thing compares `selections` against a fresh expansion (the
    /// desktop repo listing ships one per preset); this endpoint deliberately
    /// does not, because it never parses the project's config.
    #[serde(skip_serializing_if = "Option::is_none")]
    started_from: Option<StartOrigin>,
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
    /// Full run UUID (same contract as `veld orchestrator runs list --json`).
    run_id: String,
    /// Git-style short prefix, for display.
    short_id: String,
    status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<String>,
    /// What that run was started from (see [`RunInfo::started_from`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    started_from: Option<StartOrigin>,
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

/// One of a node's ports, as the dashboards need it.
///
/// `url` present means the port is routed (`protocol: "http"`) and can be opened;
/// absent means it is a raw `tcp` port, reachable at `hostname:port` and at
/// nothing else. The distinction is carried as a *value*, not inferred from a
/// naming convention, because rendering a raw port as a link produces a URL that
/// looks right and goes nowhere.
#[derive(Serialize)]
struct EndpointInfo {
    /// Port name as declared in `ports` — `http` for the conventional default.
    name: String,
    hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    port: u16,
    /// Whether this is the node's primary port — the one `${veld.url}` means and
    /// the one `url` above repeats. Lets a client show it first without
    /// re-deriving the rule.
    primary: bool,
}

#[derive(Serialize)]
struct NodeInfo {
    name: String,
    variant: String,
    status: NodeStatus,
    /// The primary port's URL. Kept alongside `endpoints` (where it appears
    /// again, flagged `primary`) because every existing client reads it, and a
    /// single-port node — still the common case — needs nothing more.
    url: Option<String>,
    /// Every port this node claimed, primary first. Empty for a node that
    /// declared none, and for any run recorded before per-port endpoints.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    endpoints: Vec<EndpointInfo>,
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

/// A node's ports for the dashboards, primary first.
///
/// The primary is matched **by value** against `NodeState::url`, the same rule
/// `NodeState::routed_urls` uses, so the two never disagree about which port is
/// the primary.
fn endpoint_infos(ns: &NodeState) -> Vec<EndpointInfo> {
    let mut out: Vec<EndpointInfo> = ns
        .endpoints_or_legacy()
        .iter()
        .map(|(name, e)| EndpointInfo {
            name: name.clone(),
            // Port-stripped, as `raw_addresses` and the `hosts.<name>` builtin
            // both are: a `url_template` carrying a literal port would
            // otherwise render and copy `db.test:8443:5432`.
            hostname: veld_core::url::hostname_of_url(&e.hostname).to_owned(),
            url: e.url.clone(),
            port: e.port,
            primary: ns.is_primary(e),
        })
        .collect();
    // Primary first; the rest keep the map's name order.
    out.sort_by_key(|e| !e.primary);
    out
}

/// Load a project's root config for the `/api/environments` listing. Returns
/// `None` if the project has no readable config — the dashboard then simply
/// shows no actions.
///
/// Read through the stat-validated [`config_cache`](crate::feedback_server::config_cache)
/// rather than parsed fresh: this runs for every registry entry on every poll,
/// and a fresh parse is an include-glob walk, a JSONC strip, a hash and a serde
/// pass per project. Display-only — see [`load_project_config_fresh`] for the
/// caller that acts on the result.
fn load_project_config(project_root: &std::path::Path) -> Option<config::VeldConfig> {
    crate::feedback_server::config_cache::parse_config(&config::root_config_in(project_root)?)
}

/// [`load_project_config`] parsed fresh, for the one path that *acts* on the
/// config (deciding whether an action may be spawned). One parse per explicit
/// request, so the cache buys nothing there and a fresh read keeps "never act on
/// a config that has changed" trivially true.
fn load_project_config_fresh(project_root: &std::path::Path) -> Option<config::VeldConfig> {
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

// These four handlers are synchronous all the way down — database reads and file
// reads, no awaits — and the UI polls them. Each runs on the blocking pool so a
// `Db::open()` waiting out a GC pass's write lock parks no worker (see
// `crate::offload`).
async fn list_environments() -> Result<Json<EnvironmentList>, StatusCode> {
    crate::offload::blocking(list_environments_blocking).await
}

fn list_environments_blocking() -> Result<Json<EnvironmentList>, StatusCode> {
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
                                    endpoints: if live { endpoint_infos(ns) } else { Vec::new() },
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
                                started_from: run
                                    .graph_snapshot
                                    .as_ref()
                                    .and_then(|s| s.started_from.clone()),
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
                        created_at: latest
                            .map(|l| l.created_at.to_rfc3339())
                            .unwrap_or_default(),
                        urls: if live { r.urls.clone() } else { HashMap::new() },
                        nodes,
                        started_from: latest
                            .and_then(|l| l.graph_snapshot.as_ref())
                            .and_then(|s| s.started_from.clone()),
                        history,
                    }
                })
                .collect();
            // Active (live) runs first, newest-started at the top; then ended
            // runs, last-stopped at the top. The UI relies on this order for
            // its Active|History tabs and the IDE run selector — a lexical
            // name sort made the newest run hard to find while debugging a
            // crash, which is what this order exists to fix.
            runs.sort_by(|a, b| {
                // Active first.
                let by_live = b.live.cmp(&a.live);
                if by_live != std::cmp::Ordering::Equal {
                    return by_live;
                }
                // Primary time — start for live runs, stop for ended.
                // RFC 3339 UTC strings compare chronologically.
                let a_primary = if a.live {
                    a.created_at.as_str()
                } else {
                    a.ended_at.as_deref().unwrap_or("")
                };
                let b_primary = if b.live {
                    b.created_at.as_str()
                } else {
                    b.ended_at.as_deref().unwrap_or("")
                };
                let by_primary = b_primary.cmp(a_primary);
                if by_primary != std::cmp::Ordering::Equal {
                    return by_primary;
                }
                // Deterministic tiebreakers so a poll refresh never reshuffles
                // runs that share a timestamp (ended_at can be absent): the
                // registry is a HashMap, so an `Equal` comparator would pin
                // the order to whatever (unstable) iteration order the fresh
                // registry happened to produce. `run_id` is unique → total.
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.name.cmp(&b.name))
                    .then_with(|| a.run_id.cmp(&b.run_id))
            });

            ProjectInfo {
                name: entry.project_name.clone(),
                project_root: entry.project_root.display().to_string(),
                runs,
            }
        })
        .collect();

    // Projects by name, with `project_root` as a deterministic tiebreak: two
    // worktrees of one repo share the project *name* (`veld`), so a name-only
    // sort ties and falls back to the registry HashMap's iteration order,
    // which changes between polls and reshuffles their runs.
    projects.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.project_root.cmp(&b.project_root))
    });

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
    /// Resident memory in bytes, summed across the process tree. Double-counts
    /// pages shared inside the tree — `footprint` is the figure to show.
    mem: u64,
    /// Number of live processes in the tree.
    procs: u32,
    /// Recent **footprint** samples (bytes), oldest-first, for the sparkline.
    /// Footprint rather than RSS so the collapsed sparkline and the expanded
    /// chart plot the same quantity — two charts of one node that disagree are
    /// worse than one chart.
    spark: Vec<u64>,
    /// The honest tree total: summed PSS (Linux) / phys_footprint (macOS).
    footprint: u64,
    /// Virtual address space, summed.
    virt: u64,
    /// Cumulative CPU seconds burned by the tree.
    cpu_seconds: f64,
    /// Page-class split, when the platform reports one (Linux). `null` on macOS
    /// and on samples taken with `VELD_STATS_MEMORY_DETAIL=off`; the UI hides
    /// the "split by type" view rather than drawing an empty stack.
    #[serde(flatten)]
    classes: MemoryClasses,
}

/// The optional page-class fields, flattened into [`NodeStats`] and
/// [`StatsBucketDto`] so both wire shapes name them identically.
#[derive(Serialize, Default)]
struct MemoryClasses {
    private_clean: Option<u64>,
    private_dirty: Option<u64>,
    shared_clean: Option<u64>,
    shared_dirty: Option<u64>,
    swap: Option<u64>,
    wired: Option<u64>,
}

impl From<&veld_core::stats::MemoryBreakdown> for MemoryClasses {
    fn from(m: &veld_core::stats::MemoryBreakdown) -> Self {
        Self {
            private_clean: m.private_clean,
            private_dirty: m.private_dirty,
            shared_clean: m.shared_clean,
            shared_dirty: m.shared_dirty,
            swap: m.swap,
            wired: m.wired,
        }
    }
}

async fn get_stats() -> Result<Json<StatsResponse>, StatusCode> {
    crate::offload::blocking(get_stats_blocking).await
}

fn get_stats_blocking() -> Result<Json<StatsResponse>, StatusCode> {
    let db = open_db()?;
    let registry = db.registry().map_err(|e| {
        warn!("failed to load registry for stats: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let now = chrono::Utc::now();
    let mut projects: HashMap<String, HashMap<String, HashMap<String, NodeStats>>> = HashMap::new();
    for entry in registry.projects.values() {
        for (run_name, run_info) in &entry.runs {
            // The same predicate the sampler writes by — one definition in
            // `veld_core::stats`, because a reader stricter than the writer
            // shows "no stats yet" while rows are being written for that very
            // node, and a reader looser than the writer serves stale ones.
            // Freshness below still hides anything that stopped being sampled,
            // so a `Starting` run that is stuck reads as absent, not frozen.
            if !veld_core::stats::is_sampled(run_info.status) {
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
                    .map(|h| h.memory.footprint)
                    .collect();
                nodes.insert(
                    node_key,
                    NodeStats {
                        cpu: s.cpu_percent,
                        mem: s.memory_bytes,
                        procs: s.process_count,
                        spark,
                        footprint: s.memory.footprint,
                        virt: s.memory.virtual_bytes,
                        cpu_seconds: s.cpu_seconds,
                        classes: (&s.memory).into(),
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
// Stats history API — the scrubbable graphs
// ---------------------------------------------------------------------------

/// Longest history window a client may request (seconds).
///
/// Taken from `veld_core::stats::NODE_STATS_RETENTION_SECS` rather than restated:
/// asking for more than the GC keeps can only return the same rows, and a local
/// copy of the number would let the two drift until the API promised a day of
/// data it no longer had.
const MAX_HISTORY_WINDOW_SECS: i64 = veld_core::stats::NODE_STATS_RETENTION_SECS;

/// Default window when the client doesn't ask (15 minutes).
const DEFAULT_HISTORY_WINDOW_SECS: i64 = 900;

/// Ceiling on returned buckets. The bucket width is derived from
/// `window / points`, so this — not the window — is what bounds the payload and
/// the render cost. 2000 points is already finer than any plausible chart pixel
/// width.
const MAX_HISTORY_POINTS: u32 = 2000;

/// Default bucket count: enough to fill a wide chart at one point per few
/// pixels without asking the browser to draw more than it can show.
const DEFAULT_HISTORY_POINTS: u32 = 240;

/// Cap on per-process series returned. Past this the chart is unreadable and the
/// legend is longer than the graph; the omitted tail is reported in
/// `processes_omitted` so the UI can say so rather than silently truncating.
const MAX_PROCESS_SERIES: usize = 12;

/// Clamp a requested window to what retention can actually answer.
///
/// Pure and separate from the handler so the clamp has a test: the handler's DB
/// access is what made this untestable before.
fn clamp_window_secs(requested: Option<i64>) -> i64 {
    requested
        .unwrap_or(DEFAULT_HISTORY_WINDOW_SECS)
        .clamp(1, MAX_HISTORY_WINDOW_SECS)
}

/// Which memory metrics these buckets actually carry, in picker order.
///
/// **Unions over every bucket, not just the newest.** Reading only the last
/// bucket looked like it bought picker stability and did the opposite: one
/// process failing its `smaps_rollup` read poisons that whole sample's page
/// classes (`MemoryBreakdown::add`, and the SQL `CASE WHEN COUNT(x)=COUNT(*)`),
/// and a short-lived child exiting mid-scan is routine — so a single transient
/// failure in the newest bucket dropped every page class from the picker, which
/// made the UI reset the reader's chosen metric and kick them out of the
/// by-type view. A class present anywhere in the window is offerable; the
/// buckets where it is absent render as gaps, which the chart already does.
fn derive_available_metrics(buckets: &[veld_core::stats::StatsBucket]) -> Vec<&'static str> {
    use veld_core::stats::MemoryMetric;
    // Footprint and resident always have a value (footprint falls back to RSS).
    // `virtual` does NOT: a pre-v7 row never recorded it, and `MemoryMetric::read`
    // reports 0 as absent — so offering it unconditionally listed a metric that
    // renders as an unexplained blank chart on a window of old samples.
    let mut out = vec![
        MemoryMetric::Footprint.as_str(),
        MemoryMetric::Resident.as_str(),
    ];
    for m in MemoryMetric::ALL {
        if !out.contains(&m.as_str())
            && buckets
                .iter()
                .any(|b| m.read(b.memory_bytes, &b.memory).is_some())
        {
            out.push(m.as_str());
        }
    }
    out
}

/// Truncate per-process series to [`MAX_PROCESS_SERIES`], returning how many
/// were dropped. Counting before truncating is the whole content of this
/// function — swap the two and `processes_omitted` is always 0 and the UI's
/// "N more processes not charted" note silently disappears.
fn cap_series<T>(series: &mut Vec<T>) -> usize {
    let omitted = series.len().saturating_sub(MAX_PROCESS_SERIES);
    series.truncate(MAX_PROCESS_SERIES);
    omitted
}

#[derive(Deserialize)]
struct HistoryQuery {
    /// Required, like every run-addressed route — a bare run name is ambiguous
    /// across projects.
    project_root: String,
    run: String,
    /// Node key, `"node:variant"`.
    node: String,
    /// Window length in seconds, ending now. Clamped to
    /// [`MAX_HISTORY_WINDOW_SECS`].
    #[serde(default)]
    window: Option<i64>,
    /// Requested bucket count, clamped to [`MAX_HISTORY_POINTS`].
    #[serde(default)]
    points: Option<u32>,
    /// Include the per-process series and the latest process tree. Off by
    /// default: it is several times the query cost and the collapsed UI never
    /// needs it.
    #[serde(default)]
    processes: bool,
}

/// One aggregated bucket on the wire. A flatter shape than
/// [`veld_core::stats::StatsBucket`] — the page classes are hoisted to the top
/// level so a chart can index a metric by its wire name (`"private_dirty"`)
/// without knowing whether it lives in a nested object.
#[derive(Serialize)]
struct StatsBucketDto {
    /// Bucket start, epoch milliseconds.
    t: i64,
    /// Raw samples averaged into this bucket. Never 0 — empty buckets are
    /// omitted, so a gap in `t` means "no data", not "zero usage".
    samples: u32,
    cpu: f32,
    cpu_peak: f32,
    procs: f32,
    /// RSS (`resident`), averaged.
    resident: u64,
    footprint: u64,
    footprint_peak: u64,
    #[serde(rename = "virtual")]
    virtual_bytes: u64,
    #[serde(flatten)]
    classes: MemoryClasses,
}

impl From<&veld_core::stats::StatsBucket> for StatsBucketDto {
    fn from(b: &veld_core::stats::StatsBucket) -> Self {
        Self {
            t: b.bucket_start.timestamp_millis(),
            samples: b.samples,
            cpu: b.cpu_percent,
            cpu_peak: b.cpu_peak,
            procs: b.process_count,
            resident: b.memory_bytes,
            footprint: b.memory.footprint,
            footprint_peak: b.footprint_peak,
            virtual_bytes: b.memory.virtual_bytes,
            classes: (&b.memory).into(),
        }
    }
}

#[derive(Serialize)]
struct ProcessSeriesDto {
    pid: u32,
    name: String,
    cmd: Option<String>,
    buckets: Vec<StatsBucketDto>,
}

/// One process in the latest tree snapshot, for the process table.
#[derive(Serialize)]
struct ProcessRowDto {
    pid: u32,
    parent_pid: Option<u32>,
    /// Depth below the node's root process. A row's parent may be absent from
    /// this list (the sampler caps how many processes it records per sample), so
    /// indent by `depth` rather than by looking the parent up.
    depth: u32,
    name: String,
    cmd: Option<String>,
    cpu: f32,
    cpu_seconds: f64,
    resident: u64,
    footprint: u64,
    #[serde(rename = "virtual")]
    virtual_bytes: u64,
    /// Epoch milliseconds, when the platform reported a start time.
    started_at: Option<i64>,
    #[serde(flatten)]
    classes: MemoryClasses,
}

#[derive(Serialize)]
struct StatsHistoryResponse {
    /// Window actually served, in epoch milliseconds — the clamps mean this can
    /// be narrower than requested, and a chart's axis must follow what was
    /// served, not what was asked for.
    start: i64,
    end: i64,
    bucket_secs: i64,
    /// Memory metrics this node's samples actually carry, in the order a picker
    /// should offer them. Derived from the data rather than from the daemon's
    /// platform, so a window spanning a `VELD_STATS_MEMORY_DETAIL` change
    /// reports honestly.
    available_metrics: Vec<&'static str>,
    buckets: Vec<StatsBucketDto>,
    /// Per-process series, empty unless `processes=true`.
    processes: Vec<ProcessSeriesDto>,
    /// How many series were dropped by [`MAX_PROCESS_SERIES`].
    processes_omitted: usize,
    /// The most recent process tree. Served whenever the node has rows —
    /// unlike `processes`, this is one query for the latest sample, and the UI
    /// shows the table under every split mode.
    tree: Vec<ProcessRowDto>,
    /// Retention horizon for node aggregates (seconds). Served so the UI builds
    /// its window presets from what the daemon actually keeps instead of a
    /// hardcoded copy that can silently drift out of agreement with the GC.
    retention_secs: i64,
    /// Retention horizon for per-process rows (seconds) — shorter than
    /// `retention_secs`. A by-process chart over a longer window is legitimately
    /// empty before this boundary, and the UI says so rather than looking broken.
    process_retention_secs: i64,
}

async fn get_stats_history(
    query: Query<HistoryQuery>,
) -> Result<Json<StatsHistoryResponse>, StatusCode> {
    crate::offload::blocking(move || get_stats_history_blocking(query)).await
}

fn get_stats_history_blocking(
    Query(q): Query<HistoryQuery>,
) -> Result<Json<StatsHistoryResponse>, StatusCode> {
    use veld_core::stats::StatsWindow;

    validate_run_name(&q.run)?;
    let db = open_db()?;
    let registry = db.registry().map_err(|e| {
        warn!("failed to load registry for stats history: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    // Resolves to the registry's own path, and 404s a project the daemon does
    // not track — the same gate every run-addressed route uses.
    let project_root = resolve_run_project(&registry, &q.project_root, &q.run)?;

    let end = chrono::Utc::now();
    let window_secs = clamp_window_secs(q.window);
    let start = end - chrono::Duration::seconds(window_secs);
    let points = q.points.unwrap_or(DEFAULT_HISTORY_POINTS);
    let window = StatsWindow::for_points(start, end, points.min(MAX_HISTORY_POINTS));

    let buckets = db
        .node_stats_buckets(&project_root, &q.run, &q.node, window)
        .map_err(|e| {
            warn!("failed to load stats history for '{}': {e}", q.node);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let available_metrics = derive_available_metrics(&buckets);

    // The tree is one query for the latest sample, so it is served for every
    // split mode — the UI's process table sits under all of them. Only the
    // per-process *series* (one grouped query over the whole window) is gated
    // on `processes`.
    let tree = db
        .latest_process_tree(&project_root, &q.run, &q.node)
        .unwrap_or_default();

    let (processes, processes_omitted) = if q.processes {
        let mut series = db
            .process_stats_buckets(&project_root, &q.run, &q.node, window)
            .map_err(|e| {
                warn!("failed to load process history for '{}': {e}", q.node);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        // Already sorted heaviest-first by the query, so truncation drops the
        // tail rather than an arbitrary slice.
        let omitted = cap_series(&mut series);
        (series, omitted)
    } else {
        (Vec::new(), 0)
    };

    Ok(Json(StatsHistoryResponse {
        start: start.timestamp_millis(),
        end: end.timestamp_millis(),
        bucket_secs: window.bucket_secs,
        available_metrics,
        buckets: buckets.iter().map(StatsBucketDto::from).collect(),
        processes: processes
            .into_iter()
            .map(|s| ProcessSeriesDto {
                pid: s.pid,
                name: s.name,
                cmd: s.cmd,
                buckets: s.buckets.iter().map(StatsBucketDto::from).collect(),
            })
            .collect(),
        processes_omitted,
        retention_secs: veld_core::stats::NODE_STATS_RETENTION_SECS,
        process_retention_secs: veld_core::stats::PROCESS_STATS_RETENTION_SECS,
        tree: tree
            .into_iter()
            .map(|p| ProcessRowDto {
                pid: p.pid,
                parent_pid: p.parent_pid,
                depth: p.depth,
                name: p.name,
                cmd: p.cmd,
                cpu: p.cpu_percent,
                cpu_seconds: p.cpu_seconds,
                resident: p.memory_bytes,
                footprint: p.memory.footprint,
                virtual_bytes: p.memory.virtual_bytes,
                started_at: p.started_at.map(|t| t.timestamp_millis()),
                classes: (&p.memory).into(),
            })
            .collect(),
    }))
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
    /// (alias "veld"). Mirrors `veld orchestrator logs --source`.
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
    run_name: Path<String>,
    query: Query<LogQuery>,
) -> Result<Json<LogResponse>, StatusCode> {
    crate::offload::blocking(move || get_logs_blocking(run_name, query)).await
}

fn get_logs_blocking(
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
    // Suppressed under a specific `node` filter: this is the run's liveness
    // stream (probes, recoveries; rows have `node = NULL`), not the node you
    // asked for, and it drowned per-node output while debugging a crash.
    if include_internal && q.node.is_none() {
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
    // does not apply to it — the same rule `veld orchestrator logs` follows for the run-level
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

/// Check that a request's `Host` names this machine — the DNS-rebinding guard for
/// the routes that hand file contents to the page that asked (`file_pane`,
/// `changes`).
///
/// [`check_csrf`] keeps a *cross-origin* page from sending the header, but DNS
/// rebinding makes the page same-origin: `evil.example` re-resolves to 127.0.0.1,
/// its scripts send any header they like, and the one thing left saying
/// `evil.example` is `Host`. Every legitimate caller names a loopback address or a
/// `.localhost` name, which cannot be rebound — the CLI (`daemon_base`, 127.0.0.1),
/// Caddy's `veld.localhost` and `*.veld.localhost` routes (`reverse_proxy` keeps
/// the client's `Host`), and vite's `/api` proxy (`localhost:5199` or its
/// `.veld.localhost` name; no `changeOrigin`). A dev instance may also be reached
/// at its `VELD_MANAGEMENT_HOST` or a `dev_trusted_origins` host, which need not
/// be `.localhost`, so those are allowed by exact name.
///
/// A request with **no** `Host` passes: every browser sends one, so its absence is
/// never a rebinding, and refusing it would only turn away a bare HTTP/1.0 tool.
pub(super) fn check_host(headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    let Some(raw) = headers.get(axum::http::header::HOST) else {
        return Ok(());
    };
    let host = raw.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
    let extra: Vec<String> = veld_core::instance::management_host()
        .into_iter()
        .chain(
            veld_core::instance::dev_trusted_origins()
                .iter()
                .filter_map(|o| o.split_once("://").map(|(_, h)| hostname_of(h))),
        )
        .collect();
    if host_is_local(host, &extra) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// The hostname half of a `host[:port]`, lowercased, without a trailing dot.
fn hostname_of(authority: &str) -> String {
    let name = match authority.strip_prefix('[') {
        // `[::1]:19898` — the brackets are part of the name as `Host` spells it.
        Some(rest) => rest
            .split_once(']')
            .map_or(authority, |(inner, _)| &authority[..inner.len() + 2]),
        None => authority.split(':').next().unwrap_or_default(),
    };
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Whether `host` (a `Host` header's value) is loopback, a `.localhost` name, or
/// one of `extra`. Pure, for the tests.
fn host_is_local(host: &str, extra: &[String]) -> bool {
    let name = hostname_of(host);
    matches!(name.as_str(), "127.0.0.1" | "[::1]" | "localhost")
        || name.ends_with(".localhost")
        || extra.iter().any(|e| e.eq_ignore_ascii_case(&name))
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

/// Run a node-defined action through the engine, which reads the live outputs
/// and runs it unattended. Any credentials stay server-side — the browser only
/// ever sent a name.
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
    let cfg = load_project_config_fresh(&project_root);
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

    engine_status(
        super::engine::spawn_daemon_action(
            &project_root,
            veld_engine::api::ActionKind::Action {
                run: run_name,
                action: body.action,
                node: body.node,
            },
        )
        .await,
    )
}

/// What an IDE button gets back: `202` once the worker is running, or the
/// engine's refusal (`503` during an update).
fn engine_status(r: Result<String, (StatusCode, String)>) -> StatusCode {
    match r {
        Ok(_) => StatusCode::ACCEPTED,
        Err((code, message)) => {
            warn!("engine refused an IDE action ({code}): {message}");
            code
        }
    }
}

/// Stop / restart helper: an engine action for the run in the project the
/// caller scoped it to — the project root is what disambiguates which
/// project's `main` gets stopped.
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
    let run = run_name.to_owned();
    let kind = match action {
        "restart" => veld_engine::api::ActionKind::Restart { run },
        _ => veld_engine::api::ActionKind::Stop { run },
    };
    engine_status(super::engine::spawn_daemon_action(&project_root, kind).await)
}

/// Where an older daemon captured a spawned `veld`'s stderr.
fn spawn_log_dir() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".veld").join("spawn-logs"))
}

/// Delete leftover capture files at daemon start.
///
/// Nothing writes here any more: the daemon used to exec `veld` and capture its
/// stderr in this directory, and now runs the engine in workers instead. A
/// daemon killed between such a spawn and its reap left the file named on disk,
/// and nothing else collects this directory — so the sweep stays, to clear what
/// an older daemon left behind.
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

/// Hand a run that ended abnormally to an engine `stop`, so its nodes' `on_stop`
/// hooks actually run.
///
/// A crashed, orphaned or half-torn-down run still owes every node's teardown,
/// and for a container node that hook is the only thing that removes its
/// container: killing the `docker run` / `container run` client does not stop
/// the container, and `--rm` only fires when the container itself exits. Left
/// undone it is not clutter but a hard blocker, because the container name
/// interpolates `${veld.run}` — the *environment* name, not a per-run id — so
/// the name is identical on every run of that environment and the next
/// `veld orchestrator start` fails with `container ... already exists` until somebody removes
/// it by hand. `veld self gc` does not help: it collects veld's own state and logs,
/// never the container runtime's.
///
/// **Delegated to an engine worker rather than run in this process.** Resolving
/// a hook's command, `${vars.*}`, env and cwd is `Orchestrator`'s job and it must
/// stay the only resolver (AGENTS.md → Config Authoring Principles: one owner
/// for resolution). The worker gets the user's login-shell `PATH` resolved in
/// the project's own directory — under launchd this daemon's own `PATH` cannot
/// find `docker` at all — and a hung hook hangs the worker, not the daemon.
///
/// The delegate reaches the run when it is already terminal and takes
/// `Orchestrator::stop`'s already-ended branch, which runs what the run still
/// owes. **Callers must gate this on their own guarded status transition**
/// (`finalize_crashed` / `finalize_run` returning `true`): that single-winner
/// election is what stops the 5s monitor, the 600s GC and a concurrent
/// `veld orchestrator start` sweep from each spawning one. Even so, a duplicate is harmless
/// rather than wrong — teardown's ledger is per node and persisted as each hook
/// returns, so a second delegate resumes instead of repeating.
///
/// Best-effort by design and not awaited: the worker's outcome is in its
/// action's event log (`engine_actions`), not here. Neither a failed teardown
/// nor a refused one (an update holding the lock) loses anything permanently:
/// the next `veld orchestrator start` for the project sweeps whatever its runs still owe.
pub(crate) async fn delegate_teardown(project_root: &std::path::Path, run_name: &str) {
    match super::engine::spawn_daemon_action(
        project_root,
        veld_engine::api::ActionKind::Stop {
            run: run_name.to_owned(),
        },
    )
    .await
    {
        Ok(id) => {
            tracing::info!(action = %id, "engine stop for '{run_name}' to tear down an ended run");
        }
        Err((code, message)) => warn!(
            "could not tear down the ended run '{run_name}' ({code}: {message}); its on_stop \
             hooks run on the next `veld orchestrator start` here"
        ),
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

    /// Every way the daemon is legitimately reached passes, and a rebound name
    /// does not — however it dresses up as local.
    #[test]
    fn only_local_hosts_pass_the_rebinding_guard() {
        let extra = ["veld-dev.example".to_owned()];
        for ok in [
            "127.0.0.1:19898",
            "127.0.0.1",
            "[::1]:19898",
            "localhost:5199",
            "LOCALHOST",
            "veld.localhost",
            "veld.localhost:18443",
            "dev-daemon.run.veld.localhost",
            "localhost.",
            "veld-dev.example:443",
        ] {
            assert!(host_is_local(ok, &extra), "{ok}");
        }
        for refused in [
            "evil.example",
            "evil.example:19898",
            "localhost.evil.example",
            "evillocalhost",
            "127.0.0.1.evil.example",
            "[::2]:19898",
            "",
        ] {
            assert!(!host_is_local(refused, &extra), "{refused}");
        }
    }

    #[test]
    fn check_host_refuses_a_rebound_name_and_allows_none() {
        let mut headers = axum::http::HeaderMap::new();
        assert!(check_host(&headers).is_ok(), "no Host is no browser");
        headers.insert(axum::http::header::HOST, "veld.localhost".parse().unwrap());
        assert!(check_host(&headers).is_ok());
        headers.insert(axum::http::header::HOST, "evil.example".parse().unwrap());
        assert_eq!(check_host(&headers), Err(StatusCode::FORBIDDEN));
    }

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
                    hostnames: Vec::new(),
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
            // Stats history addresses a run through required query fields rather
            // than a path segment (a node key like `web:local` is not
            // path-safe), so the same rule has to hold there: no project scope,
            // no answer. Rejected during extraction, before any DB access.
            assert_eq!(
                status(get("/api/stats/history?run=main&node=web:local")).await,
                StatusCode::BAD_REQUEST
            );
            assert_eq!(
                status(get(
                    "/api/stats/history?project_root=/repos/alpha&node=web:local"
                ))
                .await,
                StatusCode::BAD_REQUEST,
                "a scope without a run name is not addressable either"
            );
        }

        #[tokio::test]
        async fn stats_history_validates_its_numeric_query_fields() {
            // `window`/`points` are typed, so a garbage value must 400 rather
            // than silently falling back to a default window — a client asking
            // for the wrong range should be told, not quietly answered.
            for uri in [
                "/api/stats/history?project_root=/repos/alpha&run=main&node=web:local&window=soon",
                "/api/stats/history?project_root=/repos/alpha&run=main&node=web:local&points=lots",
            ] {
                assert_eq!(status(get(uri)).await, StatusCode::BAD_REQUEST, "{uri}");
            }
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

    /// The pure parts of `/api/stats/history`. The handler itself needs a DB, so
    /// these three were extracted precisely so the rules could be pinned.
    mod stats_history {
        use super::super::*;
        use veld_core::stats::{MemoryBreakdown, StatsBucket};

        fn bucket(memory: MemoryBreakdown) -> StatsBucket {
            StatsBucket {
                bucket_start: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
                samples: 1,
                cpu_percent: 1.0,
                cpu_peak: 1.0,
                process_count: 1.0,
                memory_bytes: 1000,
                memory,
                footprint_peak: 500,
            }
        }

        fn detailed() -> MemoryBreakdown {
            MemoryBreakdown {
                footprint: 500,
                virtual_bytes: 5000,
                private_clean: Some(1),
                private_dirty: Some(2),
                shared_clean: Some(3),
                shared_dirty: Some(4),
                swap: Some(5),
                wired: Some(6),
            }
        }

        #[test]
        fn window_clamps_to_retention_and_defaults() {
            assert_eq!(clamp_window_secs(None), DEFAULT_HISTORY_WINDOW_SECS);
            assert_eq!(clamp_window_secs(Some(300)), 300);
            // Asking for a week can only return what retention kept.
            assert_eq!(
                clamp_window_secs(Some(7 * 24 * 3600)),
                MAX_HISTORY_WINDOW_SECS
            );
            // Zero and negative would make an empty or inverted range.
            assert_eq!(clamp_window_secs(Some(0)), 1);
            assert_eq!(clamp_window_secs(Some(-60)), 1);
        }

        #[test]
        fn the_window_clamp_tracks_gc_retention() {
            // The two used to be independent literals whose doc comments merely
            // claimed to agree. Now one defines the other — assert it, so a
            // future edit that reintroduces a local copy fails here.
            assert_eq!(
                MAX_HISTORY_WINDOW_SECS,
                veld_core::stats::NODE_STATS_RETENTION_SECS
            );
            // (The ordering of the two horizons is guarded by a `const` assert
            // at their definition in `veld_core::stats`.)
        }

        #[test]
        fn available_metrics_always_offers_the_two_unconditional_totals() {
            // Even with no data at all — the picker must never be empty.
            // `virtual` is deliberately NOT here: a pre-v7 row never recorded it
            // and `MemoryMetric::read` reports 0 as absent, so offering it
            // unconditionally would list a metric that plots as a blank chart.
            let m = derive_available_metrics(&[]);
            assert_eq!(m, vec!["footprint", "resident"]);
        }

        #[test]
        fn virtual_is_offered_only_when_the_data_carries_it() {
            let mut b = bucket(detailed());
            assert!(
                derive_available_metrics(&[b]).contains(&"virtual"),
                "a real virtual size is offerable"
            );
            // A bucket built from pre-v7 rows: `virtual_bytes` averaged to
            // nothing and surfaced as 0, which means "not recorded".
            b.memory.virtual_bytes = 0;
            assert!(
                !derive_available_metrics(&[b]).contains(&"virtual"),
                "an unrecorded virtual size must not be offered"
            );
        }

        #[test]
        fn a_class_present_in_any_bucket_stays_offerable() {
            // The regression this replaced: one transient probe failure in the
            // NEWEST bucket used to drop every page class from the picker, which
            // reset the reader's metric and closed the by-type view.
            let buckets = vec![
                bucket(detailed()),
                bucket(MemoryBreakdown::basic(1000, 5000)), // newest: probe failed
            ];
            let m = derive_available_metrics(&buckets);
            assert!(
                m.contains(&"private_dirty"),
                "a class measured earlier in the window is still offerable, got {m:?}"
            );
            assert!(m.contains(&"swap"));
        }

        #[test]
        fn a_class_no_bucket_carries_is_not_offered() {
            // macOS: totals only. The picker must not list what it cannot plot.
            let buckets = vec![bucket(MemoryBreakdown::basic(1000, 5000))];
            let m = derive_available_metrics(&buckets);
            assert_eq!(m, vec!["footprint", "resident", "virtual"]);
            assert!(!m.contains(&"private_dirty"));
        }

        #[test]
        fn cap_series_counts_before_truncating() {
            let mut few: Vec<u32> = (0..3).collect();
            assert_eq!(cap_series(&mut few), 0);
            assert_eq!(few.len(), 3);

            let mut many: Vec<u32> = (0..MAX_PROCESS_SERIES as u32 + 5).collect();
            assert_eq!(cap_series(&mut many), 5, "omitted count is pre-truncation");
            assert_eq!(many.len(), MAX_PROCESS_SERIES);
            // Heaviest-first ordering means truncation keeps the head.
            assert_eq!(many[0], 0);
        }
    }

    /// The dashboards decide what to render as a link from `EndpointInfo`, so
    /// three things have to hold: the primary is first, a raw port carries no
    /// URL, and a row from before per-port endpoints still describes itself.
    #[test]
    fn endpoint_infos_puts_the_primary_first_and_keeps_raw_ports_unlinked() {
        use veld_core::state::NodeEndpoint;

        let mut ns = NodeState::new("web", "local");
        ns.port = Some(3000);
        ns.url = Some("https://web.dev.p.localhost".into());
        // Inserted so that the primary is NOT the map's first key, which is what
        // makes the ordering claim worth testing at all.
        ns.endpoints.insert(
            "admin".into(),
            NodeEndpoint {
                hostname: "web-admin.dev.p.localhost".into(),
                url: Some("https://web-admin.dev.p.localhost".into()),
                port: 3001,
            },
        );
        ns.endpoints.insert(
            "http".into(),
            NodeEndpoint {
                hostname: "web.dev.p.localhost".into(),
                url: Some("https://web.dev.p.localhost".into()),
                port: 3000,
            },
        );
        ns.endpoints.insert(
            "pg".into(),
            NodeEndpoint {
                hostname: "web-pg.dev.p.localhost".into(),
                url: None,
                port: 5432,
            },
        );

        let eps = endpoint_infos(&ns);
        assert_eq!(eps[0].name, "http", "the primary leads");
        assert!(eps[0].primary);
        assert_eq!(eps.len(), 3);
        assert!(
            eps[1..].iter().all(|e| !e.primary),
            "exactly one endpoint is the primary"
        );
        let pg = eps
            .iter()
            .find(|e| e.name == "pg")
            .expect("raw port listed");
        assert!(pg.url.is_none(), "a raw port must never carry a URL");
        assert_eq!(pg.port, 5432);

        // A legacy row: `url` and `port`, no map. It still describes one port,
        // or the node would render as having none.
        let mut legacy = NodeState::new("web", "local");
        legacy.port = Some(3000);
        legacy.url = Some("https://web.dev.p.localhost".into());
        let eps = endpoint_infos(&legacy);
        assert_eq!(eps.len(), 1);
        assert!(eps[0].primary && eps[0].url.is_some());
        assert_eq!(eps[0].hostname, "web.dev.p.localhost");

        // And a node that genuinely has no ports offers nothing.
        assert!(endpoint_infos(&NodeState::new("watcher", "dev")).is_empty());
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
}
