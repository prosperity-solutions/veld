//! The trash cleanup: free the space of a worktree that has sat in the trash.
//!
//! A binned checkout keeps its `node_modules/`, `target/` and caches at full size
//! for as long as it sits in the bin, which by default is until the user empties
//! it. After `worktree.trashCleanupHours` this runs the project's
//! `trash_cleanup.steps` (skipped when `extensions.autoRefresh` is off), then deletes:
//!
//! - **by size, only what git ignores** — files and folders over the user's
//!   thresholds (`worktree.trashCleanupMinFolderMB` / `worktree.trashCleanupMinFileMB`).
//!   That is exactly what an un-forced `git worktree remove` deletes without asking,
//!   so the size rule can cost a reinstall after a restore, and can also delete
//!   ignored data that cannot be regenerated (a local database file, a hand-built
//!   `.venv`) once it is over the size;
//! - **by name, any untracked path** the project's `trash_cleanup.paths` select,
//!   ignored or not — the project's own list of what is always safe to throw away,
//!   including output nobody added to `.gitignore`.
//!
//! **Tracked files are never touched**, by either rule: deleting one would make the
//! checkout look modified, which is what an un-forced removal refuses. Neither are:
//!
//! - **A nested repository.** An untracked folder holding its own `.git` may carry
//!   commits that exist nowhere else; `git worktree remove` would delete an ignored
//!   one, but retention is off by default, so this would be the first thing to do
//!   it unasked.
//! - **Anything through a symlink** — links are never followed to size, walk or delete.
//!   A link is unlinked only when a project pattern names it.
//!
//! **Restoring wins.** A restore during a cleanup cancels it ([`cancel`], called
//! by `worktree_trash::try_restore`): a running step's process group is killed and
//! the deletion stops at the next entry. What was removed before that is recorded on
//! the row, and the IDE announces it when the worktree comes back. Deleting the worktree
//! cancels it the same way, and waits for it to stop ([`cancel_and_wait`]) so that
//! `git worktree remove` never races this module over the same files.
//!
//! **Once each time a worktree is moved to the trash, whatever happens.** A cleanup
//! that fails is still recorded as done, with the failure in its summary, so a
//! broken step does not re-run every ten minutes. Restoring the worktree and
//! trashing it again makes it due again.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{debug, info, warn};
use veld_core::db::{Db, TrashCleanup, WorktreeRecord};

/// How long one `trash_cleanup.steps` command may run.
const STEP_TIMEOUT: Duration = Duration::from_secs(300);
/// How long listing a checkout's ignored files may take.
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a deletion waits for a cleanup it cancelled to stop.
const CANCEL_WAIT: Duration = Duration::from_secs(30);
/// How long a restore waits for a cleanup it cancelled to record what it removed.
const RESTORE_WAIT: Duration = Duration::from_secs(5);
/// Poll interval for the cancellation flag while a step runs.
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// The kv key holding when this machine first saw the cleanup switched on.
///
/// The cooldown counts from the later of this and `trashed_at`, so the release
/// that turns the cleanup on does not empty every worktree already in the bin
/// ten minutes after the update — before the user has read the card announcing it.
const ENABLED_SINCE_KEY: &str = "trashCleanup.enabledSince";

/// Worktrees being cleaned, each with its cancellation flag.
static CLEANING: Mutex<Option<HashMap<i64, Arc<AtomicBool>>>> = Mutex::new(None);

/// Whether a sweep is already working through its list.
static RUNNING: AtomicBool = AtomicBool::new(false);

fn cleaning() -> std::sync::MutexGuard<'static, Option<HashMap<i64, Arc<AtomicBool>>>> {
    CLEANING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks a worktree as being cleaned, and un-marks it on drop.
struct CleaningGuard(i64, Arc<AtomicBool>);

impl CleaningGuard {
    fn claim(id: i64) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        cleaning()
            .get_or_insert_with(HashMap::new)
            .insert(id, flag.clone());
        Self(id, flag)
    }
}

impl Drop for CleaningGuard {
    fn drop(&mut self) {
        if let Some(map) = cleaning().as_mut() {
            map.remove(&self.0);
        }
    }
}

