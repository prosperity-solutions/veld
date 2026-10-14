//! The engine's home in the daemon: spawning workers, streaming what they say,
//! noticing when one dies, and the `/api/engine/*` routes every face talks to.
//!
//! **One engine per machine, many faces.** `veld start` in a terminal, the IDE's
//! start button, the monitor's crash recovery and the trash flow's teardown all
//! come through [`spawn_action`]. None of them execs a `veld` binary any more:
//! the daemon spawns `veld-daemon --engine-worker` (see `engine_worker.rs`) and
//! the worker runs the engine.
//!
//! **Nothing here waits on a worker.** A handler answers as soon as the worker is
//! spawned; a face that wants the outcome reads the action's event log, which
//! the worker writes directly. That is what lets a worker outlive this daemon
//! (an update restarts it), and what removed the circular wait the old
//! `spawn_veld` had to dodge (a `veld stop` the daemon waited on, calling back
//! into the daemon).

use std::path::{Path, PathBuf};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tracing::{info, warn};
use veld_core::db::Db;
use veld_core::db::engine_actions::{STATUS_FAILED, STATUS_LOST, STATUS_RUNNING};
use veld_engine::api::{
    ActionOutcome, ActionRequest, ActionSecrets, EngineEvent, FailureKind, PreflightRequest,
    ResolveActionRequest, SaveVarAnswers, SubmitAction, Submitted,
};

use super::management::check_csrf;
use crate::engine_worker::WorkerInput;

type ApiError = (StatusCode, String);

fn err(code: StatusCode, message: impl Into<String>) -> ApiError {
    (code, message.into())
}

/// How long a freshly spawned worker gets to take its lock before a free lock
/// means it died. It takes it within milliseconds of starting.
const LOCK_GRACE_SECS: i64 = 15;

/// How often the supervisor checks running actions for dead workers.
const SWEEP_SECS: u64 = 5;

/// The directory a face may scope an action to: one that holds a `veld.json`,
/// canonicalized so two spellings of a path are one project.
pub(crate) fn project_root_of(path: &Path) -> Result<PathBuf, ApiError> {
    let canon = std::fs::canonicalize(path)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "no such project directory"))?;
    let config = veld_core::config::root_config_in(&canon)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no veld.json in that directory"))?;
    Ok(veld_core::config::project_root(&config))
}

/// Start a worker for `request`. Returns the action id as soon as the worker is
/// spawned; the outcome arrives in the action's event log.
///
/// Refuses with `503` while `veld update` holds the update lock — the answer a
/// UI already shows as "try again shortly", and the honest one. A worker already
/// running keeps running across the update.
pub(crate) async fn spawn_action(
    request: ActionRequest,
    secrets: ActionSecrets,
) -> Result<String, ApiError> {
    if veld_core::update_lock::current().is_some() {
        warn!(
            kind = request.kind.label(),
            "refusing an engine action: an update is in progress"
        );
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "a veld update is in progress — try again when it finishes",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let project_root = request.project_root.clone();
    // Resolved **before** the row exists: a cold directory resolves a login
    // shell inline (seconds, on a slow rc file), and the supervisor's lock grace
    // counts from the row's `created_at`.
    //
    // The user's login-shell PATH, resolved in the project's own directory — a
    // config's commands need whatever tool versions that directory selects
    // (`.nvmrc`, `.zshrc`). Only PATH is inherited; everything else a node needs
    // it declares in veld.json, which is the same whoever started the run.
    let path_env = veld_core::user_path::cached_user_path_for(&project_root).await;

    let request_json = serde_json::to_string(&request)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    {
        let id = id.clone();
        let root = project_root.to_string_lossy().into_owned();
        let run = request.kind.run().unwrap_or_default().to_owned();
        let kind = request.kind.label();
        crate::offload::blocking(move || {
            Db::open().and_then(|db| db.insert_engine_action(&id, &root, &run, kind, &request_json))
        })
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    let input = WorkerInput {
        action_id: id.clone(),
        request,
        secrets,
    };
    match spawn_worker(&project_root, &path_env, &input).await {
        Ok(pid) => {
            info!(action = %id, pid, "engine worker spawned");
            Ok(id)
        }
        Err(e) => {
            warn!(action = %id, error = %e, "could not spawn an engine worker");
            let outcome = ActionOutcome::Failed {
                message: format!("could not start the engine worker: {e}"),
                kind: FailureKind::Error,
                failed_node: None,
            };
            record_end(&id, STATUS_FAILED, &outcome).await;
            Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not start the engine worker",
            ))
        }
    }
}

