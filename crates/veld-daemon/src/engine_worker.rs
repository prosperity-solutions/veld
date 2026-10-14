//! `veld-daemon --engine-worker`: one engine action, in a process of its own.
//!
//! The daemon spawns one of these per start/stop/restart/action, in its own
//! process group, with the action on stdin. Same pattern as the PTY holder and
//! for the same reason: launchd's and systemd's restarts take the daemon's group
//! down, and a run half-started when `veld update` restarts the daemon must not
//! be cut in half. The worker finishes the action whatever happens to the daemon
//! and writes everything it has to say into the action's event log, where
//! whichever daemon is up streams it to the face that asked.
//!
//! What the worker gets and nothing else:
//! - **stdin**: [`WorkerInput`] — the request *and* its secrets (the caller's
//!   declared env values, var answers). The one channel that is not persisted,
//!   not in `ps`, and not in a log.
//! - **cwd**: the project root. **`PATH`**: the user's login-shell `PATH` resolved
//!   in that directory, exactly what the daemon used to give a `veld start` it execed.
//!   Everything else is the daemon's own environment; nodes inherit it.
//!
//! stderr is the daemon's log (inherited), so tracing here is for operators;
//! anything a *user* must see is an event.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use veld_engine::api::{ActionOutcome, ActionRequest, ActionSecrets, EngineEvent, FailureKind};
use veld_engine::host::{EngineHost, HostFuture, ShareRelease};

/// What the daemon writes to a worker's stdin.
#[derive(Serialize, Deserialize)]
pub struct WorkerInput {
    pub action_id: String,
    pub request: ActionRequest,
    #[serde(default)]
    pub secrets: ActionSecrets,
}

/// The status column value for an outcome.
pub fn status_for(outcome: &ActionOutcome) -> &'static str {
    use veld_core::db::engine_actions::*;
    match outcome {
        ActionOutcome::Cancelled => STATUS_CANCELLED,
        ActionOutcome::Failed {
            kind: FailureKind::WorkerLost,
            ..
        } => STATUS_LOST,
        ActionOutcome::Failed { .. } | ActionOutcome::Unknown => STATUS_FAILED,
        _ => STATUS_SUCCEEDED,
    }
}

/// The worker's [`EngineHost`]: share release goes to the daemon over HTTP.
///
/// Over HTTP from a *separate process* is what makes this safe: the old
/// in-daemon equivalent (a `veld stop` the daemon waited on, calling back into the
/// daemon) was a circular wait. Nothing in the daemon waits on a worker.
struct WorkerHost {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl EngineHost for WorkerHost {
    fn cancel_requested(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn release_shares<'a>(&'a self, run_id: &'a str) -> HostFuture<'a, ShareRelease> {
        Box::pin(async move {
            let client = veld_core::share::DaemonClient::new();
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                client.unshare_run(run_id),
            )
            .await
            {
                Ok(Ok(n)) => ShareRelease::Released(n),
                Ok(Err(veld_core::share::DaemonError::NotRunning)) => ShareRelease::NotRunning,
                Ok(Err(e)) => ShareRelease::Failed(e.to_string()),
                Err(_) => ShareRelease::TimedOut,
            }
        })
    }

    fn command_stats_observer(
        &self,
        db: &veld_core::db::Db,
        project_root: &std::path::Path,
        run: &str,
    ) -> Option<Arc<dyn veld_core::stats::StepObserver>> {
        Some(Arc::new(veld_stats::CommandStatsRecorder::start(
            db.clone(),
            project_root.to_path_buf(),
            run.to_owned(),
        )))
    }
}

/// Writes events to the action's log. A write that fails is logged and dropped:
/// the action must not stop because its progress could not be recorded.
struct DbSink {
    db: veld_core::db::Db,
    action_id: String,
}

impl veld_engine::action::EventSink for DbSink {
    fn emit(&self, event: EngineEvent) {
        // The final event is written by `run` below, in the same transaction that
        // ends the action (`finish_engine_action_with_event`). One emitted here
        // would end a face's stream while the action is still `running`, and a
        // second, contradicting one would follow.
        if matches!(event, EngineEvent::Finished { .. }) {
            tracing::error!(action = %self.action_id, "engine worker: an action emitted its own `finished`; dropped");
            return;
        }
        let json = match serde_json::to_string(&event) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "engine worker: unserializable event dropped");
                return;
            }
        };
        if let Err(e) = self.db.append_engine_event(&self.action_id, &json) {
            tracing::warn!(action = %self.action_id, error = %e, "engine worker: event dropped");
        }
    }
}

