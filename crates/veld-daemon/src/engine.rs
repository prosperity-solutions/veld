//! The engine's home in the daemon: spawning workers, streaming what they say,
//! noticing when one dies, and the `/api/engine/*` routes every face talks to.
//!
//! **One engine per machine, many faces.** `veld orchestrator start` in a terminal, the IDE's
//! start button, the monitor's crash recovery and the trash flow's teardown all
//! come through [`spawn_action`]. None of them execs a `veld` binary any more:
//! the daemon spawns `veld-daemon --engine-worker` (see `engine_worker.rs`) and
//! the worker runs the engine.
//!
//! **Nothing here waits on a worker.** A handler answers as soon as the worker is
//! spawned; a face that wants the outcome reads the action's event log, which
//! the worker writes directly. That is what lets a worker outlive this daemon
//! (an update restarts it), and what removed the circular wait the old
//! `spawn_veld` had to dodge (a `veld orchestrator stop` the daemon waited on, calling back
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
use veld_core::db::engine_actions::{
    EngineActionKey, EngineActionReservation, EngineActionRow, STATUS_FAILED, STATUS_LOST,
    STATUS_RUNNING,
};
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
/// Refuses with `503` while `veld self update` holds the update lock — the answer a
/// UI already shows as "try again shortly", and the honest one. A worker already
/// running keeps running across the update.
pub(crate) async fn spawn_action(
    request: ActionRequest,
    secrets: ActionSecrets,
) -> Result<String, ApiError> {
    spawn_action_with_key(request, secrets, None).await
}

/// Submit once per authenticated client/key and public request. Secrets are
/// delivered only on the first spawn and never participate in retry comparison.
/// A reserved action without a worker remains pending until the supervisor marks
/// it lost; a retry must never guess whether the original worker executed.
pub(crate) async fn spawn_action_with_key(
    request: ActionRequest,
    secrets: ActionSecrets,
    key: Option<EngineActionKey>,
) -> Result<String, ApiError> {
    spawn_action_bound(request, secrets, key, None).await
}

/// The public UI intent, before presets and worktree aliases are resolved.
/// Only route handlers with public, typed payloads may construct this binding.
pub(crate) struct UiSubmission {
    key: Option<EngineActionKey>,
    binding: serde_json::Value,
}

impl UiSubmission {
    pub(crate) fn new(
        headers: &HeaderMap,
        route: &str,
        payload: impl serde::Serialize,
    ) -> Result<Self, ApiError> {
        let payload = serde_json::to_value(payload)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        Ok(Self {
            key: action_key(headers)?,
            binding: serde_json::json!({ "route": route, "payload": payload }),
        })
    }

    /// Replay before consulting mutable config, registry entries, or live runs.
    pub(crate) async fn replay(&self) -> Result<Option<String>, ApiError> {
        match self.key {
            Some(key) => existing_submission(&key.action_id(), "", Some(&self.binding)).await,
            None => Ok(None),
        }
    }
}

async fn existing_submission(
    id: &str,
    request_json: &str,
    binding: Option<&serde_json::Value>,
) -> Result<Option<String>, ApiError> {
    let Some(existing) = stored_action(id).await? else {
        return Ok(None);
    };
    let same = match binding {
        Some(binding) => {
            let stored: serde_json::Value = serde_json::from_str(&existing.request_json)
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            stored.get("ui_submission") == Some(binding)
        }
        None => existing.request_json == request_json,
    };
    if same {
        Ok(Some(existing.id))
    } else {
        Err(key_conflict())
    }
}

pub(crate) async fn spawn_ui_action(
    project_root: &Path,
    kind: veld_engine::api::ActionKind,
    submission: UiSubmission,
) -> Result<String, ApiError> {
    spawn_action_bound(
        ActionRequest {
            project_root: project_root.to_owned(),
            kind,
            debug: false,
        },
        ActionSecrets::default(),
        submission.key,
        Some(submission.binding),
    )
    .await
}

/// A successful submit always returns a receipt, including legacy UI routes.
pub(crate) fn submitted(id: String) -> (StatusCode, Json<Submitted>) {
    (StatusCode::ACCEPTED, Json(Submitted { action_id: id }))
}