#[cfg(not(test))]
async fn spawn_worker(
    project_root: &Path,
    path_env: &str,
    input: &WorkerInput,
) -> std::io::Result<u32> {
    use tokio::io::AsyncWriteExt;
    let exe = veld_core::process::own_exe();
    let mut child = tokio::process::Command::new(exe)
        .arg("--engine-worker")
        .current_dir(project_root)
        .env("PATH", path_env)
        .stdin(std::process::Stdio::piped())
        // stdout nulled, stderr inherited: the worker's tracing belongs in the
        // daemon's log, and a pipe nobody drains would block it.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        // Its own group: launchd's and systemd's restarts of the daemon must not
        // take an action down with it.
        .process_group(0)
        .spawn()?;
    let pid = child.id().unwrap_or(0);
    let json = serde_json::to_vec(input).map_err(std::io::Error::other)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&json).await?;
        // Dropping stdin is the worker's end-of-input.
    }
    // Dropped, not awaited: tokio reaps it on SIGCHLD while this daemon lives,
    // and init reaps it if the daemon goes first.
    drop(child);
    Ok(pid)
}

/// Tests never spawn a worker: the action stays `running` with no lock, which is
/// exactly the state the sweep's tests need.
#[cfg(test)]
async fn spawn_worker(_: &Path, _: &str, _: &WorkerInput) -> std::io::Result<u32> {
    Ok(0)
}

/// Write a final event and status for an action the worker did not finish.
async fn record_end(id: &str, status: &'static str, outcome: &ActionOutcome) {
    let id = id.to_owned();
    let event = serde_json::to_string(&EngineEvent::Finished {
        outcome: outcome.clone(),
    })
    .unwrap_or_default();
    let outcome = serde_json::to_string(outcome).unwrap_or_default();
    let _ = crate::offload::blocking(move || -> Result<bool, veld_core::db::DbError> {
        // Guarded and atomic: only the call that ends a still-running action
        // writes its Finished event, so a worker finishing at the same moment
        // does not get a second, contradicting one.
        Db::open()?.finish_engine_action_with_event(&id, status, &outcome, &event)
    })
    .await;
}

/// Wait for an action to end, up to `timeout`. `None` on timeout or if the row
/// vanished. For the daemon's own callers that must know how it went (crash
/// recovery counts attempts); faces read the event stream instead.
pub(crate) async fn await_action(
    id: &str,
    timeout: std::time::Duration,
) -> Option<(String, ActionOutcome)> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let row = {
            let id = id.to_owned();
            crate::offload::blocking(move || Db::open().and_then(|db| db.engine_action(&id)))
                .await
                .ok()
                .flatten()
        }?;
        if row.status != STATUS_RUNNING {
            let outcome = row
                .outcome_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or(ActionOutcome::Unknown);
            return Some((row.status, outcome));
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Run an action for the daemon itself (crash teardown, recovery, the trash
/// flow): resolved project root, no secrets — there is no caller with a
/// terminal, so a run needing a declared env var fails and says which.
pub(crate) async fn spawn_daemon_action(
    project_root: &Path,
    kind: veld_engine::api::ActionKind,
) -> Result<String, ApiError> {
    spawn_action(
        ActionRequest {
            project_root: project_root.to_path_buf(),
            kind,
            debug: false,
        },
        ActionSecrets::default(),
    )
    .await
}

/// Start a run for a face with no terminal (the IDE's start button): preflight
/// non-interactively, refuse with the preflight's own words when it cannot go
/// ahead, else spawn the worker.
pub(crate) async fn start_unattended(
    project_root: &Path,
    selections: Vec<String>,
    preset: Option<String>,
    name: Option<String>,
) -> Result<String, ApiError> {
    use veld_engine::api::{PreflightPlan, PreflightResponse};
    let req = PreflightRequest {
        project_root: project_root.to_path_buf(),
        plan: PreflightPlan::Start {
            selections,
            preset,
            name,
            oneshot: false,
            all_logs: false,
        },
        var_flags: Vec::new(),
        interactive: false,
    };
    let answer = crate::offload::blocking(move || veld_engine::preflight::preflight(&req)).await;
    let ready = match answer {
        PreflightResponse::Ready(ready) => ready,
        PreflightResponse::Refused { message, .. } => {
            return Err(err(StatusCode::UNPROCESSABLE_ENTITY, message));
        }
        PreflightResponse::Choose(_) | PreflightResponse::Unknown => {
            return Err(err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "nothing selected and no default_preset — pick a preset",
            ));
        }
    };
    if !ready.unanswered.is_empty() {
        let names: Vec<&str> = ready.unanswered.iter().map(|v| v.name.as_str()).collect();
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "this project needs values on this machine first: {} — set them with \
                 `veld config set`",
                names.join(", ")
            ),
        ));
    }
    spawn_daemon_action(project_root, ready.action).await
}

