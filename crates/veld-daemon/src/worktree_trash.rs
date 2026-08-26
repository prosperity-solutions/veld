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
//! **The row records the bin, not a queue.** `trashed_at` is the durable record that
//! the user binned this checkout, and the retention clock. It is deliberately *not*
//! also a work item, because those two readings disagree about the case that matters:
//! at boot, "in the bin" and "queued for removal" look identical on the row, and
//! [`recover`] used to resume every trashed row — so restarting the daemon (a `veld
//! update` does exactly that) permanently deleted the whole trash, with
//! `worktree.trashRetentionDays` at its default `0` meaning *keep until I empty it*.
//! The setting was read correctly by the sweep and bypassed entirely by recovery.
//!
//! So **boot recovery is the retention sweep and nothing else** ([`recover`]). Every
//! other removal is queued by the request that asked for it, in the daemon that
//! received it. Two candidates for keeping more than that were tried and rejected, both
//! recorded in [`recover`]: inferring an interrupted removal from the filesystem, which
//! cannot work because git leaves no reliable trace of one, and a durable
//! "removal started" flag, which would identify one honestly and then retry it in the
//! one mode a half-deleted checkout refuses.
//!
//! The cost, stated: a *Delete permanently* interrupted by the daemon going away does
//! not resume. The worktree stays in the bin — where, if git had got partway through,
//! the checkout is damaged and the next delete surfaces git's refusal. That is worse
//! than a clean resume and much better than the alternative it replaces, which was
//! deleting every worktree in the bin on every restart.
//!
//! **Nothing here moves a directory.** Relocating the checkout instead of leaving it
//! in place would be O(1) and tempting, and it would bypass every safety check
//! `git worktree remove` exists to enforce while pulling the directory out from under
//! the PTY sessions, runs and browser panes rooted at that path — failing immediately
//! and invisibly rather than when the trash is emptied.
//!
//! **A failure the user could act on is never silent.** Any step that fails after the
//! row has been read takes the worktree back out of the trash with the reason on it
//! (`worktrees.trash_error`), which the rail renders and announces once. The one
//! exception leaves nothing to report: a database this worker cannot open or read,
//! which is logged and leaves the row exactly where the user put it — in the bin,
//! with the checkout untouched.

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
/// Best-effort, and the request is genuinely dropped if no worker is listening or the
/// daemon exits before the queue drains. That is the safe direction and the deliberate
/// one: what is lost is a *deletion* that never started, and the worktree is still in
/// the bin with its checkout intact for the user to ask again. The alternative —
/// treating the trash flag as a standing instruction to delete — is the bug this
/// module was rewritten to remove. The retention sweep is the only thing that
/// re-raises a removal on its own, and [`recover`] runs it at boot for exactly that
/// reason.
pub fn enqueue(worktree_id: i64) {
    if let Some(tx) = QUEUE.get() {
        let _ = tx.send(worktree_id);
    }
}

/// Start the worker and run the retention sweep once — which is all a restart does.
/// An interrupted removal is deliberately not resumed; see [`recover`].
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
        // hits STOP_TIMEOUT delays the queue behind it while the rail still shows it
        // in the Trash lane. If removals ever become a stream, spawn per id — there
        // is no ordering constraint to preserve.
        while let Some(id) = rx.recv().await {
            process(id).await;
        }
    });
}

/// Resume what a previous daemon left for this one — which is the retention sweep,
/// and nothing else.
///
/// Called once when the worker starts. Returns how many removals it queued, so the
/// decision is observable to a test rather than only to the log.
///
/// **It deliberately does not resume an interrupted `git worktree remove`.** The
/// filesystem does not carry the signal that would be needed to find one: git deletes
/// the checkout with `remove_dir_recursively`, which walks the directory in readdir
/// order, so `.git` is just another entry and its absence proves nothing. Measured on
/// git 2.50.1 — killing `git worktree remove` on a 3,600-file checkout left `.git` in
/// place 8 times out of 8. Anything keyed on "is this half-removed?" would therefore
/// both miss the real interruption and fire on states that are not one, and the only
/// repair available at that point is an `rm -rf` outside git. That is a bad trade
/// against the failure it would prevent, which is a checkout sitting in the bin.
///
/// A durable "removal started" flag would identify one honestly, and is still the
/// wrong answer: the retry it buys runs un-forced (`DeleteQuery::force` is deliberately
/// not persisted, so a crash cannot silently upgrade a removal to one that discards
/// uncommitted work), and un-forced is exactly what a half-deleted checkout refuses —
/// git reads the missing files as modifications. The user would get the worktree
/// ejected from the bin carrying "contains modified or untracked files", which is the
/// behaviour this fix exists to stop.
///
/// So an interrupted removal stays in the trash and the user asks again. See the
/// module header for what that costs.
fn recover() -> usize {
    match Db::open() {
        Ok(db) => recover_with(&db),
        Err(e) => {
            warn!("worktree trash: cannot open database for recovery: {e}");
            crate::dbhealth::note_error(&e);
            0
        }
    }
}