async fn spawn_action_bound(
    request: ActionRequest,
    secrets: ActionSecrets,
    key: Option<EngineActionKey>,
    binding: Option<serde_json::Value>,
) -> Result<String, ApiError> {
    let id = key.map_or_else(|| uuid::Uuid::new_v4().to_string(), |key| key.action_id());
    let request_json = if let Some(binding) = &binding {
        let mut stored = serde_json::to_value(&request)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        stored["ui_submission"] = binding.clone();
        serde_json::to_string(&stored)
    } else {
        serde_json::to_string(&request)
    }
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // Recover an existing submission even during an update, without resolving
    // a shell PATH or requiring the worker's transient secrets again.
    if key.is_some()
        && let Some(existing) = existing_submission(&id, &request_json, binding.as_ref()).await?
    {
        return Ok(existing);
    }
    if veld_core::update_lock::current().is_some() {
        warn!(
            kind = request.kind.label(),
            "refusing an engine action: an update is in progress"
        );
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "a Veld update is in progress — try again when it finishes",
        ));
    }
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

    let reservation = {
        let id = id.clone();
        let root = project_root.to_string_lossy().into_owned();
        let run = request.kind.run().unwrap_or_default().to_owned();
        let kind = request.kind.label();
        let request_json = request_json.clone();
        crate::offload::blocking(move || {
            let db = Db::open()?;
            match key {
                Some(key) => db.reserve_engine_action(key, &root, &run, kind, &request_json),
                None => {
                    db.insert_engine_action(&id, &root, &run, kind, &request_json)?;
                    Ok(EngineActionReservation::Reserved)
                }
            }
        })
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };
    match reservation {
        EngineActionReservation::Existing => return Ok(id),
        EngineActionReservation::Conflict => {
            // Concurrent preflights can resolve the same UI intent differently
            // after a config edit. Only the reservation winner gets to execute.
            return existing_submission(&id, &request_json, binding.as_ref())
                .await?
                .ok_or_else(key_conflict);
        }
        EngineActionReservation::Reserved => {}
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

fn key_conflict() -> ApiError {
    err(
        StatusCode::CONFLICT,
        "this Idempotency-Key already belongs to a different engine request",
    )
}

async fn stored_action(id: &str) -> Result<Option<EngineActionRow>, ApiError> {
    let id = id.to_owned();
    crate::offload::blocking(move || Db::open().and_then(|db| db.engine_action(&id)))
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
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
async fn spawn_worker(_: &Path, _: &str, input: &WorkerInput) -> std::io::Result<u32> {
    SPAWNED_ACTIONS
        .lock()
        .unwrap()
        .push(input.action_id.clone());
    Ok(0)
}

#[cfg(test)]
static SPAWNED_ACTIONS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

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
    submission: UiSubmission,
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
                 `veld orchestrator vars set`",
                names.join(", ")
            ),
        ));
    }
    spawn_ui_action(project_root, ready.action, submission).await
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
                message: "The engine worker is unavailable. The action may not have started or may \
                          have stopped before reporting its outcome. Check `veld orchestrator status` \
                          before submitting a new action. Retrying the same key does not restart it."
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
        .route("/api/engine/actions/{id}", get(operation))
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
        let key = action_key(&headers)?;
        if let Some(key) = key
            && let Some(existing) = stored_action(&key.action_id()).await?
        {
            let stored: serde_json::Value = serde_json::from_str(&existing.request_json)
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            if stored.get("ui_submission").is_some() {
                return Err(key_conflict());
            }
            let original: ActionRequest = serde_json::from_str(&existing.request_json)
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            // An exact replay still works after the project/config disappears.
            // Alternate path spellings must resolve to the original project.
            if body.request.project_root != original.project_root {
                body.request.project_root =
                    project_root_of(&body.request.project_root).map_err(|_| key_conflict())?;
            }
            if body.request != original {
                return Err(key_conflict());
            }
            return Ok(Submitted {
                action_id: existing.id,
            });
        }
        body.request.project_root = project_root_of(&body.request.project_root)?;
        let action_id = spawn_action_with_key(body.request, body.secrets, key).await?;
        Ok(Submitted { action_id })
    }
    .await;
    match r {
        Ok(s) => (StatusCode::ACCEPTED, Json(s)).into_response(),
        Err(e) => into_response::<()>(Err(e)),
    }
}

fn uuid_header(headers: &HeaderMap, name: &'static str) -> Result<Option<uuid::Uuid>, ApiError> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("duplicate {name} header"),
        ));
    }
    let value = value
        .to_str()
        .ok()
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("{name} must be a UUID")))?;
    Ok(Some(value))
}

