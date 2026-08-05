//! The worktree trash and the worker that empties it (§19).
//!
//! **Removing a worktree puts it in the trash; it does not delete anything.** The
//! row is marked (`worktrees.trashed_at`), the checkout stays on disk, and the rail
//! shows it as trashed. `git worktree remove` runs later — when the retention period
//! `worktree.trashRetentionDays` expires, or when the user asks for it now. A
//! recycle bin, not a progress bar on a deletion already underway.
//!
//! That shape is what makes the two hard parts easy. Restoring is a real undo rather
//! than a race against a worker, and the request that bins a worktree does no slow
//! work at all — awaiting `git worktree remove` on a large checkout was what froze
//! the UI in the first place.
//!
//! **The row is the queue.** No job table, no journal. `trashed_at` is both the
//! durable record of intent and the retention clock, so a daemon that dies
//! mid-removal recovers by re-reading trashed rows at boot ([`recover`]) — and
//! re-running a removal that already succeeded is safe, because git fails, the path
//! is gone from disk, and the `git worktree prune` fallback finishes the job. A
//! separate queue would be a second claim about which worktrees exist and could drift
//! from the rows it describes; a flag on the row cannot disagree with itself.
//!
//! **Nothing here moves a directory.** Relocating the checkout instead of leaving it
//! in place would be O(1) and tempting, and it would bypass every safety check
//! `git worktree remove` exists to enforce while pulling the directory out from under
//! the PTY sessions, runs and browser panes rooted at that path — failing immediately
//! and invisibly rather than when the trash is emptied.
//!
//! **A failure the user could act on is never silent.** Any step that fails after the
//! row has been read takes the worktree back out of the trash with the reason on it
//! (`worktrees.trash_error`), which the rail renders and announces once. The two
//! exceptions leave nothing to report: a database this worker cannot open or read,
//! which is logged and leaves the row in the trash for [`recover`] to retry at the
//! next daemon start.

use std::path::{Path as FsPath, PathBuf};
use std::sync::{Mutex, OnceLock};
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

/// Worktrees whose deletion is past the point of no return.
///
/// Restoring is a real undo for the whole retention period — but not once
/// `git worktree remove` has started, because at that point the directory is going
/// away and no database write can bring it back. Without this, a restore issued in
/// that window returned `200` with a live-looking row and the checkout vanished
/// moments later anyway: the one outcome this module promises cannot happen.
/// Re-reading `trashed_at` before the removal (which is also done) cannot close it,
/// since the race is *with* the removal rather than before it.
///
/// So the window is made visible instead of pretended away: [`is_deleting`] lets the
/// restore handler answer "too late" honestly. In-memory is the right scope — one
/// daemon owns both the worker and the handler.
static DELETING: Mutex<Option<std::collections::HashSet<i64>>> = Mutex::new(None);

/// Whether a deletion for this worktree has passed the point where restoring can
/// still work — a **read-only status** for the UI's terminal "Deleting" lane.
///
/// The comment on [`try_restore`] — that reading and then writing is the racy
/// pattern the lock exists to replace — is about a *production* caller that could
/// lose the window between the two halves. Rendering is not that caller: it only
/// displays the flag and never writes on the strength of it, so exposing the read
/// is safe. It is `pub(crate)` (not part of the public API surface) and deliberately
/// segregated from any path that could follow it with a write.
pub(crate) fn now_deleting(worktree_id: i64) -> bool {
    DELETING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|set| set.contains(&worktree_id))
}

/// Test-only alias so the assertion reads as the predicate. See [`now_deleting`].
#[cfg(test)]
fn is_deleting(worktree_id: i64) -> bool {
    now_deleting(worktree_id)
}

/// Marks a worktree as being deleted, and un-marks it on drop.
///
/// A guard rather than paired calls because every early return between the mark and
/// the end of `process` would otherwise have to remember to clear it, and one that
/// forgot would wedge the worktree as un-restorable until the daemon restarted.
struct DeletingGuard(i64);

impl DeletingGuard {
    fn claim(id: i64) -> Self {
        DELETING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(std::collections::HashSet::new)
            .insert(id);
        Self(id)
    }
}