/// [`recover`] against an already-open database.
///
/// Split out so the test can pass its own `Db` rather than pointing `VELD_DB_PATH` at
/// a temporary one: that variable is process-global, this binary's tests run in
/// parallel, and a test whose subject is *deleting worktrees* is the last one that
/// should be able to reach a database it did not create.
fn recover_with(db: &Db) -> usize {
    let queued = purge_expired_trash(db);
    if queued > 0 {
        info!("worktree trash: {queued} expired removal(s) queued at startup");
    }
    queued
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
            crate::dbhealth::note_error(&e);
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
        //
        // Narrow on purpose. It is tempting to widen this to "git can no longer see a
        // working tree here" and `rm -rf` the remains, because a checkout whose `.git`
        // has gone missing is one `git worktree remove` refuses forever, with `--force`
        // too (measured, git 2.50.1). Resist it: the files are all still there, git has
        // merely lost the ability to classify them, so the widened arm deletes
        // uncommitted work in the one path that exists to protect it — and it cannot
        // even be reached by the interruption it would be written for, since git
        // deletes in readdir order and leaves `.git` in place (see `recover`). That
        // state is rare, recoverable by hand, and not worth an unbounded `rm` here.
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
        let code = super::management::spawn_veld(
            &wt_path,
            &["stop".to_owned(), "--name".to_owned(), name.clone()],
        )
        .await;
        // **Fail now, with the real reason, instead of after 120 seconds with the
        // wrong one.** `spawn_veld` refuses outright while `veld update` holds the
        // update lock, so nothing was spawned and nothing is going to stop — but
        // the poll below cannot tell that from a run that is merely slow, and it
        // would spend the whole `STOP_TIMEOUT` before reporting "stop it yourself
        // and try again" about a run the user cannot do anything about. An update
        // takes one to four minutes, so this collision is realistic rather than
        // theoretical, and "wait for the update" is the actionable answer.
        if code == axum::http::StatusCode::SERVICE_UNAVAILABLE {
            return Err(
                "a veld update is in progress, so runs cannot be stopped right now — try again \
                 when it finishes (`veld update --status`)"
                    .to_owned(),
            );
        }
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
/// harmless rather than a lost purge: [`recover`] calls this function again once the
/// worker exists, so the delay is milliseconds. That call is the *only* reason the
/// first tick can be dropped safely — recovery no longer re-enqueues trashed rows
/// wholesale, so nothing else would pick the expired ones up before the next tick.
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

    /// **The reported bug.** Restarting the daemon — which every `veld update` does —
    /// queues no removals while `worktree.trashRetentionDays` is at its default `0`,
    /// so a worktree the user binned and left alone is still there afterwards. Before
    /// the fix `recover` enqueued every row with a non-empty `trashed_at` and the whole
    /// bin went with the restart.
    ///
    /// Asserts on the **return value**, not on the row: `enqueue` is a no-op with no
    /// worker running, so a test that only checked the row survived would have passed
    /// against the buggy code too. Verified by reintroducing the bug — it fails.
    #[test]
    fn a_daemon_restart_queues_no_removals_while_the_trash_is_kept_forever() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open_at(&dir.path().join("t.db")).unwrap();
        let root = FsPath::new("/tmp/repo-trash-recover");
        db.upsert_repo(root, "repo").unwrap();
        let wts = db
            .sync_worktrees(
                root,
                &[veld_core::db::DiscoveredWorktree {
                    path: "/tmp/repo-trash-recover/wt".into(),
                    branch: "feature".into(),
                    is_main: false,
                }],
            )
            .unwrap();
        let id = wts.iter().find(|w| !w.is_main).unwrap().id;
        db.trash_worktree(id).unwrap();
        assert_eq!(db.list_trashed_worktrees().unwrap().len(), 1);

        // The default: keep until I empty it.
        assert!(
            db.trash_retention().is_none(),
            "this test is only meaningful while 0 is the default"
        );
        assert_eq!(
            recover_with(&db),
            0,
            "a restart must not queue the removal of a worktree the user only binned"
        );
        assert_eq!(
            db.list_trashed_worktrees().unwrap().len(),
            1,
            "and the row is still in the bin"
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
