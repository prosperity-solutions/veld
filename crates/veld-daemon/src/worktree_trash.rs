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
        // Strictly serial, on purpose: `git worktree remove` takes `.git/worktrees`
        // for the repo, so two concurrent removals in one repo would contend on
        // git's own lock and surface as spurious failures the user would have to
        // retry. The cost is head-of-line blocking — one worktree whose teardown
        // hits STOP_TIMEOUT delays the queue behind it, with the rail showing
        // "Pending removal" meanwhile. Acceptable because removals are a handful of
        // deliberate clicks, not a stream; if that changes, shard the queue per
        // repo rather than making it unbounded.
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

    // Re-read the intent immediately before the destructive step.
    //
    // `stop_runs` above can take up to STOP_TIMEOUT, and `git worktree remove` is
    // slow by definition — that slowness is why this worker exists. A user who
    // clicks "Keep it after all" anywhere in that window gets a 200 and sees the row
    // back in the rail, so deleting the checkout anyway would be the one thing this
    // module promises cannot happen: a silent loss with no error attached. The
    // check at the top of `process` only covers the time spent queued.
    match db.get_worktree(id) {
        Ok(Some(current)) if current.trashed_at.is_empty() => {
            info!("worktree trash: {} was restored — not removing", wt.path);
            return;
        }
        Ok(None) => return,
        Err(e) => {
            warn!("worktree trash: cannot re-check {}: {e}", wt.path);
            return;
        }
        Ok(Some(_)) => {}
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

/// Whether a checkout holds local files that removing it would destroy for good.
///
/// **`git worktree remove` is not the safety net it looks like.** An un-forced
/// removal refuses a worktree with modified or untracked *tracked-able* files, and
/// happily deletes one whose only contents are **ignored** — which is exactly where
/// `.env` files, credentials, local database dumps and build caches live. Verified,
/// not assumed: a worktree containing a gitignored `.env` reports clean from
/// `git status --porcelain` and `git worktree remove` exits 0 and deletes it
/// (git 2.50.1).
///
/// So eviction asks a stricter question than git does. `--ignored=matching` is the
/// flag that makes ignored files appear in the porcelain output at all; without it
/// they are invisible and the answer is the misleading "clean".
///
/// This gates **eviction only**, never a removal the user clicked. A person looking
/// at a confirmation dialog has asked for the checkout to be deleted and gets git's
/// own semantics; an unattended timer does not get to make that call for them.
///
/// Fails **closed**: a git invocation that errors returns "has local files", because
/// the alternative is deleting a checkout we could not inspect.
async fn has_local_files(path: &str) -> bool {
    // Run *in the worktree*, not in the repo root: `git()` already supplies `-C`,
    // and a second one would only shadow the first.
    match super::desktop::git(
        FsPath::new(path),
        &["status", "--porcelain", "--ignored=matching"],
    )
    .await
    {
        Ok(out) => !out.trim().is_empty(),
        Err(e) => {
            warn!("worktree eviction: cannot inspect {path}, leaving it alone: {e}");
            true
        }
    }
}

/// Mark worktrees idle past the configured horizon for removal.
///
/// Called from the GC pass. Returns how many were marked.
///
/// Eviction only ever *marks*: the removal goes through the same worker and the
/// same un-forced `git worktree remove`, so a checkout that has picked up changes
/// since it was marked still comes back with git's reason attached.
///
/// **git's refusal is not sufficient on its own** — see [`has_local_files`], which
/// is why an eviction candidate is additionally required to hold no ignored files
/// and no live terminal session. Those two exclusions are the difference between a
/// timer that reclaims abandoned checkouts and one that deletes the `.env` out of a
/// worktree somebody has a shell open in.
pub async fn mark_evictions(db: &Db) -> usize {
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
    if candidates.is_empty() {
        return 0;
    }
    // A veld run is not the only sign of life, and in this app it is not even the
    // common one: a worktree can be worked in all day through terminal panes with
    // `veld start` never running. The module doc names PTY sessions as a reason a
    // directory must not be pulled out from under someone — removal does that just
    // as surely as a move would.
    let busy = super::pty::worktree_ids_with_sessions().await;
    let mut marked = 0;
    for wt in candidates {
        if busy.contains(&wt.id) {
            continue;
        }
        if has_local_files(&wt.path).await {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A real repo with a real worktree. `has_local_files` shells out to git, so a
    /// fixture cannot answer the question this module gets wrong.
    fn repo_with_worktree(ignored: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str], cwd: &std::path::Path| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {:?}", out);
        };
        git(&["init", "-q"], &repo);
        git(&["config", "user.email", "t@e.st"], &repo);
        git(&["config", "user.name", "t"], &repo);
        std::fs::write(repo.join(".gitignore"), ".env\nbuild/\n").unwrap();
        git(&["add", "."], &repo);
        git(&["commit", "-qm", "init"], &repo);
        let wt = dir.path().join("wt");
        git(
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                "-b",
                "feature",
            ],
            &repo,
        );
        for (name, body) in ignored {
            let p = wt.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, body).unwrap();
        }
        (dir, wt)
    }

    #[tokio::test]
    async fn an_untouched_worktree_has_no_local_files() {
        let (_dir, wt) = repo_with_worktree(&[]);
        assert!(!has_local_files(wt.to_str().unwrap()).await);
    }

    /// The finding this gate exists for.
    ///
    /// `git worktree remove` (un-forced) exits 0 on a worktree whose only contents
    /// are gitignored and deletes them — so "git refuses a dirty checkout" is NOT
    /// sufficient protection for an unattended timer. Verified against git 2.50.1:
    /// plain `git status --porcelain` reports this tree clean.
    #[tokio::test]
    async fn a_gitignored_env_file_counts_as_local_files() {
        let (_dir, wt) = repo_with_worktree(&[(".env", "SECRET=hunter2\n")]);
        assert!(
            has_local_files(wt.to_str().unwrap()).await,
            "an ignored .env must block eviction — git alone would delete it"
        );
    }

    #[tokio::test]
    async fn a_gitignored_build_directory_counts_as_local_files() {
        let (_dir, wt) = repo_with_worktree(&[("build/out.js", "artifact\n")]);
        assert!(has_local_files(wt.to_str().unwrap()).await);
    }

    #[tokio::test]
    async fn an_untracked_file_counts_as_local_files() {
        let (_dir, wt) = repo_with_worktree(&[("scratch.txt", "notes\n")]);
        assert!(has_local_files(wt.to_str().unwrap()).await);
    }

    /// Fails closed: a path git cannot inspect is treated as holding local files,
    /// because the alternative is deleting a checkout we could not look at.
    #[tokio::test]
    async fn an_uninspectable_path_counts_as_local_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-a-repo");
        assert!(has_local_files(missing.to_str().unwrap()).await);
    }
}