/// Clears [`RUNNING`] however the sweep ends.
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Ask a running cleanup of this worktree to stop. Returns whether one was running.
pub(crate) fn cancel(id: i64) -> bool {
    match cleaning().as_ref().and_then(|m| m.get(&id)) {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

fn is_cleaning(id: i64) -> bool {
    cleaning().as_ref().is_some_and(|m| m.contains_key(&id))
}

/// After a restore cancelled a cleanup, wait briefly for it to stop and record
/// what it removed, so the restore can report that. Gives up quietly: the
/// restore has already happened, and only the report is at stake.
pub(crate) async fn wait_stopped(id: i64) {
    let deadline = tokio::time::Instant::now() + RESTORE_WAIT;
    while is_cleaning(id) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

/// Cancel a cleanup of this worktree and wait for it to stop, for a deletion
/// that is about to run `git worktree remove` on the same files.
///
/// The caller must already hold its deletion guard: a cleanup claims its own
/// guard *before* checking for a deletion, and a deletion claims its guard before
/// calling this, so one of the two always sees the other.
pub(crate) async fn cancel_and_wait(id: i64) -> Result<(), String> {
    if !cancel(id) {
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + CANCEL_WAIT;
    while is_cleaning(id) {
        if tokio::time::Instant::now() >= deadline {
            return Err(
                "the trash cleanup is still stopping in this worktree — try again in a moment"
                    .to_owned(),
            );
        }
        tokio::time::sleep(CANCEL_POLL).await;
    }
    Ok(())
}

/// Start cleaning every trashed worktree that is due. Returns how many were queued.
///
/// **Callers must hold the GC pass's database-health gate** (`housekeeping()` in
/// `gc.rs`): this deletes files on the strength of rows, and a damaged database
/// must not decide that. And go through here, not `cleanable_trashed_worktrees`
/// directly — [`due`] is what applies the cooldown from when the cleanup was
/// switched on.
///
/// The work happens on a task of its own, one worktree at a time; a pass that
/// finds one still running queues nothing.
pub fn sweep(db: &Db) -> usize {
    let ids = due(db, chrono::Utc::now());
    if ids.is_empty() || RUNNING.swap(true, Ordering::SeqCst) {
        return 0;
    }
    let queued = ids.len();
    tokio::spawn(async move {
        let _running = RunningGuard;
        for id in ids {
            clean(id).await;
        }
    });
    queued
}

/// The trashed worktrees a sweep at `now` should clean, in order.
///
/// Also keeps the [`ENABLED_SINCE_KEY`] record: cleared while the cleanup is off,
/// written on the first pass that finds it on. So *every* switch from off to on —
/// the release that ships it on, and a user turning it back on — starts the
/// cooldown from that moment rather than emptying the whole bin at once.
fn due(db: &Db, now: chrono::DateTime<chrono::Utc>) -> Vec<i64> {
    let Some(settings) = db.trash_cleanup() else {
        let _ = db.kv_delete(ENABLED_SINCE_KEY);
        return Vec::new();
    };
    let since = match db.kv_get(ENABLED_SINCE_KEY) {
        Ok(Some(s)) => chrono::DateTime::parse_from_rfc3339(&s)
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or(now),
        Ok(None) => {
            if let Err(e) = db.kv_set(ENABLED_SINCE_KEY, &now.to_rfc3339()) {
                warn!("trash cleanup: cannot record when it was enabled: {e}");
            }
            return Vec::new();
        }
        Err(e) => {
            warn!("trash cleanup: cannot read when it was enabled: {e}");
            return Vec::new();
        }
    };
    let after = chrono::Duration::from_std(settings.after).unwrap_or(chrono::Duration::MAX);
    if now - since < after {
        return Vec::new();
    }
    match db.cleanable_trashed_worktrees(now - after) {
        Ok(rows) => rows.iter().map(|w| w.id).collect(),
        Err(e) => {
            warn!("trash cleanup: cannot list the trash: {e}");
            Vec::new()
        }
    }
}

/// Clean one trashed worktree, if it still wants cleaning.
async fn clean(id: i64) {
    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => {
            warn!("trash cleanup: cannot open database: {e}");
            crate::dbhealth::note_error(&e);
            return;
        }
    };
    // Re-read for each worktree: a queue can take minutes, and turning the
    // cleanup off (or changing a size) applies to what is still waiting in it.
    let Some(settings) = db.trash_cleanup() else {
        return;
    };
    let guard = CleaningGuard::claim(id);
    let wt = match db.get_worktree(id) {
        Ok(Some(wt)) if !wt.trashed_at.is_empty() && wt.trash_cleaned_at.is_empty() => wt,
        Ok(_) => return,
        Err(e) => {
            warn!("trash cleanup: cannot load worktree {id}: {e}");
            return;
        }
    };
    // After the claim, so a deletion that starts from here on finds this cleanup
    // and cancels it — and one that started before is seen here.
    if super::worktree_trash::now_deleting(id) {
        return;
    }
    // Trashing does not stop a run, and neither does this: a cleanup is not worth
    // tearing down an environment somebody may still be using. It waits for the
    // next pass instead.
    match db.live_run_names(Path::new(&wt.path)) {
        Ok(live) if !live.is_empty() => {
            debug!(
                "trash cleanup: {} still has a running environment — trying later",
                wt.path
            );
            return;
        }
        Ok(_) => {}
        Err(e) => {
            warn!("trash cleanup: cannot check runs in {}: {e}", wt.path);
            return;
        }
    }

    let summary = if Path::new(&wt.path).is_dir() {
        run(&db, &wt, settings, guard.1.clone()).await
    } else {
        Some("found nothing to clean — the checkout is no longer on disk".to_owned())
    };
    // `None`: nothing happened that is worth recording, and the next pass should
    // try again — git failed, or the cleanup was stopped before it removed
    // anything (a deletion git then refuses leaves the worktree in the trash, and
    // it is still owed its cleanup).
    let Some(summary) = summary else {
        debug!("trash cleanup: {} — nothing recorded", wt.path);
        return;
    };
    info!("trash cleanup: {} — {summary}", wt.path);
    // Conditional on this trashing: a cleanup cancelled by a restore still
    // records what it removed, but one whose worktree was restored and binned
    // again must not mark the new trashing as cleaned.
    if let Err(e) = db.mark_trash_cleaned(id, &wt.path, &wt.trashed_at, &summary) {
        warn!(
            "trash cleanup: cannot record the cleanup of {}: {e}",
            wt.path
        );
    }
}

/// Run the project's steps, then delete. Returns the summary for the row, or
/// `None` when there is nothing to record (see `clean`).
async fn run(
    db: &Db,
    wt: &WorktreeRecord,
    settings: TrashCleanup,
    flag: Arc<AtomicBool>,
) -> Option<String> {
    let root = PathBuf::from(&wt.path);
    let mut notes: Vec<String> = Vec::new();

    // A checkout whose `.git` went missing sits inside whatever repository
    // encloses it, and git would answer with *that* repository's ignore rules.
    // Raced against the flag, like every other wait here: a deletion waiting on
    // this cleanup must not wait out a hung git.
    let toplevel = tokio::select! {
        r = tokio::time::timeout(
            LIST_TIMEOUT,
            super::desktop::git_raw_cancellable(&root, &["rev-parse", "--show-toplevel"]),
        ) => Some(r),
        () = cancelled(&flag) => None,
    };
    match toplevel {
        Some(Ok(Ok(out))) => {
            let top = String::from_utf8_lossy(&out);
            if !same_dir(Path::new(top.trim_end_matches('\n')), &root) {
                return Some(
                    "found nothing to clean — git no longer sees a worktree there".to_owned(),
                );
            }
        }
        // Stopped, timed out, or git failed: nothing to record, try next pass.
        _ => return None,
    }

    let project = project_cleanup(db, wt);
    let mut patterns: Vec<String> = Vec::new();
    match project {
        Ok(None) => {}
        Err(problem) => notes.push(problem),
        Ok(Some(p)) => {
            patterns = runtime_patterns(&p.section.paths);
            let dropped = p.section.paths.len() - patterns.len();
            if dropped > 0 {
                notes.push(format!(
                    "{dropped} pattern(s) ignored — see `veld orchestrator lint`"
                ));
            }
            let builtins = super::pty::worktree_builtins(&root, &wt.branch, &p.config);
            // The one switch for everything Veld runs from a repo's config without
            // a click. Off skips the steps; the deletion is Veld's own and stays.
            let steps: &[veld_core::config::SetupStep] = if p.section.steps.is_empty() {
                &[]
            } else if !db.extensions_auto_refresh() {
                notes.push(
                    "the project's steps were skipped — Let projects run their own status \
                     commands is off"
                        .to_owned(),
                );
                &[]
            } else {
                &p.section.steps[..p
                    .section
                    .steps
                    .len()
                    .min(veld_core::config::MAX_TRASH_CLEANUP_STEPS)]
            };
            for step in steps {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Some(spec) = step.cmd.spec() else {
                    continue;
                };
                // Lint refuses these, but lint is advice: `ide.extensions` drops the
                // same commands at runtime, and so does this. A `shell` step must
                // never see the raw branch name, which whoever named the branch chose.
                if let Some(problem) =
                    veld_core::ide::worktree_command_problem(&spec, "trash cleanup")
                {
                    warn!("trash cleanup: step \"{}\" skipped: {problem}", step.name);
                    notes.push(format!(
                        "step \"{}\" skipped — see `veld orchestrator lint`",
                        step.name
                    ));
                    continue;
                }
                let running = super::extensions::spawn_command(
                    &spec,
                    &wt.path,
                    &p.declare_root,
                    &builtins,
                    STEP_TIMEOUT,
                    &[],
                );
                // Dropping `running` kills the step's whole process group.
                let outcome = tokio::select! {
                    r = running => Some(r),
                    () = cancelled(&flag) => None,
                };
                match outcome {
                    None => break,
                    Some(Ok(out)) if out.success => {}
                    Some(Ok(out)) => {
                        let why = if out.timed_out {
                            format!("timed out after {}s", STEP_TIMEOUT.as_secs())
                        } else {
                            match out.code {
                                Some(c) => format!("exited {c}"),
                                None => "was killed".to_owned(),
                            }
                        };
                        // The last line only: tools print tokens and environment in
                        // errors, and the daemon log outlives the worktree.
                        warn!(
                            "trash cleanup: step \"{}\" in {} {why}: {}",
                            step.name,
                            wt.path,
                            out.stderr.trim().lines().last().unwrap_or("")
                        );
                        notes.push(match &step.failure_message {
                            Some(m) => format!("step \"{}\" {why}: {m}", step.name),
                            None => format!("step \"{}\" {why}", step.name),
                        });
                    }
                    Some(Err(e)) => {
                        warn!("trash cleanup: step \"{}\" in {}: {e}", step.name, wt.path);
                        notes.push(format!("step \"{}\" could not run", step.name));
                    }
                }
            }
        }
    }

    let removed = if flag.load(Ordering::SeqCst) {
        Removed::default()
    } else {
        // Untracked work git does not ignore is listed only when a pattern could
        // select it: the size limits never apply to it.
        // Raced against the flag like the steps are: a deletion waits for this
        // cleanup to stop, and must not wait out a slow listing.
        let listing = async {
            match list_untracked(&root, Untracked::Ignored).await {
                Ok(ignored) if patterns.is_empty() => Ok((ignored, Vec::new())),
                Ok(ignored) => list_untracked(&root, Untracked::NotIgnored)
                    .await
                    .map(|other| (ignored, other)),
                Err(e) => Err(e),
            }
        };
        let listed = tokio::select! {
            l = listing => l,
            () = cancelled(&flag) => Ok((Vec::new(), Vec::new())),
        };
        match listed {
            Err(e) => {
                notes.push(format!("could not list untracked files: {e}"));
                Removed::default()
            }
            Ok((ignored, other)) => {
                let flag = flag.clone();
                let root = root.clone();
                tokio::task::spawn_blocking(move || {
                    let mut removed =
                        delete_untracked(&root, &ignored, &settings, &patterns, &flag);
                    let by_pattern_only = TrashCleanup {
                        min_folder_bytes: None,
                        min_file_bytes: None,
                        ..settings
                    };
                    let more = delete_untracked(&root, &other, &by_pattern_only, &patterns, &flag);
                    removed.bytes += more.bytes;
                    removed.paths.extend(more.paths);
                    removed.problems.extend(more.problems);
                    removed
                })
                .await
                .unwrap_or_default()
            }
        }
    };
    notes.extend(removed.problems.iter().cloned());
    if flag.load(Ordering::SeqCst) {
        if removed.paths.is_empty() && removed.problems.is_empty() {
            return None;
        }
        notes.push("stopped early — the worktree was restored or deleted".to_owned());
    }
    Some(summarize(&removed, &notes))
}

/// The project's patterns as the cleanup applies them: trimmed, without a
/// trailing `/`, and within the limits lint states — lint is advice, and a config
/// that fails it still runs here.
fn runtime_patterns(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .take(veld_core::config::MAX_TRASH_CLEANUP_PATHS)
        .map(|p| p.trim().trim_end_matches('/').to_owned())
        .filter(|p| {
            !p.is_empty()
                && p.len() <= veld_core::config::MAX_TRASH_CLEANUP_PATH_LEN
                && !p.starts_with('/')
                && !p.starts_with('~')
                && !p.split('/').any(|seg| seg == "..")
        })
        .collect()
}

/// Whether two paths name the same directory, through any symlinks on the way
/// (git reports the realpath; `/tmp` is `/private/tmp` on macOS).
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

async fn cancelled(flag: &AtomicBool) {
    loop {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

/// The project's `trash_cleanup`, read from the root its commands are declared
/// in (`extensions.source`), and the config the steps interpolate against.
struct ProjectCleanup {
    config: veld_core::config::VeldConfig,
    section: veld_core::config::TrashCleanupConfig,
    declare_root: String,
}

/// `Ok(None)` when the project declares nothing (or has no config at all);
/// `Err` with a note for the summary when it declares something unreadable.
fn project_cleanup(db: &Db, wt: &WorktreeRecord) -> Result<Option<ProjectCleanup>, String> {
    // Fail closed, as every unattended command does: with `extensions.source` at
    // its `main` default, a branch cannot add commands that run on this machine
    // by editing its own `veld.json`.
    let Some(declare_root) =
        super::extensions::resolve_declare_root(db, wt, db.extensions_source())
    else {
        return Ok(None);
    };
    let Some(path) = veld_core::config::root_config_in(Path::new(&declare_root)) else {
        return Ok(None);
    };
    let config = match veld_core::config::parse_config(&path) {
        Ok(c) => c,
        Err(e) => {
            warn!("trash cleanup: {} does not load: {e}", path.display());
            return Err("veld.json does not load, so only the size limits applied".to_owned());
        }
    };
    if config.trash_cleanup.is_none() {
        return Ok(None);
    }
    match config.trash_cleanup_section() {
        Ok(section) => Ok(Some(ProjectCleanup {
            config,
            section,
            declare_root,
        })),
        Err(e) => {
            warn!("trash cleanup: `trash_cleanup` in {}: {e}", path.display());
            Err(
                "veld.json's trash_cleanup does not parse, so only the size limits applied"
                    .to_owned(),
            )
        }
    }
}

/// One untracked entry as git reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ignored {
    /// Relative path, `/`-separated, without a trailing `/`.
    rel: String,
    /// Whether git reported it as a directory (collapsed by `--directory`).
    dir: bool,
}

/// Which untracked entries to list.
#[derive(Clone, Copy)]
enum Untracked {
    /// What git ignores: the size limits and the project's patterns apply.
    Ignored,
    /// Untracked and not ignored: only the project's patterns apply.
    NotIgnored,
}

/// The checkout's untracked entries of one kind — whole untracked folders
/// collapsed to one entry. Tracked files are never listed, so nothing here can
/// make the checkout look modified.
///
/// No fallback when git fails: a checkout git cannot read is one whose untracked
/// set is unknown, and guessing at it is how a cleanup deletes work.
async fn list_untracked(root: &Path, kind: Untracked) -> Result<Vec<Ignored>, String> {
    let args: &[&str] = match kind {
        Untracked::Ignored => &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
        Untracked::NotIgnored => &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
    };
    let out = tokio::time::timeout(
        LIST_TIMEOUT,
        super::desktop::git_raw_cancellable(root, args),
    )
    .await
    .map_err(|_| format!("git took longer than {}s", LIST_TIMEOUT.as_secs()))??;
    Ok(parse_untracked(&out))
}

/// Parse `ls-files -z` output and drop entries inside another listed folder.
///
/// git lists a folder that is not itself ignored but holds only ignored files
/// twice — `logs/` and `logs/a.log` — so without this the file would be visited
/// after its folder was already gone.
fn parse_untracked(raw: &[u8]) -> Vec<Ignored> {
    let mut entries: Vec<Ignored> = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let s = String::from_utf8_lossy(s);
            let dir = s.ends_with('/');
            Ignored {
                rel: s.trim_end_matches('/').to_owned(),
                dir,
            }
        })
        .filter(|e| !e.rel.is_empty())
        .collect();
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut kept: Vec<Ignored> = Vec::with_capacity(entries.len());
    for e in entries {
        let inside = kept
            .iter()
            .rev()
            .find(|k| k.dir)
            .is_some_and(|k| e.rel.starts_with(&format!("{}/", k.rel)));
        if !inside {
            kept.push(e);
        }
    }
    kept
}

/// What the deletion pass removed.
#[derive(Debug, Default)]
struct Removed {
    bytes: u64,
    /// Removed paths with their size, for the summary.
    paths: Vec<(String, u64)>,
    problems: Vec<String>,
}

/// Delete what qualifies among `entries`. Blocking; stops at the next entry once
/// `flag` is set.
fn delete_untracked(
    root: &Path,
    entries: &[Ignored],
    settings: &TrashCleanup,
    patterns: &[String],
    flag: &AtomicBool,
) -> Removed {
    let mut removed = Removed::default();
    // A pattern selects a path or anything under it, as a `.gitignore` line does:
    // `dist` reaches `pkg/dist/extra.js` even where git lists that file on its
    // own because `pkg/dist` also holds tracked files, which stay.
    let matches = |rel: &str| {
        let mut at = rel;
        loop {
            if patterns
                .iter()
                .any(|p| veld_core::files::glob_matches(p, at))
            {
                return true;
            }
            match at.rfind('/') {
                Some(i) => at = &at[..i],
                None => return false,
            }
        }
    };
    let mut nested_repos = 0usize;
    for entry in entries {
        if flag.load(Ordering::SeqCst) {
            break;
        }
        let Some(full) = safe_join(root, &entry.rel) else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        let named = matches(&entry.rel);
        if meta.file_type().is_symlink() {
            if named {
                remove_one(&full, &entry.rel, 0, flag, &mut removed);
            }
            continue;
        }
        if meta.is_file() {
            if named || settings.min_file_bytes.is_some_and(|m| meta.len() >= m) {
                remove_one(&full, &entry.rel, meta.len(), flag, &mut removed);
            }
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        let scan = scan(&full, &entry.rel, settings, &matches, flag);
        // A walk the flag cut short has not seen the whole folder — maybe not
        // the `.git` that would keep it — so nothing in it is decided.
        if flag.load(Ordering::SeqCst) {
            break;
        }
        if scan.nested_repo {
            nested_repos += 1;
            continue;
        }
        if named || settings.min_folder_bytes.is_some_and(|m| scan.bytes >= m) {
            remove_one(&full, &entry.rel, scan.bytes, flag, &mut removed);
            continue;
        }
        for (rel, bytes) in scan.inside {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            if let Some(p) = safe_join(root, &rel) {
                remove_one(&p, &rel, bytes, flag, &mut removed);
            }
        }
    }
    if nested_repos > 0 {
        removed.problems.push(format!(
            "kept {nested_repos} folder(s) holding a git repository"
        ));
    }
    removed
}

/// What walking an untracked folder found.
struct Scan {
    bytes: u64,
    nested_repo: bool,
    /// Entries inside that qualify on their own: pattern matches, and files over
    /// the file size limit.
    inside: Vec<(String, u64)>,
}

/// Size an untracked folder without following links, noting a nested repository
/// and the entries inside it that qualify by themselves.
fn scan(
    dir: &Path,
    rel: &str,
    settings: &TrashCleanup,
    matches: &dyn Fn(&str) -> bool,
    flag: &AtomicBool,
) -> Scan {
    let mut out = Scan {
        bytes: 0,
        nested_repo: false,
        inside: Vec::new(),
    };
    let mut stack: Vec<(PathBuf, String)> = vec![(dir.to_path_buf(), rel.to_owned())];
    while let Some((path, rel)) = stack.pop() {
        if flag.load(Ordering::SeqCst) {
            break;
        }
        let Ok(read) = std::fs::read_dir(&path) else {
            continue;
        };
        for child in read.flatten() {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let name = child.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                out.nested_repo = true;
                return out;
            }
            let child_rel = format!("{rel}/{name}");
            let Ok(meta) = std::fs::symlink_metadata(child.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                if matches(&child_rel) {
                    out.inside.push((child_rel, 0));
                }
                continue;
            }
            if meta.is_dir() {
                if matches(&child_rel) {
                    let inner = scan(&child.path(), &child_rel, settings, matches, flag);
                    out.bytes += inner.bytes;
                    if inner.nested_repo {
                        out.nested_repo = true;
                        return out;
                    }
                    out.inside.push((child_rel, inner.bytes));
                } else {
                    stack.push((child.path(), child_rel));
                }
                continue;
            }
            let len = meta.len();
            out.bytes += len;
            if matches(&child_rel) || settings.min_file_bytes.is_some_and(|m| len >= m) {
                out.inside.push((child_rel, len));
            }
        }
    }
    out
}

/// `root/rel`, or `None` when `rel` could leave `root` — an absolute path, a `..`,
/// or a symlinked folder on the way down.
fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    let mut at = root.to_path_buf();
    let components: Vec<Component<'_>> = rel_path.components().collect();
    if components.is_empty() {
        return None;
    }
    for (i, c) in components.iter().enumerate() {
        let Component::Normal(part) = c else {
            return None;
        };
        if part == &std::ffi::OsStr::new(".git") {
            return None;
        }
        at.push(part);
        if i + 1 < components.len() {
            let meta = std::fs::symlink_metadata(&at).ok()?;
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return None;
            }
        }
    }
    Some(at)
}