/// How long an in-flight action keeps the monitor's recovery and GC's stale
/// `stopping` reaper away from its run. A worker stuck on a hook that never
/// returns holds its lock for ever; past this, the run is treated as having no
/// live ender again and the old safety nets apply.
const ACTION_GATE_SECS: i64 = 2 * 60 * 60;

/// Whether an engine action is starting, stopping or restarting `run` in
/// `project_root` right now. The monitor uses it so a crash recovery does not
/// race a restart already in flight, and GC so it does not reap a run whose
/// teardown is merely slow.
///
/// A `stop --all` counts for every run of its project. A node action (an IDE
/// button) does not: it changes no run's lifecycle, and a long one must not
/// switch recovery off.
pub(crate) fn has_running_action(db: &Db, project_root: &Path, run: &str) -> bool {
    let root = project_root.to_string_lossy();
    let now = chrono::Utc::now();
    db.running_engine_actions()
        .map(|rows| {
            rows.iter().any(|r| {
                let fresh = chrono::DateTime::parse_from_rfc3339(&r.created_at)
                    .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() < ACTION_GATE_SECS)
                    .unwrap_or(false);
                let kind = serde_json::from_str::<ActionRequest>(&r.request_json)
                    .map(|req| req.kind)
                    .unwrap_or(veld_engine::api::ActionKind::Unknown);
                fresh
                    && r.project_root == root
                    && kind.gates_recovery()
                    && (r.run_name == run || kind.gates_whole_project())
            })
        })
        .unwrap_or(false)
}

/// Mark every running action whose worker is gone as lost. Run at boot (adopting
/// what the previous daemon left) and every [`SWEEP_SECS`].
///
/// A worker's liveness is its lock, not its PID: a PID recycled while the daemon
/// was down would keep a dead action "running" forever.
async fn sweep_lost() {
    sweep_lost_with(LOCK_GRACE_SECS).await
}

async fn sweep_lost_with(grace_secs: i64) {
    let rows = crate::offload::blocking(|| Db::open().and_then(|db| db.running_engine_actions()))
        .await
        .unwrap_or_default();
    let now = chrono::Utc::now();
    for row in rows {
        let young = chrono::DateTime::parse_from_rfc3339(&row.created_at)
            .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() < grace_secs)
            .unwrap_or(false);
        if young {
            continue;
        }
        // Take the worker's lock ourselves, and hold it while declaring the
        // action lost. A probe-then-write would race a worker that takes its lock
        // between the two (a host that slept, a slow spawn): it would run the
        // whole action behind a face told it failed. Holding the lock, a late
        // worker cannot acquire it and exits; one that already holds it is alive.
        let _held = match veld_core::engine_lock::WorkerLock::acquire(&row.id) {
            Ok(lock) => Some(lock),
            // Held: the worker is alive.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            // Out of descriptors: a fact about this daemon, not the worker.
            Err(e) if matches!(e.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) => continue,
            // Anything else (a filesystem without `flock`, an unwritable engine
            // dir) says nothing about a worker. The worker could not take its own
            // lock either and exited, so the action must still end — or it stays
            // `running` for good and its face's stream never finishes.
            Err(e) => {
                warn!(action = %row.id, error = %e, "engine lock unavailable; ending the action without it");
                None
            }
        };
        warn!(action = %row.id, kind = %row.kind, "engine worker gone without finishing");
        record_end(
            &row.id,
            STATUS_LOST,
            &ActionOutcome::Failed {
                message: "The engine worker stopped before it finished. Check `veld status` \
                          and run the command again."
                    .to_owned(),
                kind: FailureKind::WorkerLost,
                failed_node: None,
            },
        )
        .await;
    }
}