/// Parse retry headers after ingress replaces the remote identity with its
/// authenticated client UUID. With neither header, keep the local behavior.
pub(crate) fn action_key(headers: &HeaderMap) -> Result<Option<EngineActionKey>, ApiError> {
    match (
        uuid_header(headers, "x-veld-remote-client")?,
        uuid_header(headers, "idempotency-key")?,
    ) {
        (None, None) => Ok(None),
        // The nil namespace belongs only to local submissions. Ingress supplies
        // a paired client's generated UUID for every remote request.
        (None, Some(idempotency_key)) => Ok(Some(EngineActionKey {
            client_id: uuid::Uuid::nil(),
            idempotency_key,
        })),
        (Some(client_id), Some(idempotency_key)) => Ok(Some(EngineActionKey {
            client_id,
            idempotency_key,
        })),
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            "remote engine actions require both X-Veld-Remote-Client and Idempotency-Key",
        )),
    }
}

fn check_action_scope(headers: &HeaderMap, id: &str) -> Result<(), ApiError> {
    let client_id = uuid_header(headers, "x-veld-remote-client")?;
    let allowed = match client_id {
        Some(client_id) => EngineActionKey::belongs_to(id, client_id),
        None => !id.starts_with("remote-") || EngineActionKey::belongs_to(id, uuid::Uuid::nil()),
    };
    if allowed {
        Ok(())
    } else {
        Err(err(StatusCode::NOT_FOUND, "no such action"))
    }
}

