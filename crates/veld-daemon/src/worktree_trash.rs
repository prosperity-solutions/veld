//! Background worktree removal — the worker behind the rail's trash (§19).
//!
//! `git worktree remove` on a large checkout takes long enough that awaiting it
//! inside the HTTP request froze the UI, so the request now only records intent
//! (`worktrees.trashed_at`) and returns; this worker does the slow part.
//!
//! **The row is the queue.** There is no job table and no journal. `trashed_at`
//! is the durable record of intent, so a daemon that dies mid-removal recovers by
//! re-reading trashed rows at boot ([`recover`]) — and re-running a removal that
//! already succeeded is safe, because git fails, the path is gone from disk, and
//! the `git worktree prune` fallback finishes the job. The alternative, a queue
//! that is a separate claim about which worktrees exist, can drift from the rows
//! it describes; a flag on the row cannot disagree with itself.
//!
//! **Nothing here moves or deletes a directory itself.** Relocating the checkout
//! would make the request O(1) instead of merely fast, and it would bypass every
//! safety check `git worktree remove` exists to enforce — leaving a hand-rolled
//! preflight as the only thing between two clicks and someone's uncommitted work —
//! while pulling the directory out from under the PTY sessions, runs and browser
//! panes rooted at that path. Removal is always git's decision, and git's refusal
//! is the safety net that makes auto-eviction defensible at all.
//!
//! **A failure is never silent.** Any step that fails takes the worktree back out
//! of trash with the reason on the row (`worktrees.trash_error`), which the rail
//! renders and toasts once. A background job that fails quietly is worse than the
//! blocking version it replaced.

use std::path::{Path as FsPath, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};
use veld_core::db::Db;

/// How long to wait for a worktree's runs to finish tearing down before giving up
/// and reporting it.
///
/// Generous: teardown runs a project's `on_stop` hooks, which routinely stop
/// containers. Giving up does not force anything — it puts the worktree back with
/// "its run did not stop", which is a state the user can act on.
const STOP_TIMEOUT: Duration = Duration::from_secs(120);

/// Poll interval while waiting for teardown. `veld stop` is spawned
/// fire-and-forget, so the run's persisted status is the only observable.
const STOP_POLL: Duration = Duration::from_secs(1);

/// Sender into the worker task. `None` until [`spawn`] runs (tests and the CLI
/// link this crate without starting the daemon's tasks).
static QUEUE: OnceLock<mpsc::UnboundedSender<i64>> = OnceLock::new();

/// Ask the worker to process a trashed worktree now.
///
/// Best-effort by design: the row is already marked, so a send that finds no
/// worker (or a dropped receiver) only delays the removal to the next daemon
/// start, which [`recover`] handles. It never loses the request.
pub fn enqueue(worktree_id: i64) {
    if let Some(tx) = QUEUE.get() {
        let _ = tx.send(worktree_id);
    }
}

/// Start the worker and re-enqueue anything left trashed by a previous run of
/// the daemon.
pub fn spawn() {
    let (tx, mut rx) = mpsc::unbounded_channel::<i64>();
    if QUEUE.set(tx).is_err() {
        return; // already started
    }
    tokio::spawn(async move {
        recover();
        while let Some(id) = rx.recv().await {
            process(id).await;
        }
    });
}

/// Re-enqueue every trashed worktree — the crash-recovery path.
///
/// Called once when the worker starts. Also worth knowing: the GC pass enqueues
/// its eviction candidates the same way, so a removal that failed and was retried
/// by the user takes exactly this path too.
fn recover() {
    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => {
            warn!("worktree trash: cannot open database for recovery: {e}");
            return;
        }
    };
    match db.list_trashed_worktrees() {
        Ok(rows) if !rows.is_empty() => {
            info!("worktree trash: resuming {} pending removal(s)", rows.len());
            for wt in rows {
                enqueue(wt.id);
            }
        }
        Ok(_) => {}
        Err(e) => warn!("worktree trash: cannot list pending removals: {e}"),
    }
}