/// The supervisor loop. Spawned once at daemon start.
pub(crate) async fn supervise() {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(SWEEP_SECS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        sweep_lost().await;
        // A week of finished actions is plenty to read back what happened.
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(7))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let _ = crate::offload::blocking(move || {
            Db::open().and_then(|db| db.prune_engine_actions(&cutoff))
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub(crate) fn routes() -> Router {
    Router::new()
        .route("/api/engine/preflight", post(preflight))
        .route("/api/engine/actions", post(submit))
        .route("/api/engine/actions/{id}/events", get(events))
        .route("/api/engine/actions/{id}/cancel", post(cancel))
        .route("/api/engine/var-answers", post(save_var_answers))
        .route("/api/engine/resolve-action", post(resolve_action))
        .route("/api/engine/env-names", get(env_names))
        // Every route goes ABOVE this line: `route_layer` wraps only the routes
        // added before it. `every_engine_route_refuses_a_foreign_host` checks.
        .route_layer(axum::middleware::from_fn(host_guard))
}

/// The DNS-rebinding guard for every `/api/engine/*` route, GETs included.
///
/// These routes run a `veld.json` from a path the caller names, and hand back
/// logs, outputs and resolved env. `check_csrf` keeps a *cross-origin* page from
/// sending the header, but a rebound page is same-origin and sends any header it
/// likes — the only thing left that names the attacker is `Host`. Every
/// legitimate face (the CLI on 127.0.0.1, the IDE on `*.localhost`) passes
/// [`super::management::check_host`]. Without this, a page the user merely
/// visits could have the daemon run a `veld.json` it got the browser to download.
pub(crate) async fn host_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    match super::management::check_host(req.headers()) {
        Ok(()) => next.run(req).await,
        Err(code) => code.into_response(),
    }
}

pub(crate) fn into_response<T: serde::Serialize>(r: Result<T, ApiError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err((code, message)) => {
            (code, Json(serde_json::json!({ "error": message }))).into_response()
        }
    }
}

async fn preflight(headers: HeaderMap, Json(mut req): Json<PreflightRequest>) -> Response {
    if let Err(code) = check_csrf(&headers) {
        return code.into_response();
    }
    let r = async {
        req.project_root = project_root_of(&req.project_root)?;
        Ok(crate::offload::blocking(move || veld_engine::preflight::preflight(&req)).await)
    }
    .await;
    into_response(r)
}

async fn submit(headers: HeaderMap, Json(mut body): Json<SubmitAction>) -> Response {
    if let Err(code) = check_csrf(&headers) {
        return code.into_response();
    }
    let r = async {
        body.request.project_root = project_root_of(&body.request.project_root)?;
        let action_id = spawn_action(body.request, body.secrets).await?;
        Ok(Submitted { action_id })
    }
    .await;
    match r {
        Ok(s) => (StatusCode::ACCEPTED, Json(s)).into_response(),
        Err(e) => into_response::<()>(Err(e)),
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: i64,
}

/// NDJSON: one `{"seq":…,"event":…}` per line, ending after `finished`.
///
/// Resumable: a face whose connection dropped (a daemon restart mid-start) asks
/// again with `?after=<last seq>` and gets exactly what it missed.
async fn events(UrlPath(id): UrlPath<String>, Query(q): Query<EventsQuery>) -> Response {
    let exists = {
        let id = id.clone();
        crate::offload::blocking(move || Db::open().and_then(|db| db.engine_action(&id)))
            .await
            .ok()
            .flatten()
            .is_some()
    };
    if !exists {
        return (StatusCode::NOT_FOUND, "no such action").into_response();
    }
    let stream =
        futures_util::stream::unfold((id, q.after, false), |(id, after, done)| async move {
            if done {
                return None;
            }
            loop {
                let polled = {
                    let id = id.clone();
                    crate::offload::blocking(move || {
                        let db = Db::open()?;
                        // Status **before** events: the final event is written
                        // in the same transaction that ends the action, so a
                        // status read as over guarantees the batch read after it
                        // holds the final event.
                        let running = db
                            .engine_action(&id)?
                            .is_some_and(|r| r.status == STATUS_RUNNING);
                        let batch = db.engine_events_after(&id, after, 256)?;
                        Ok::<_, veld_core::db::DbError>((batch, running))
                    })
                    .await
                };
                // A database error must not read as "the action is over": abort
                // the body, so the face sees a broken stream and resumes from
                // `after` instead of reporting an outcome it never got.
                let (batch, running) = match polled {
                    Ok(p) => p,
                    Err(e) => {
                        return Some((
                            Err(std::io::Error::other(e.to_string())),
                            (id, after, true),
                        ));
                    }
                };
                if batch.is_empty() {
                    if !running {
                        // Ended, and every event has been sent.
                        return None;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
                let mut out = String::new();
                let mut last = after;
                let mut finished = false;
                for (seq, json) in batch {
                    last = seq;
                    finished |= json.contains(veld_engine::api::FINISHED_TAG);
                    out.push_str(&format!(r#"{{"seq":{seq},"event":{json}}}"#));
                    out.push('\n');
                }
                return Some((Ok::<_, std::io::Error>(out), (id, last, finished)));
            }
        });
    (
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response()
}

/// Ctrl-C from a face: SIGINT to the worker, which cleans up what it started and
/// ends with `cancelled`. Only while its lock is held, so a recycled PID is never
/// signalled.
async fn cancel(headers: HeaderMap, UrlPath(id): UrlPath<String>) -> Response {
    if let Err(code) = check_csrf(&headers) {
        return code.into_response();
    }
    let row = {
        let id = id.clone();
        crate::offload::blocking(move || Db::open().and_then(|db| db.engine_action(&id)))
            .await
            .ok()
            .flatten()
    };
    let Some(row) = row else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no such action" })),
        )
            .into_response();
    };
    if row.status != STATUS_RUNNING {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "this action has already ended" })),
        )
            .into_response();
    }
    // Only a start (or a restart's start phase) can be undone. A stop or a node
    // action runs to its end: cutting a teardown short leaves containers behind.
    let cancellable = serde_json::from_str::<ActionRequest>(&row.request_json)
        .is_ok_and(|req| req.kind.cancellable());
    if !cancellable {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "this action cannot be interrupted; it is finishing" })),
        )
            .into_response();
    }
    match row.worker_pid {
        Some(pid) if pid > 1 && veld_core::engine_lock::is_held(&id) => {
            // SAFETY: kill only sends a signal.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
            StatusCode::ACCEPTED.into_response()
        }
        _ => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "the engine worker is not reachable yet (still starting, or just finishing)"
            })),
        )
            .into_response(),
    }
}