fn remove_one(path: &Path, rel: &str, bytes: u64, flag: &AtomicBool, removed: &mut Removed) {
    // Checked here as well as inside, so `Ok(false)` below means "stopped after
    // it had started", never "not started at all".
    if flag.load(Ordering::SeqCst) {
        return;
    }
    match remove_tree(path, flag) {
        Ok(true) => {
            removed.bytes += bytes;
            removed.paths.push((rel.to_owned(), bytes));
        }
        // Stopped partway: not reported as removed, because it is not — but named,
        // because a half-deleted dependency folder is exactly what a restored
        // worktree's first run trips over.
        Ok(false) => removed.problems.push(format!("partly deleted {rel}")),
        Err(e) => {
            warn!("trash cleanup: cannot delete {}: {e}", path.display());
            removed.problems.push(format!("could not delete {rel}"));
        }
    }
}

/// Delete a file, link or folder without following links, checking `flag`
/// between entries. `Ok(false)` when it stopped early.
///
/// Not `remove_dir_all`, which cannot be interrupted: a restore that lands while a
/// large `node_modules` is going must stop the deletion, not wait minutes for it.
fn remove_tree(path: &Path, flag: &AtomicBool) -> std::io::Result<bool> {
    if flag.load(Ordering::SeqCst) {
        return Ok(false);
    }
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        std::fs::remove_file(path)?;
        return Ok(true);
    }
    for child in std::fs::read_dir(path)? {
        if flag.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let child = child?;
        if !remove_tree(&child.path(), flag)? {
            return Ok(false);
        }
    }
    std::fs::remove_dir(path)?;
    Ok(true)
}