/// Public recovery receipt. A reservation without a worker PID is pending,
/// never proof that execution started. No request, secrets, or process data leave
/// this route. Retention follows the existing seven-day finished-action pruning.
async fn operation(headers: HeaderMap, UrlPath(id): UrlPath<String>) -> Response {
    let r = async {
        check_action_scope(&headers, &id)?;
        let row = stored_action(&id)
            .await?
            .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such action"))?;
        let outcome = row
            .outcome_json
            .as_deref()
            .map(serde_json::from_str::<ActionOutcome>)
            .transpose()
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let status = if row.status == STATUS_RUNNING && row.worker_pid.is_none() {
            "pending"
        } else {
            &row.status
        };
        Ok(serde_json::json!({
            "action_id": row.id,
            "status": status,
            "outcome": outcome,
            "created_at": row.created_at,
            "ended_at": row.ended_at,
        }))
    }
    .await;
    into_response(r)
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
async fn events(
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<EventsQuery>,
) -> Response {
    if let Err(e) = check_action_scope(&headers, &id) {
        return into_response::<()>(Err(e));
    }
    let exists = match stored_action(&id).await {
        Ok(row) => row.is_some(),
        Err(e) => return into_response::<()>(Err(e)),
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
    if let Err(e) = check_action_scope(&headers, &id) {
        return into_response::<()>(Err(e));
    }
    let row = match stored_action(&id).await {
        Ok(row) => row,
        Err(e) => return into_response::<()>(Err(e)),
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

    struct ActionTestEnv {
        dir: tempfile::TempDir,
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl ActionTestEnv {
        // The caller holds lock_db_env for this fixture's whole lifetime.
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            std::fs::write(dir.path().join("veld.json"), "{}").unwrap();
            let previous = [
                ("VELD_DB_PATH", dir.path().join("t.db")),
                ("VELD_ENGINE_DIR", dir.path().join("engine")),
            ]
            .into_iter()
            .map(|(name, value)| {
                let previous = std::env::var_os(name);
                // SAFETY: the caller holds the crate-wide process-state guard.
                unsafe { std::env::set_var(name, value) };
                (name, previous)
            })
            .collect();
            Self { dir, previous }
        }
    }

    impl Drop for ActionTestEnv {
        fn drop(&mut self) {
            for (name, value) in &self.previous {
                // SAFETY: the caller still holds the process-state guard.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    fn retry_key() -> EngineActionKey {
        EngineActionKey {
            client_id: uuid::Uuid::new_v4(),
            idempotency_key: uuid::Uuid::new_v4(),
        }
    }

    fn submission(
        project_root: &Path,
        key: Option<EngineActionKey>,
        run: &str,
        secret: &str,
    ) -> axum::http::Request<Body> {
        let body = SubmitAction {
            request: ActionRequest {
                project_root: project_root.to_owned(),
                kind: ActionKind::Stop { run: run.into() },
                debug: false,
            },
            secrets: ActionSecrets {
                env: [("SYNTHETIC_TOKEN".into(), secret.into())].into(),
                ..Default::default()
            },
        };
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/engine/actions")
            .header("host", "127.0.0.1")
            .header("x-veld-request", "1")
            .header("content-type", "application/json");
        if let Some(key) = key {
            request = request
                .header("x-veld-remote-client", key.client_id.to_string())
                .header("idempotency-key", key.idempotency_key.to_string());
        }
        request
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    async fn lookup(id: &str, client: uuid::Uuid) -> (StatusCode, serde_json::Value) {
        let (status, body) = call(
            axum::http::Request::builder()
                .uri(format!("/api/engine/actions/{id}"))
                .header("host", "127.0.0.1")
                .header("x-veld-remote-client", client.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        (status, serde_json::from_str(&body).unwrap())
    }

    fn spawn_count(id: &str) -> usize {
        SPAWNED_ACTIONS
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| *seen == id)
            .count()
    }

    #[test]
    fn remote_submissions_require_both_single_uuid_headers() {
        assert!(action_key(&HeaderMap::new()).unwrap().is_none());
        let key = retry_key();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-veld-remote-client",
            key.client_id.to_string().parse().unwrap(),
        );
        assert_eq!(action_key(&headers).unwrap_err().0, StatusCode::BAD_REQUEST);
        headers.insert("idempotency-key", "invalid".parse().unwrap());
        assert_eq!(action_key(&headers).unwrap_err().0, StatusCode::BAD_REQUEST);
        headers.insert(
            "idempotency-key",
            key.idempotency_key.to_string().parse().unwrap(),
        );
        assert_eq!(
            action_key(&headers).unwrap().unwrap().action_id(),
            key.action_id()
        );
        headers.append(
            "idempotency-key",
            key.idempotency_key.to_string().parse().unwrap(),
        );
        assert_eq!(action_key(&headers).unwrap_err().0, StatusCode::BAD_REQUEST);
        headers.remove("x-veld-remote-client");
        headers.insert(
            "idempotency-key",
            key.idempotency_key.to_string().parse().unwrap(),
        );
        let local = action_key(&headers).unwrap().unwrap();
        assert_eq!(local.client_id, uuid::Uuid::nil());
        assert_eq!(local.idempotency_key, key.idempotency_key);
        headers.insert("x-veld-remote-client", "invalid".parse().unwrap());
        assert_eq!(action_key(&headers).unwrap_err().0, StatusCode::BAD_REQUEST);
        headers.insert(
            "x-veld-remote-client",
            key.client_id.to_string().parse().unwrap(),
        );
        headers.append(
            "x-veld-remote-client",
            key.client_id.to_string().parse().unwrap(),
        );
        assert_eq!(action_key(&headers).unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    fn ui_request(
        method: &str,
        uri: &str,
        key: Option<EngineActionKey>,
        body: serde_json::Value,
    ) -> axum::http::Request<Body> {
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1")
            .header("x-veld-request", "1")
            .header("content-type", "application/json");
        if let Some(key) = key {
            request = request.header("idempotency-key", key.idempotency_key.to_string());
            if !key.client_id.is_nil() {
                request = request.header("x-veld-remote-client", key.client_id.to_string());
            }
        }
        request.body(Body::from(body.to_string())).unwrap()
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn ui_routes_replay_before_reading_worktrees_registry_or_config() {
        let _guard = crate::http_server::lock_db_env();
        let env = ActionTestEnv::new();
        let start = serde_json::json!({ "preset": "dev", "selections": [], "run_name": "dev" });
        let action = serde_json::json!({ "action": "build", "node": null });
        for (route, uri, payload, body, kind) in [
            (
                "/api/worktrees/42/start",
                "/api/worktrees/42/start",
                start.clone(),
                start,
                ActionKind::Start {
                    run: "dev".into(),
                    selections: vec!["web:local".into()],
                    origin_preset: Some("dev".into()),
                    oneshot: None,
                    all_logs: false,
                },
            ),
            (
                "/api/environments/dev/stop",
                "/api/environments/dev/stop?project_root=/untracked",
                serde_json::json!({ "project_root": "/untracked" }),
                serde_json::Value::Null,
                ActionKind::Stop { run: "dev".into() },
            ),
            (
                "/api/environments/dev/restart",
                "/api/environments/dev/restart?project_root=/untracked",
                serde_json::json!({ "project_root": "/untracked" }),
                serde_json::Value::Null,
                ActionKind::Restart { run: "dev".into() },
            ),
            (
                "/api/environments/dev/action",
                "/api/environments/dev/action?project_root=/untracked",
                serde_json::json!({ "project_root": "/untracked", "body": action }),
                action,
                ActionKind::Action {
                    run: "dev".into(),
                    action: "build".into(),
                    node: None,
                },
            ),
        ] {
            for local in [false, true] {
                let mut key = retry_key();
                if local {
                    key.client_id = uuid::Uuid::nil();
                }
                let request = ui_request("POST", uri, Some(key), body.clone());
                let submission = UiSubmission::new(request.headers(), route, &payload).unwrap();
                let id = spawn_ui_action(env.dir.path(), kind.clone(), submission)
                    .await
                    .unwrap();
                for _ in 0..2 {
                    let (status, receipt) =
                        call(ui_request("POST", uri, Some(key), body.clone())).await;
                    assert_eq!(status, StatusCode::ACCEPTED, "{uri}: {receipt}");
                    assert_eq!(
                        serde_json::from_str::<Submitted>(&receipt)
                            .unwrap()
                            .action_id,
                        id
                    );
                }
                assert_eq!(spawn_count(&id), 1);
                let conflicting = UiSubmission::new(
                    request.headers(),
                    route,
                    serde_json::json!({ "changed": true }),
                )
                .unwrap();
                assert_eq!(
                    conflicting.replay().await.unwrap_err().0,
                    StatusCode::CONFLICT
                );
            }
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn concurrent_ui_intents_keep_the_reservation_winners_resolved_request() {
        let _guard = crate::http_server::lock_db_env();
        let env = ActionTestEnv::new();
        let key = retry_key();
        let request = ui_request(
            "POST",
            "/api/worktrees/42/start",
            Some(key),
            serde_json::json!({}),
        );
        let first = UiSubmission::new(
            request.headers(),
            "/api/worktrees/42/start",
            serde_json::json!({ "preset": "dev" }),
        )
        .unwrap();
        let second = UiSubmission::new(
            request.headers(),
            "/api/worktrees/42/start",
            serde_json::json!({ "preset": "dev" }),
        )
        .unwrap();
        let (one, two) = tokio::join!(
            spawn_ui_action(
                env.dir.path(),
                ActionKind::Stop {
                    run: "resolved-before-edit".into()
                },
                first
            ),
            spawn_ui_action(
                env.dir.path(),
                ActionKind::Stop {
                    run: "resolved-after-edit".into()
                },
                second
            ),
        );
        assert_eq!(one.unwrap(), two.unwrap());
        assert_eq!(spawn_count(&key.action_id()), 1);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn keyed_operation_events_and_cancel_require_the_owning_identity() {
        let _guard = crate::http_server::lock_db_env();
        let _env = ActionTestEnv::new();
        let key = retry_key();
        let id = key.action_id();
        let db = Db::open().unwrap();
        db.insert_engine_action(&id, "/p", "dev", "start", &start())
            .unwrap();
        db.finish_engine_action_with_event(
            &id,
            STATUS_LOST,
            r#"{"result":"unknown"}"#,
            r#"{"type":"finished","outcome":{"result":"unknown"}}"#,
        )
        .unwrap();
        for (method, suffix) in [("GET", ""), ("GET", "/events"), ("POST", "/cancel")] {
            let uri = format!("/api/engine/actions/{id}{suffix}");
            for principal in [None, Some(retry_key())] {
                assert_eq!(
                    call(ui_request(method, &uri, principal, serde_json::Value::Null))
                        .await
                        .0,
                    StatusCode::NOT_FOUND
                );
            }
            let expected = if method == "POST" {
                StatusCode::CONFLICT
            } else {
                StatusCode::OK
            };
            assert_eq!(
                call(ui_request(method, &uri, Some(key), serde_json::Value::Null))
                    .await
                    .0,
                expected
            );
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn remote_retries_spawn_once_ignore_secrets_and_expose_public_outcomes() {
        let _guard = crate::http_server::lock_db_env();
        let env = ActionTestEnv::new();
        let root = env.dir.path();
        let key = retry_key();
        let id = key.action_id();
        let (first, second) = tokio::join!(
            call(submission(root, Some(key), "dev", "synthetic-first")),
            call(submission(root, Some(key), "dev", "synthetic-second")),
        );
        for (status, body) in [first, second] {
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            assert_eq!(
                serde_json::from_str::<Submitted>(&body).unwrap().action_id,
                id
            );
        }
        assert_eq!(spawn_count(&id), 1);
        assert_eq!(
            call(submission(root, Some(key), "other", "synthetic-third"))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(spawn_count(&id), 1);
        let (status, receipt) = lookup(&id, key.client_id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(receipt["status"], "pending");
        assert!(receipt["outcome"].is_null());
        assert!(receipt.get("request").is_none());
        assert!(receipt.get("worker_pid").is_none());
        assert_eq!(
            lookup(&id, uuid::Uuid::new_v4()).await.0,
            StatusCode::NOT_FOUND
        );

        let db = Db::open().unwrap();
        let row = db.engine_action(&id).unwrap().unwrap();
        assert!(!row.request_json.contains("synthetic-"));
        assert!(!row.request_json.contains("SYNTHETIC_TOKEN"));
        db.set_engine_action_pid(&id, 123).unwrap();
        assert_eq!(lookup(&id, key.client_id).await.1["status"], "running");
        let outcome = ActionOutcome::Stopped {
            stopped: vec!["dev".into()],
            already: false,
        };
        db.finish_engine_action(
            &id,
            veld_core::db::engine_actions::STATUS_SUCCEEDED,
            &serde_json::to_string(&outcome).unwrap(),
        )
        .unwrap();
        let (_, receipt) = lookup(&id, key.client_id).await;
        assert_eq!(receipt["status"], "succeeded");
        assert_eq!(receipt["outcome"]["result"], "stopped");
        assert!(receipt["ended_at"].is_string());
        assert_eq!(
            call(submission(root, Some(key), "dev", "synthetic-fourth"))
                .await
                .0,
            StatusCode::ACCEPTED
        );
        assert_eq!(spawn_count(&id), 1);

        let (_, first) = call(submission(root, None, "local", "synthetic-local")).await;
        let (_, second) = call(submission(root, None, "local", "synthetic-local")).await;
        let first: Submitted = serde_json::from_str(&first).unwrap();
        let second: Submitted = serde_json::from_str(&second).unwrap();
        assert_ne!(first.action_id, second.action_id);
        assert_eq!(spawn_count(&first.action_id), 1);
        assert_eq!(spawn_count(&second.action_id), 1);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_reservation_abandoned_before_spawn_is_never_restarted() {
        let _guard = crate::http_server::lock_db_env();
        let env = ActionTestEnv::new();
        let root = project_root_of(env.dir.path()).unwrap();
        let key = retry_key();
        let id = key.action_id();
        let request = ActionRequest {
            project_root: root.clone(),
            kind: ActionKind::Stop { run: "dev".into() },
            debug: false,
        };
        let db = Db::open().unwrap();
        db.reserve_engine_action(
            key,
            root.to_str().unwrap(),
            "dev",
            "stop",
            &serde_json::to_string(&request).unwrap(),
        )
        .unwrap();
        drop(db);
        assert_eq!(
            call(submission(&root, Some(key), "dev", "synthetic-secret"))
                .await
                .0,
            StatusCode::ACCEPTED
        );
        assert_eq!(lookup(&id, key.client_id).await.1["status"], "pending");
        assert_eq!(spawn_count(&id), 0);
        std::fs::remove_file(root.join("veld.json")).unwrap();
        sweep_lost_with(0).await;
        let (status, receipt) = lookup(&id, key.client_id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(receipt["status"], "lost");
        assert_eq!(receipt["outcome"]["kind"], "worker_lost");
        assert_eq!(
            call(submission(&root, Some(key), "dev", "synthetic-secret"))
                .await
                .0,
            StatusCode::ACCEPTED
        );
        assert_eq!(spawn_count(&id), 0);
    }

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
        let _env = crate::http_server::lock_db_env();
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
        let app = routes()
            .merge(super::super::engine_query::routes())
            .merge(super::super::management::routes())
            .merge(super::super::desktop::routes());
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
            ("GET", "/api/engine/actions/x"),
            ("POST", "/api/engine/preflight"),
            ("POST", "/api/engine/resolve-action"),
            ("POST", "/api/engine/var-answers"),
            ("POST", "/api/engine/actions/x/cancel"),
            ("GET", "/api/engine/env-names?project_root=/"),
            ("GET", "/api/engine/actions/x/events"),
            ("GET", "/api/engine/status?project_root=/"),
            ("GET", "/api/engine/logs?project_root=/"),
            ("GET", "/api/engine/overview"),
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
        let _env = crate::http_server::lock_db_env();
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
        let _env = crate::http_server::lock_db_env();
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
    /// the `veld` CLI. That exec is what made a `veld orchestrator stop` the daemon waited on
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