async fn save_var_answers(headers: HeaderMap, Json(mut req): Json<SaveVarAnswers>) -> Response {
    if let Err(code) = check_csrf(&headers) {
        return code.into_response();
    }
    let r = async {
        req.project_root = project_root_of(&req.project_root)?;
        crate::offload::blocking(move || veld_engine::preflight::save_var_answers(&req))
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
    }
    .await;
    into_response(r)
}

async fn resolve_action(headers: HeaderMap, Json(mut req): Json<ResolveActionRequest>) -> Response {
    if let Err(code) = check_csrf(&headers) {
        return code.into_response();
    }
    let r = async {
        req.project_root = project_root_of(&req.project_root)?;
        let (config_path, cfg) = veld_engine::preflight::load_config(&req.project_root)
            .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
        let root = req.project_root.clone();
        let state = crate::offload::blocking(move || {
            Db::open().and_then(|db| db.load_project_state(&root))
        })
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        // The node's `{ "env": … }` sources read the asking terminal's values,
        // never this daemon's own environment.
        let env = std::mem::take(&mut req.env);
        veld_core::values::with_caller_env(
            env,
            veld_engine::run_action::resolve(
                &cfg,
                &config_path,
                &state,
                &req.action,
                req.run,
                req.node.as_deref(),
                req.display_only,
            ),
        )
        .await
        .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))
    }
    .await;
    into_response(r)
}