impl Drop for DeletingGuard {
    fn drop(&mut self) {
        if let Some(set) = DELETING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            set.remove(&self.0);
        }
    }
}

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
        // Strictly serial, for bounded concurrency and nothing more.
        //
        // An earlier version of this comment claimed git locks `.git/worktrees` per
        // repo so concurrent removals would contend. **That is not true** — four
        // simultaneous `git worktree remove` calls in one repo all succeed (tested,
        // git 2.50.1) — and it is recorded here because a false justification is
        // worse than none: the next person would have preserved serialism to protect
        // an invariant that does not exist, and it also made the inline forced path
        // look like a bug.
        //
        // The real reason is that removals are a handful of deliberate clicks plus a
        // capped timer, so one at a time is the simplest thing that cannot spawn
        // unboundedly. The cost is head-of-line blocking: one worktree whose teardown
        // hits STOP_TIMEOUT delays the queue behind it while the rail shows "Pending
        // removal". If removals ever become a stream, spawn per id — there is no
        // ordering constraint to preserve.
        while let Some(id) = rx.recv().await {
            process(id).await;
        }
    });
}

/// Re-enqueue every trashed worktree — the crash-recovery path.
///
/// Called once when the worker starts. The GC pass enqueues expired trash the same
/// way, as does an explicit "delete now", so every removal takes this one path.
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

    // Stopping the checkout's runs is authorised for every removal that reaches
    // here: either the user asked for it now, or they binned the worktree and left
    // it in the trash for the whole retention period. There is no unattended path
    // that deletes a worktree nobody put in the trash.
    if let Err(reason) = stop_runs(&db, &wt.path).await {
        fail(reason);
        return;
    }

    match delete_checkout(&db, &wt, false).await {
        Ok(Deleted::Yes) => info!("worktree trash: deleted {}", wt.path),
        Ok(Deleted::Restored) => {}
        Err(reason) => fail(reason),
    }
}

/// Whether a deletion actually happened.
pub enum Deleted {
    Yes,
    /// The worktree was taken out of the trash before the deletion could start, so
    /// nothing was deleted and nothing went wrong.
    Restored,
}

/// **The one function that deletes a checkout.** Both the background worker and the
/// inline forced path go through it, and that is the point.
///
/// Three separate rounds of review found the same defect here — a restore that
/// returned `200` while the checkout was deleted anyway — and the first two fixes
/// were correct but incomplete, because each guarded *one* of the two code paths that
/// ran `git worktree remove`. A third guard would have been the same mistake again.
/// So there is now one path, it claims the guard itself, and a future caller cannot
/// bypass what it does not implement.
///
/// The order is load-bearing: **claim the guard, then re-read the intent.** That
/// makes both interleavings safe. A restore that landed before the claim is seen by
/// the re-read and cancels the deletion; one that arrives after it is refused by
/// [`is_deleting`] rather than granted and then silently overruled. There is no third
/// window, because from the claim onwards `restore_worktree` cannot succeed.
async fn delete_checkout(
    db: &Db,
    wt: &veld_core::db::WorktreeRecord,
    force: bool,
) -> Result<Deleted, String> {
    let _deleting = DeletingGuard::claim(wt.id);

    // Re-read the intent now that no restore can slip past. `stop_runs` may have
    // taken up to STOP_TIMEOUT, and the retention sweep may have queued this minutes
    // ago; the check when the job was picked up covers neither.
    match db.get_worktree(wt.id) {
        Ok(Some(current)) if current.trashed_at.is_empty() => return Ok(Deleted::Restored),
        Ok(None) => return Ok(Deleted::Restored),
        Err(e) => return Err(format!("cannot re-check the worktree: {e}")),
        Ok(Some(_)) => {}
    }

    let repo_root = PathBuf::from(&wt.repo_root);
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push("--");
    args.push(&wt.path);
    match super::desktop::git(&repo_root, &args).await {
        Ok(_) => {}
        // Already gone from disk — deleted by hand, or by an attempt that died after
        // git finished but before the row was dropped. Prune git's bookkeeping and
        // treat it as done; this is what makes retrying a half-finished deletion
        // idempotent.
        Err(_) if !FsPath::new(&wt.path).exists() => {
            let _ = super::desktop::git(&repo_root, &["worktree", "prune"]).await;
        }
        Err(e) => return Err(e),
    }
    if let Err(e) = db.remove_worktree(wt.id) {
        // The checkout is gone; only the row survived. The next reconcile poll reaps
        // it, since the path has left `git worktree list`.
        warn!("worktree trash: deleted {} but kept its row: {e}", wt.path);
    }
    Ok(Deleted::Yes)
}

/// Delete a trashed worktree inline, discarding uncommitted changes.
///
/// The forced path, for the request handler. Same single owner as the worker, so it
/// inherits the guard rather than needing its own — the omission round 3 found.
pub async fn delete_checkout_forced(
    db: &Db,
    wt: &veld_core::db::WorktreeRecord,
) -> Result<(), String> {
    match delete_checkout(db, wt, true).await {
        Ok(_) => Ok(()),
        Err(reason) => Err(reason),
    }
}