/// Work one trashed worktree: stop its runs, `git worktree remove`, drop the row.
///
/// Opens its own `Db` (like the GC pass) so a schema migrated under a running
/// daemon does not strand the worker on a stale connection.
async fn process(id: i64) {
    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => {
            warn!("worktree trash: cannot open database: {e}");
            return;
        }
    };
    let wt = match db.get_worktree(id) {
        Ok(Some(wt)) => wt,
        // Already reaped — by an earlier attempt, or by the reconcile pass once
        // the path left `git worktree list`. Both mean the work is done.
        Ok(None) => return,
        Err(e) => {
            warn!("worktree trash: cannot load worktree {id}: {e}");
            return;
        }
    };
    // The user may have restored it while this was queued. Intent lives on the
    // row, so re-reading it here is what makes "undo" work without cancellation
    // plumbing.
    if wt.trashed_at.is_empty() {
        return;
    }

    let fail = |reason: String| {
        warn!("worktree trash: {} — {reason}", wt.path);
        if let Err(e) = db.untrash_worktree(id, &reason) {
            warn!("worktree trash: cannot record failure for {id}: {e}");
        }
    };

    if let Err(reason) = stop_runs(&db, &wt.path).await {
        fail(reason);
        return;
    }

    let repo_root = PathBuf::from(&wt.repo_root);
    match super::desktop::git(&repo_root, &["worktree", "remove", "--", &wt.path]).await {
        Ok(_) => {}
        // Already gone from disk — removed by hand, or by an attempt that died
        // after git finished but before the row was dropped. Prune git's
        // bookkeeping and treat it as done; this is what makes retrying a
        // half-finished removal idempotent.
        Err(_) if !FsPath::new(&wt.path).exists() => {
            let _ = super::desktop::git(&repo_root, &["worktree", "prune"]).await;
        }
        Err(e) => {
            fail(e.to_string());
            return;
        }
    }
    if let Err(e) = db.remove_worktree(id) {
        warn!("worktree trash: removed {} but kept its row: {e}", wt.path);
        return;
    }
    info!("worktree trash: removed {}", wt.path);
}

/// Ask every live run in the worktree to stop, then wait for the runs to leave
/// the live set.
///
/// Waits on the *persisted status*, not on the spawned command: `veld stop` is
/// fire-and-forget from here, and a run's teardown is two-phase (#162), so the
/// database is the only place that says whether it finished.
async fn stop_runs(db: &Db, path: &str) -> Result<(), String> {
    let wt_path = PathBuf::from(path);
    let names = db
        .live_run_names(&wt_path)
        .map_err(|e| format!("cannot check for running environments: {e}"))?;
    if names.is_empty() {
        return Ok(());
    }
    info!(
        "worktree trash: stopping {} run(s) in {path} before removal",
        names.len()
    );
    for name in &names {
        super::management::spawn_veld(
            &wt_path,
            &["stop".to_owned(), "--name".to_owned(), name.clone()],
        )
        .await;
    }
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    loop {
        tokio::time::sleep(STOP_POLL).await;
        match db.live_run_names(&wt_path) {
            Ok(v) if v.is_empty() => return Ok(()),
            Ok(v) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(format!(
                        "run \"{}\" did not stop within {}s — stop it yourself and try again",
                        v.join("\", \""),
                        STOP_TIMEOUT.as_secs()
                    ));
                }
            }
            Err(e) => return Err(format!("cannot check for running environments: {e}")),
        }
    }
}

/// Mark worktrees idle past the configured horizon for removal.
///
/// Called from the GC pass. Returns how many were marked.
///
/// Eviction only ever *marks*: the removal goes through the same worker and the
/// same un-forced `git worktree remove`, so git still refuses a dirty or locked
/// checkout and the worktree comes back with the reason attached. That is the
/// safety property — the destructive timer never gets to override git, and a
/// developer's uncommitted work survives a horizon they set carelessly.
pub fn mark_evictions(db: &Db) -> usize {
    let Some(after) = db.worktree_evict_after() else {
        return 0; // off, which is the default
    };
    let candidates = match db.worktree_eviction_candidates(after.as_secs() as i64) {
        Ok(c) => c,
        Err(e) => {
            warn!("worktree eviction: cannot list candidates: {e}");
            return 0;
        }
    };
    let mut marked = 0;
    for wt in candidates {
        match db.trash_worktree(wt.id) {
            Ok(Some(_)) => {
                info!(
                    "worktree eviction: {} idle for over {} day(s) — queued for removal",
                    wt.path,
                    after.as_secs() / 86_400
                );
                enqueue(wt.id);
                marked += 1;
            }
            Ok(None) => {}
            Err(e) => warn!("worktree eviction: cannot trash {}: {e}", wt.path),
        }
    }
    marked
}