#[derive(Deserialize)]
struct EnvNamesQuery {
    project_root: PathBuf,
}

/// The env names a project reads with `{ "env": … }` — what a face may send.
async fn env_names(Query(q): Query<EnvNamesQuery>) -> Response {
    let r = async {
        let root = project_root_of(&q.project_root)?;
        let (_, cfg) = veld_engine::preflight::load_config(&root)
            .map_err(|e| err(StatusCode::UNPROCESSABLE_ENTITY, e))?;
        crate::offload::blocking(move || {
            Db::open()
                .map(|db| veld_engine::preflight::env_names(&cfg, &db, &root))
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        })
        .await
    }
    .await;
    into_response(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_engine::api::ActionKind;

    /// A stored request of `kind`, as `spawn_action` writes it.
    fn stored(kind: ActionKind) -> String {
        serde_json::to_string(&ActionRequest {
            project_root: "/p".into(),
            kind,
            debug: false,
        })
        .unwrap()
    }

    fn start() -> String {
        stored(ActionKind::Start {
            run: "dev".into(),
            selections: vec![],
            origin_preset: None,
            oneshot: None,
            all_logs: false,
        })
    }

    // The guard serialises a process-wide env var for the whole test, awaits included.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_worker_without_its_lock_is_lost_only_after_the_grace() {
        let _env = crate::feedback_server::lock_db_env();
        let dir = tempfile::TempDir::new().unwrap();
        // SAFETY: lock_db_env serialises every writer of these variables.
        unsafe {
            std::env::set_var("VELD_DB_PATH", dir.path().join("t.db"));
            std::env::set_var("VELD_ENGINE_DIR", dir.path().join("engine"));
        }
        let db = Db::open().unwrap();
        db.insert_engine_action("a", "/p", "dev", "start", &start())
            .unwrap();

        // Just spawned: a free lock is not yet evidence of anything.
        sweep_lost_with(3600).await;
        assert_eq!(
            db.engine_action("a").unwrap().unwrap().status,
            STATUS_RUNNING
        );
        assert!(has_running_action(&db, Path::new("/p"), "dev"));

        // A live worker's lock keeps it running past the grace.
        let lock = veld_core::engine_lock::WorkerLock::acquire("a").unwrap();
        sweep_lost_with(0).await;
        assert_eq!(
            db.engine_action("a").unwrap().unwrap().status,
            STATUS_RUNNING
        );
        drop(lock);

        // Its worker is gone: lost, with a final event saying why.
        sweep_lost_with(0).await;
        assert_eq!(db.engine_action("a").unwrap().unwrap().status, STATUS_LOST);
        assert!(!has_running_action(&db, Path::new("/p"), "dev"));
        let events = db.engine_events_after("a", 0, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].1.contains("worker_lost"), "{events:?}");
        unsafe {
            std::env::remove_var("VELD_DB_PATH");
            std::env::remove_var("VELD_ENGINE_DIR");
        }
    }

    async fn call(req: axum::http::Request<axum::body::Body>) -> (StatusCode, String) {
        use tower::ServiceExt;
        // Both engine routers, merged as the daemon serves them.
        let app = routes().merge(super::super::engine_query::routes());
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The DNS-rebinding guard: a page rebound to 127.0.0.1 still says its own
    /// name in `Host`, and must not reach a route that runs a `veld.json`.
    #[tokio::test]
    async fn every_engine_route_refuses_a_foreign_host() {
        for (method, uri) in [
            ("POST", "/api/engine/actions"),
            ("POST", "/api/engine/preflight"),
            ("POST", "/api/engine/resolve-action"),
            ("POST", "/api/engine/var-answers"),
            ("POST", "/api/engine/actions/x/cancel"),
            ("GET", "/api/engine/env-names?project_root=/"),
            ("GET", "/api/engine/actions/x/events"),
            ("GET", "/api/engine/status?project_root=/"),
            ("GET", "/api/engine/logs?project_root=/"),
        ] {
            let req = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "evil.example:19899")
                .header("x-veld-request", "1")
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap();
            assert_eq!(call(req).await.0, StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    // The guard serialises a process-wide env var for the whole test, awaits included.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn events_resume_after_a_seq_and_end_after_finished() {
        let _env = crate::feedback_server::lock_db_env();
        let dir = tempfile::TempDir::new().unwrap();
        // SAFETY: lock_db_env serialises every writer of this variable.
        unsafe { std::env::set_var("VELD_DB_PATH", dir.path().join("t.db")) };
        let db = Db::open().unwrap();
        db.insert_engine_action("e", "/p", "dev", "stop", "{}")
            .unwrap();
        let notice = r#"{"type":"notice","message":"one","level":"info"}"#;
        let first = db.append_engine_event("e", notice).unwrap();
        db.append_engine_event("e", r#"{"type":"notice","message":"two","level":"info"}"#)
            .unwrap();
        db.finish_engine_action_with_event(
            "e",
            veld_core::db::engine_actions::STATUS_SUCCEEDED,
            "{}",
            r#"{"type":"finished","outcome":{"result":"stopped","stopped":["dev"]}}"#,
        )
        .unwrap();

        let get = |after: i64| {
            axum::http::Request::builder()
                .uri(format!("/api/engine/actions/e/events?after={after}"))
                .header("host", "127.0.0.1:19899")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let (status, body) = call(get(first)).await;
        assert_eq!(status, StatusCode::OK);
        let lines: Vec<veld_engine::api::EventEnvelope> = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "{body}");
        assert!(lines[0].seq > first);
        assert!(matches!(lines[1].event, EngineEvent::Finished { .. }));

        // Read again past the end: an action that is over closes at once.
        let (status, body) = call(get(lines[1].seq)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty(), "{body}");
        unsafe { std::env::remove_var("VELD_DB_PATH") };
    }

    // The guard serialises a process-wide env var for the whole test, awaits included.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_stop_cannot_be_cancelled_and_node_actions_do_not_gate_recovery() {
        let _env = crate::feedback_server::lock_db_env();
        let dir = tempfile::TempDir::new().unwrap();
        // SAFETY: lock_db_env serialises every writer of these variables.
        unsafe {
            std::env::set_var("VELD_DB_PATH", dir.path().join("t.db"));
            std::env::set_var("VELD_ENGINE_DIR", dir.path().join("engine"));
        }
        let db = Db::open().unwrap();
        db.insert_engine_action(
            "s",
            "/p",
            "dev",
            "stop",
            &stored(ActionKind::Stop { run: "dev".into() }),
        )
        .unwrap();
        let _lock = veld_core::engine_lock::WorkerLock::acquire("s").unwrap();
        db.set_engine_action_pid("s", std::process::id()).unwrap();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/engine/actions/s/cancel")
            .header("host", "127.0.0.1")
            .header("x-veld-request", "1")
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, body) = call(req).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("cannot be interrupted"), "{body}");

        // A node action holds no run's lifecycle; a stop --all holds every run's.
        db.insert_engine_action(
            "a",
            "/q",
            "dev",
            "action",
            &stored(ActionKind::Action {
                run: "dev".into(),
                action: "psql".into(),
                node: None,
            }),
        )
        .unwrap();
        assert!(!has_running_action(&db, Path::new("/q"), "dev"));
        db.insert_engine_action("all", "/r", "", "stop_all", &stored(ActionKind::StopAll))
            .unwrap();
        assert!(has_running_action(&db, Path::new("/r"), "anything"));
        unsafe {
            std::env::remove_var("VELD_DB_PATH");
            std::env::remove_var("VELD_ENGINE_DIR");
        }
    }

    /// The daemon runs the engine in workers of its own binary; it never execs
    /// the `veld` CLI. That exec is what made a `veld stop` the daemon waited on
    /// call back into the daemon — the circular wait this design removed.
    #[test]
    fn the_daemon_never_execs_the_veld_cli() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        let mut offenders = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    // Built at run time, so this file's own source does not match
                    // and needs no exemption.
                    let needles = [
                        format!("Command::new({}veld{})", '"', '"'),
                        format!("find_veld_{}(", "binary"),
                        format!("spawn_{}(", "veld"),
                    ];
                    for needle in &needles {
                        if text.contains(needle.as_str()) {
                            offenders.push(format!("{}: {needle}", path.display()));
                        }
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "{offenders:#?}");
    }
}