/// Start remembering SIGINT, and stop it from being fatal to this process.
fn install_sigint_latch() -> Arc<std::sync::atomic::AtomicBool> {
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
        Ok(mut sig) => {
            let flag = Arc::clone(&flag);
            tokio::spawn(async move {
                while sig.recv().await.is_some() {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
        Err(e) => tracing::warn!(error = %e, "engine worker: could not take SIGINT"),
    }
    flag
}

/// The worker's whole life. Returns the process exit code.
pub async fn run() -> i32 {
    // Before anything can replace the binary: the log sink of every node this
    // worker spawns execs this path, and after an update Linux reports the old
    // inode as "… (deleted)".
    let _ = veld_core::process::own_exe();

    // SIGINT is how the daemon cancels a start. Latched here for the whole life
    // of the process, which also replaces SIGINT's default action: without it a
    // stray SIGINT during a stop or a teardown would kill this worker half-way
    // through cleaning up — the one thing a worker exists to never do. The
    // engine's own `ctrl_c` listeners still receive it. **Do not remove**: the
    // test `a_sigint_is_remembered_and_does_not_kill_the_worker` pins it.
    let cancelled = install_sigint_latch();

    let input: WorkerInput = {
        use tokio::io::AsyncReadExt;
        let mut buf = String::new();
        if let Err(e) = tokio::io::stdin().read_to_string(&mut buf).await {
            tracing::error!(error = %e, "engine worker: could not read its input");
            return 1;
        }
        match serde_json::from_str(&buf) {
            Ok(i) => i,
            Err(e) => {
                // Never log `buf`: it carries the caller's secrets.
                tracing::error!(error = %e, "engine worker: unreadable input");
                return 1;
            }
        }
    };
    let action_id = input.action_id.clone();

    let _lock = match veld_core::engine_lock::WorkerLock::acquire(&action_id) {
        Ok(lock) => lock,
        Err(e) => {
            tracing::error!(action = %action_id, error = %e, "engine worker: no lock, not running");
            return 1;
        }
    };
    let db = match veld_core::db::Db::open() {
        Ok(db) => db,
        Err(e) => {
            tracing::error!(action = %action_id, error = %e, "engine worker: no database");
            return 1;
        }
    };
    // A worker that took longer than the supervisor's grace to get here (a host
    // that slept between spawn and lock) may already have been declared lost.
    // Running the action now would do it behind the back of a face that was told
    // it failed.
    match db.engine_action(&action_id) {
        Ok(Some(row)) if row.status == veld_core::db::engine_actions::STATUS_RUNNING => {}
        _ => {
            tracing::warn!(action = %action_id, "engine worker: the action is no longer running; not starting it");
            return 1;
        }
    }
    let _ = db.set_engine_action_pid(&action_id, std::process::id());

    veld_core::values::set_caller_env(input.secrets.env.clone());

    let sink = Arc::new(DbSink {
        db: db.clone(),
        action_id: action_id.clone(),
    });
    let request_kind = input.request.kind.label();
    let request_run = input.request.kind.run().map(str::to_owned);
    let request_root = input.request.project_root.clone();
    tracing::info!(action = %action_id, kind = request_kind, "engine worker: start");
    let outcome = veld_engine::action::run(
        input.request,
        input.secrets,
        Arc::new(WorkerHost { cancelled }),
        sink.clone(),
    )
    .await;
    if let ActionOutcome::Failed { message, .. } = &outcome {
        // Said where an operator and a user will look: the daemon log, and the
        // run's own log (`veld logs --source veld`) — an IDE button or a crash
        // teardown has no terminal to print it on.
        tracing::warn!(action = %action_id, kind = request_kind, "engine action failed: {message}");
        if let Some(run) = &request_run {
            let log = veld_core::logging::LogWriter::for_run(
                db.clone(),
                &request_root,
                run,
                veld_core::db::LogStream::Internal,
            );
            let _ = log
                .write_line(&format!("[engine] {request_kind} failed: {message}"))
                .await;
        }
    }
    let status = status_for(&outcome);
    let outcome_json = serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_owned());
    let event_json = serde_json::to_string(&EngineEvent::Finished {
        outcome: outcome.clone(),
    })
    .unwrap_or_default();
    match db.finish_engine_action_with_event(&action_id, status, &outcome_json, &event_json) {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            action = %action_id,
            "engine worker: the daemon had already ended this action; its outcome is dropped"
        ),
        Err(e) => {
            tracing::warn!(action = %action_id, error = %e, "engine worker: could not record its end")
        }
    }
    tracing::info!(action = %action_id, status, "engine worker: done");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cancel (SIGINT) must be remembered, and must not kill the process —
    /// in this test binary, a missing latch would end the whole run.
    #[tokio::test]
    async fn a_sigint_is_remembered_and_does_not_kill_the_worker() {
        let flag = install_sigint_latch();
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
        // SAFETY: raise only sends a signal to this process.
        unsafe { libc::raise(libc::SIGINT) };
        for _ in 0..50 {
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("SIGINT was not latched");
    }
}