/// Take a worktree out of the trash, unless its deletion has already started.
///
/// The check and the write share the `DELETING` lock. Two separate calls left a
/// window — narrow, but real on a multi-core runtime — where the worker could claim
/// the guard between a restore's check and its write, so the worker saw a trashed row
/// and the caller got a `200`.
pub fn try_restore(db: &Db, id: i64) -> Result<bool, veld_core::db::DbError> {
    let deleting = DELETING.lock().unwrap_or_else(|e| e.into_inner());
    if deleting.as_ref().is_some_and(|set| set.contains(&id)) {
        return Ok(false);
    }
    db.untrash_worktree(id, "")?;
    drop(deleting);
    Ok(true)
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

/// Delete trashed worktrees whose retention period has expired.
///
/// Called from the GC pass. Returns how many were queued.
///
/// The GC scheduler is spawned before the server that calls [`spawn`], and tokio's
/// `interval` fires its first tick immediately, so the first pass after a daemon
/// start can run before `QUEUE` exists and every `enqueue` here is a no-op. That is
/// harmless rather than a lost purge: [`recover`] re-enqueues every trashed row when
/// the worker does start, expired ones included, so the delay is milliseconds.
///
/// This is the **only** thing that deletes a checkout the user has not asked about
/// twice, and it is opt-in: `worktree.trashRetentionDays` defaults to zero, which
/// means "keep until I empty it" and makes this a no-op. When it is set, everything
/// it acts on is something the user put in the trash themselves and then left there
/// for the whole period — so there is no activity heuristic here, and deliberately
/// none: guessing that a worktree is abandoned is a different feature from honouring
/// a request that has been sitting in the bin for a fortnight.
///
/// The removal itself goes through the same worker and the same un-forced
/// `git worktree remove` as a hand-clicked one, so a checkout that picked up
/// uncommitted changes while it sat in the trash still comes back with git's reason
/// attached rather than being discarded.
pub fn purge_expired_trash(db: &Db) -> usize {
    let Some(retention) = db.trash_retention() else {
        return 0; // keep until emptied, which is the default
    };
    let expired = match db.expired_trashed_worktrees(retention.as_secs() as i64) {
        Ok(rows) => rows,
        Err(e) => {
            warn!("worktree trash: cannot list expired trash: {e}");
            return 0;
        }
    };
    for wt in &expired {
        info!(
            "worktree trash: {} has been in the trash over {} day(s) — deleting",
            wt.path,
            retention.as_secs() / 86_400
        );
        enqueue(wt.id);
    }
    expired.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The residual half of the restore race, and why `is_deleting` exists.
    ///
    /// Re-reading `trashed_at` before `git worktree remove` closes the window
    /// *before* the removal; it cannot close the window *during* it, because once git
    /// has deleted the directory no database write brings it back. So a restore in
    /// that window has to be refused rather than granted and then silently overruled.
    #[test]
    fn a_worktree_being_deleted_is_reported_as_such_until_the_guard_drops() {
        assert!(!is_deleting(41), "nothing is being deleted yet");
        {
            let _guard = DeletingGuard::claim(41);
            assert!(is_deleting(41));
            assert!(!is_deleting(42), "and only that worktree");
        }
        // Released on drop, so an early return anywhere in `process` cannot wedge a
        // worktree as permanently un-restorable.
        assert!(!is_deleting(41));
    }

    /// Pins the property that three rounds of review kept breaking: there is exactly
    /// one function that runs `git worktree remove`, so a guard added to it covers
    /// every caller. A second such call site is how the force path ended up
    /// unguarded, and a grep is the only thing that can catch a third.
    #[test]
    fn only_one_function_runs_git_worktree_remove() {
        let src = include_str!("worktree_trash.rs");
        let daemon_desktop = include_str!("desktop.rs");
        let occurrences = src.matches("\"worktree\", \"remove\"").count()
            + daemon_desktop.matches("\"worktree\", \"remove\"").count();
        assert_eq!(
            occurrences, 1,
            "`git worktree remove` must be spawned from exactly one place \
             (`delete_checkout`); a second call site is a second unguarded deletion \
             path, which is the defect rounds 1-3 each found a different half of"
        );
    }

    #[test]
    fn claiming_the_same_worktree_twice_is_harmless() {
        let outer = DeletingGuard::claim(7);
        {
            let _inner = DeletingGuard::claim(7);
            assert!(is_deleting(7));
        }
        // The set is keyed by id, so the inner guard's drop clears it while the outer
        // one is still alive. Acceptable because a worktree is only ever processed by
        // the single serial worker — recorded so nobody relies on nesting.
        drop(outer);
        assert!(!is_deleting(7));
    }
}