/// The one-line summary stored on the row and shown in the IDE.
///
/// Written to read after "the cleanup …", which is how the restore notice uses it.
///
/// Empty when nothing was removed and there is nothing to report, so a restore
/// stays quiet after a cleanup that had nothing to do.
fn summarize(removed: &Removed, notes: &[String]) -> String {
    if removed.paths.is_empty() && notes.is_empty() {
        return String::new();
    }
    let mut out = if removed.paths.is_empty() {
        "removed nothing".to_owned()
    } else {
        let mut by_size = removed.paths.clone();
        by_size.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let shown: Vec<&str> = by_size.iter().take(3).map(|(p, _)| p.as_str()).collect();
        let more = by_size.len().saturating_sub(shown.len());
        let mut s = format!(
            "freed {} — {}",
            human_bytes(removed.bytes),
            shown.join(", ")
        );
        if more > 0 {
            s.push_str(&format!(" and {more} more"));
        }
        s
    };
    for note in notes {
        out.push_str("; ");
        out.push_str(note);
    }
    out
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(folder_mb: Option<u64>, file_mb: Option<u64>) -> TrashCleanup {
        TrashCleanup {
            after: Duration::from_secs(3600),
            min_folder_bytes: folder_mb.map(|n| n * 1024 * 1024),
            min_file_bytes: file_mb.map(|n| n * 1024 * 1024),
        }
    }

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    const MB: usize = 1024 * 1024;

    /// A repository with every kind of entry the cleanup has to tell apart.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let r = dir.path();
        git(r, &["init", "-q"]);
        std::fs::write(
            r.join(".gitignore"),
            "node_modules/\nsmall/\n*.bin\nlogs/*.log\nvendor/\n.env\n.cache/\nlink\n",
        )
        .unwrap();
        write(&r.join("src/main.rs"), 10);
        write(&r.join("tracked.bin"), 2 * MB);
        git(
            r,
            &["add", "-f", ".gitignore", "src/main.rs", "tracked.bin"],
        );
        // Ignored and over the folder size (5 MB in the test): goes whole.
        write(&r.join("node_modules/a/index.js"), 3 * MB);
        write(&r.join("node_modules/b/index.js"), 3 * MB);
        // Ignored folder under the folder size, but one file over the file size.
        write(&r.join("small/tiny.txt"), 10);
        write(&r.join("small/big.dat"), 2 * MB);
        // Ignored large file at the top level.
        write(&r.join("dump.bin"), 2 * MB);
        // Folder that is not ignored itself, holding only ignored files.
        write(&r.join("logs/a.log"), 2 * MB);
        // Ignored folder that is a nested repository: never touched.
        write(&r.join("vendor/lib/big.dat"), 3 * MB);
        git(&r.join("vendor/lib"), &["init", "-q"]);
        // Small ignored file: stays.
        write(&r.join(".env"), 10);
        // Untracked but NOT ignored: never touched, whatever its size.
        write(&r.join("work.dat"), 5 * MB);
        // A modified tracked file matching an ignore pattern: still tracked.
        write(&r.join("tracked.bin"), 3 * MB);
        // An ignored symlink to a folder outside: never followed.
        let outside = tempfile::TempDir::new().unwrap();
        write(&outside.path().join("precious.dat"), 3 * MB);
        std::os::unix::fs::symlink(outside.path(), r.join("link")).unwrap();
        std::mem::forget(outside);
        dir
    }

    async fn ignored(r: &Path) -> Vec<Ignored> {
        listed(r, true)
    }

    fn listed(r: &Path, ignored: bool) -> Vec<Ignored> {
        let mut args = vec!["ls-files", "--others"];
        if ignored {
            args.push("--ignored");
        }
        args.extend(["--exclude-standard", "--directory", "-z"]);
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        parse_untracked(&out.stdout)
    }

    /// The project's list is for what is always safe to throw away, including
    /// output nobody added to `.gitignore` — but never a tracked file, and never
    /// by size.
    #[test]
    fn patterns_reach_untracked_work_git_does_not_ignore_but_never_tracked_files() {
        let dir = fixture();
        let r = dir.path();
        write(&r.join("generated/out.dat"), 6 * MB);
        write(&r.join("pkg/dist/bundle.js"), 10);
        git(r, &["add", "-f", "pkg"]);
        write(&r.join("pkg/dist/extra.js"), 10);
        let flag = AtomicBool::new(false);
        let patterns = vec![
            "work.dat".to_owned(),
            "dist".to_owned(),
            "src/**".to_owned(),
        ];
        let unignored = listed(r, false);
        let none = settings(None, None);
        delete_untracked(r, &unignored, &none, &patterns, &flag);

        assert!(
            !r.join("work.dat").exists(),
            "named, untracked, not ignored"
        );
        assert!(
            r.join("generated/out.dat").exists(),
            "unnamed: size never applies"
        );
        assert!(r.join("pkg/dist/bundle.js").exists(), "tracked file kept");
        assert!(
            !r.join("pkg/dist/extra.js").exists(),
            "its untracked sibling goes"
        );
        assert!(r.join("src/main.rs").exists(), "tracked, though named");
    }

    #[tokio::test]
    async fn only_large_ignored_entries_go_and_everything_else_stays() {
        let dir = fixture();
        let r = dir.path();
        let link_target = std::fs::read_link(r.join("link")).unwrap();
        let entries = ignored(r).await;
        let flag = AtomicBool::new(false);
        let removed = delete_untracked(r, &entries, &settings(Some(5), Some(1)), &[], &flag);

        assert!(!r.join("node_modules").exists(), "large ignored folder");
        assert!(
            !r.join("small/big.dat").exists(),
            "large file in a small folder"
        );
        assert!(
            r.join("small/tiny.txt").exists(),
            "small file in a small folder"
        );
        assert!(!r.join("dump.bin").exists(), "large ignored file");
        assert!(
            !r.join("logs/a.log").exists(),
            "ignored file in an unignored folder"
        );
        assert!(
            r.join("vendor/lib/big.dat").exists(),
            "nested repository kept"
        );
        assert!(r.join(".env").exists(), "small ignored file kept");
        assert!(r.join("work.dat").exists(), "untracked, not ignored: kept");
        assert!(r.join("tracked.bin").exists(), "tracked: kept");
        assert!(r.join("src/main.rs").exists());
        assert!(r.join("link").exists(), "an unnamed link is kept");
        assert!(link_target.join("precious.dat").exists(), "never followed");
        assert!(
            removed
                .problems
                .iter()
                .any(|p| p.contains("git repository")),
            "{:?}",
            removed.problems
        );
    }

    #[tokio::test]
    async fn patterns_select_ignored_entries_whatever_their_size() {
        let dir = fixture();
        let r = dir.path();
        let link_target = std::fs::read_link(r.join("link")).unwrap();
        let entries = ignored(r).await;
        let flag = AtomicBool::new(false);
        let patterns = vec![
            ".env".to_owned(),
            "tiny.txt".to_owned(),
            "link".to_owned(),
            "work.dat".to_owned(),
            "src/**".to_owned(),
        ];
        delete_untracked(r, &entries, &settings(None, None), &patterns, &flag);

        assert!(!r.join(".env").exists(), "named and ignored");
        assert!(
            !r.join("small/tiny.txt").exists(),
            "named inside an ignored folder"
        );
        assert!(!r.join("link").exists(), "a named link is unlinked");
        assert!(link_target.join("precious.dat").exists(), "…not followed");
        assert!(r.join("work.dat").exists(), "not in the ignored listing");
        assert!(r.join("src/main.rs").exists(), "tracked files never");
        assert!(
            r.join("node_modules").exists(),
            "sizes off: nothing by size"
        );
    }

    /// Once cancelled, nothing more goes — in particular nothing a cut-short walk
    /// collected before it could see a nested repository's `.git`.
    #[tokio::test]
    async fn a_cancelled_cleanup_deletes_nothing() {
        let dir = fixture();
        let r = dir.path();
        let entries = ignored(r).await;
        let flag = AtomicBool::new(true);
        let removed = delete_untracked(r, &entries, &settings(Some(1), Some(1)), &[], &flag);
        assert!(removed.paths.is_empty(), "{:?}", removed.paths);
        assert!(r.join("node_modules").exists());
        assert!(r.join("dump.bin").exists());
        assert!(r.join("vendor/lib/big.dat").exists());
    }

    #[test]
    fn a_set_flag_stops_the_deletion() {
        let dir = tempfile::TempDir::new().unwrap();
        write(&dir.path().join("a/b/c.txt"), 10);
        let flag = AtomicBool::new(true);
        assert!(!remove_tree(&dir.path().join("a"), &flag).unwrap());
        assert!(dir.path().join("a/b/c.txt").exists());
    }

    /// Every switch from off to on starts the cooldown then — not only the first
    /// one — so turning the cleanup back on does not empty the whole bin.
    #[test]
    fn the_cooldown_counts_from_each_time_the_cleanup_is_switched_on() {
        use serde_json::Value;
        use std::collections::BTreeMap;
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open_at(&dir.path().join("t.db")).unwrap();
        let root = Path::new("/tmp/repo-trash-cleanup-due");
        db.upsert_repo(root, "repo").unwrap();
        let wts = db
            .sync_worktrees(
                root,
                &[veld_core::db::DiscoveredWorktree {
                    path: "/tmp/repo-trash-cleanup-due/wt".into(),
                    branch: "feature".into(),
                    is_main: false,
                }],
            )
            .unwrap();
        let id = wts.iter().find(|w| !w.is_main).unwrap().id;
        db.trash_worktree(id).unwrap();
        let set = |hours: i64| {
            let mut patch = BTreeMap::new();
            patch.insert("worktree.trashCleanupHours".to_owned(), Value::from(hours));
            db.patch_settings(&patch).unwrap();
        };
        let now = chrono::Utc::now();
        let later = |h: i64| now + chrono::Duration::hours(h);

        assert!(
            due(&db, now).is_empty(),
            "the first pass only records the switch"
        );
        assert!(due(&db, later(23)).is_empty(), "still inside the cooldown");
        assert_eq!(
            due(&db, later(25)),
            [id],
            "due a day after it was switched on"
        );

        set(0);
        assert!(due(&db, later(30)).is_empty(), "off");
        set(24);
        assert!(
            due(&db, later(31)).is_empty(),
            "switched back on: the cooldown starts again"
        );
        assert!(due(&db, later(54)).is_empty());
        assert_eq!(due(&db, later(56)), [id]);
    }

    #[test]
    fn runtime_patterns_apply_the_limits_lint_states() {
        let mut paths: Vec<String> = vec![
            " dist/ ".into(),
            "/abs".into(),
            "../up".into(),
            "".into(),
            "x".repeat(veld_core::config::MAX_TRASH_CLEANUP_PATH_LEN + 1),
            "~/x".into(),
        ];
        paths.extend((0..100).map(|i| format!("p{i}")));
        let got = runtime_patterns(&paths);
        assert_eq!(got[0], "dist");
        assert_eq!(
            got.len(),
            1 + veld_core::config::MAX_TRASH_CLEANUP_PATHS - 6,
            "only the first 64 are read, and the five invalid ones dropped"
        );
    }

    #[test]
    fn nested_duplicates_are_dropped() {
        let got = parse_untracked(b"logs/\0logs/a.log\0lo\0node_modules/\0x/node_modules/\0");
        let rels: Vec<&str> = got.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, ["lo", "logs", "node_modules", "x/node_modules"]);
    }

    #[test]
    fn paths_that_leave_the_root_are_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(safe_join(dir.path(), "../x").is_none());
        assert!(safe_join(dir.path(), "/etc").is_none());
        assert!(safe_join(dir.path(), ".git/objects").is_none());
        std::os::unix::fs::symlink("/tmp", dir.path().join("l")).unwrap();
        assert!(safe_join(dir.path(), "l/x").is_none(), "through a symlink");
        assert!(
            safe_join(dir.path(), "l").is_some(),
            "the link itself is fine"
        );
    }

    #[test]
    fn the_summary_names_the_largest_first() {
        let removed = Removed {
            bytes: 3 * 1024 * 1024 * 1024,
            paths: vec![
                ("a".into(), 1),
                ("node_modules".into(), 2 * 1024 * 1024 * 1024),
                ("target".into(), 1024 * 1024 * 1024),
                ("b".into(), 2),
            ],
            problems: vec![],
        };
        assert_eq!(
            summarize(&removed, &["step \"x\" exited 1".into()]),
            "freed 3.0 GB — node_modules, target, b and 1 more; step \"x\" exited 1"
        );
        assert_eq!(summarize(&Removed::default(), &[]), "", "quiet when idle");
        assert_eq!(
            summarize(&Removed::default(), &["step \"x\" exited 1".into()]),
            "removed nothing; step \"x\" exited 1"
        );
    }
}
