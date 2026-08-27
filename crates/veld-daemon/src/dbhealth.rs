//! Whether veld's own database is healthy, and whether it is being backed up.
//!
//! **Why this module exists.** On 2026-08-26 a 75-minute window of disk I/O
//! errors damaged one page of one table in a maintainer's `veld.db`. Everything
//! else kept working. The daemon logged `database disk image is malformed` 440
//! times over the following 17 hours, backups stopped completely, and nothing
//! told anybody: the only visible symptom was one project rendering as
//! `repository unavailable` in the IDE — a mislabel produced by an unrelated code
//! path that collapsed "the database write failed" into "git cannot read this
//! repo". The fault was found the next day, by eye, because the label looked
//! wrong.
//!
//! Three things follow from that incident, and they are the whole design:
//!
//! 1. **Waiting to be told is not enough.** Passive observation only ever sees
//!    the pages somebody happened to touch, and `pane_layouts` is touched only
//!    when a person rearranges panes. So this module *asks* — `PRAGMA
//!    quick_check` on a timer, which costs ~15 ms on a real 8.7 MB database and
//!    names exactly this fault class. Errors that other subsystems trip over are
//!    still folded in via [`note_error`], because they arrive sooner than the
//!    next probe.
//! 2. **`Db::open()` succeeding proves nothing.** It succeeded throughout the
//!    incident. Any health story built on "can we open it" reports OK while the
//!    file rots, which is precisely what `veld doctor` did.
//! 3. **The record of a fault must not live in the thing that is faulty.** The
//!    backup scheduler already demonstrated the failure mode: its "cannot open
//!    the database" arm wrote nothing, so the `backup.lastRun` kv row kept its
//!    last *successful* stamp for 17 hours while every attempt failed. So the
//!    live state here is process memory, and the one thing that must outlive the
//!    process — "we have already told the human" — is a small JSON file beside
//!    the database, never a row inside it.
//!
//! **Backup health is attempt-based, not age-based, and that is deliberate.**
//! `veld doctor` judges backups by the age of the newest artifact against a
//! threshold with a 12-hour floor, which is the right rule for a cold CLI check
//! that has no memory. It is the wrong rule here: 12 hours is *how the incident
//! stayed invisible overnight*. The daemon knows the outcome of the last attempt
//! it made, so a failing backup is reportable within one interval of the first
//! failure rather than after half a day.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use veld_core::db::{Db, DbError, DbFault, Integrity, backup};

/// How often the probe asks SQLite whether the file is still intact.
///
/// Five minutes, not five seconds: a quick check is cheap but not free, and the
/// fault it looks for does not appear and vanish between ticks. The paths that
/// *do* run every few seconds report through [`note_error`] instead, so a fault
/// somebody trips over is recorded immediately and the probe is the backstop for
/// the tables nothing reads.
const PROBE_INTERVAL_SECS: u64 = 300;

/// How long before the same unresolved fault is worth telling somebody about
/// again. A fault that is still there tomorrow is still worth a banner; one that
/// re-notified every probe would be noise, and noise is how a real warning gets
/// dismissed by reflex.
///
/// **Yes, this is the twelve hours `backup::overdue_after` argues against, and it
/// is not the same twelve hours.** There, twelve hours was the delay before the
/// user was told *anything* — the gap the incident lived in. Here the user has
/// already been told: the banner is up, undismissable, from the first detection,
/// and this only governs how often the *OS-level* nudge repeats for a fault they
/// are already looking at. Detection latency is bounded by the probe interval;
/// this is a repeat interval, and a repeat that is too eager is how a real
/// warning gets trained away.
const RENOTIFY_AFTER_HOURS: i64 = 12;

// ---------------------------------------------------------------------------
// The wire shape
// ---------------------------------------------------------------------------

/// Everything the IDE needs to say what is wrong and what to do about it.
///
/// Deliberately not shared with `veld doctor`: that lives in `crates/veld`, which
/// cannot see a `veld-daemon` type, and it answers the same questions from the
/// database and the backups directory directly — a cold check with no daemon to
/// ask is the case it exists for.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HealthView {
    /// When the probe last completed. `None` before the first one.
    pub checked_at: Option<DateTime<Utc>>,
    pub database: DatabaseView,
    pub backups: BackupView,
    pub restore: RestoreView,
    /// Set when this fault has not been announced to a human within
    /// [`RENOTIFY_AFTER_HOURS`]. A client that shows a system notification for
    /// it must claim it first (`POST /api/db-health/notified`), so that several
    /// open windows produce one banner rather than one each.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<NotifyView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseView {
    /// `"ok"`, or [`DbFault::as_str`] — `"corrupt"` / `"io"`.
    ///
    /// Note the hand-written [`Default`] below: the derived one would leave this
    /// `""`, and every client correctly reads "not ok" as a fault, so a
    /// default-constructed view serialised as *"your database is damaged"*.
    pub state: &'static str,
    /// What SQLite said. Empty when healthy.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_seen: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<DateTime<Utc>>,
    /// How many times this fault has been observed since it first appeared —
    /// the number that was 440 and invisible.
    pub hits: u64,
    /// The database file itself, so the UI can name it without guessing.
    pub path: String,
}

/// A healthy database is the only sane default, for the reason on
/// [`DatabaseView::state`].
impl Default for DatabaseView {
    fn default() -> Self {
        Self {
            state: "ok",
            detail: String::new(),
            first_seen: None,
            last_seen: None,
            hits: 0,
            path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupView {
    /// `"ok"` · `"failing"` (attempts are erroring) · `"overdue"` (no attempt
    /// when one was due) · `"off"` (switched off in settings) · `"unknown"`
    /// (nothing observed yet this process).
    ///
    /// Hand-written [`Default`] below, for the same reason as
    /// [`DatabaseView::state`]: a derived `""` reads as a fault to every client.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attempt: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    /// The newest artifact on disk, read from the filesystem — so this answers
    /// even when the database cannot be opened at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest: Option<backup::Artifact>,
    pub interval_minutes: i64,
}

/// `unknown` rather than `""`: nothing has been observed yet, which is not a
/// complaint. See [`BackupView::state`].
impl Default for BackupView {
    fn default() -> Self {
        Self {
            state: "unknown",
            last_ok: None,
            last_error: None,
            last_attempt: None,
            consecutive_failures: 0,
            newest: None,
            interval_minutes: veld_core::db::DEFAULT_BACKUP_INTERVAL_MINUTES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RestoreView {
    /// The newest artifact that passes a full `integrity_check` — what a restore
    /// would actually put back. `None` means there is nothing safe to offer, and
    /// the UI must say so rather than showing a button that cannot work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<backup::Artifact>,
    /// Whether this daemon comes back on its own after the restart a restore
    /// needs (an installed launchd/systemd job does; a dev instance does not).
    pub restarts_automatically: bool,
    /// What to run when it will not. Sent with the *health*, not only with the
    /// restore's response, because the screen that needs to name it is the one
    /// where somebody is deciding whether to go ahead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyView {
    /// Stable per fault kind, so claiming it is idempotent.
    pub id: String,
    pub title: String,
    pub body: String,
}

// ---------------------------------------------------------------------------
// Live state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Fault {
    kind: Option<DbFault>,
    detail: String,
    first_seen: Option<DateTime<Utc>>,
    last_seen: Option<DateTime<Utc>>,
    hits: u64,
}

impl Fault {
    /// Fold one observation in.
    ///
    /// A function of the fault rather than of the global, so the two properties
    /// that matter — a repeat observation counts without moving `first_seen`,
    /// and a *different* kind starts over — are testable without a process-wide
    /// singleton that every other test in the binary shares.
    fn observe(&mut self, kind: DbFault, detail: String, now: DateTime<Utc>) {
        // A different kind supersedes rather than accumulates: corruption after
        // an I/O window is the story, and keeping the earlier kind's
        // `first_seen` would date the wrong fault.
        if self.kind != Some(kind) {
            *self = Fault {
                kind: Some(kind),
                first_seen: Some(now),
                ..Default::default()
            };
        }
        self.detail = detail;
        self.last_seen = Some(now);
        self.hits = self.hits.saturating_add(1);
    }
}

#[derive(Debug, Clone, Default)]
struct BackupState {
    last_ok: Option<DateTime<Utc>>,
    last_attempt: Option<DateTime<Utc>>,
    last_error: Option<String>,
    consecutive_failures: u32,
}

#[derive(Debug, Default)]
struct State {
    checked_at: Option<DateTime<Utc>>,
    fault: Fault,
    backup: BackupState,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Record a database error observed by somebody else's code path.
///
/// Called from the funnels that already log one — the desktop API's `db_err`,
/// and each periodic task's error arm — so the *first* symptom a user's own
/// activity produces is recorded immediately rather than waiting up to
/// [`PROBE_INTERVAL_SECS`] for the probe to notice.
///
/// Errors that are not about the file (a refused row, a taken alias) are
/// ignored, which is what makes this safe to call from a hot path: the
/// classification is [`DbError::fault`], not "was there an error".
pub fn note_error(e: &DbError) {
    let Some(kind) = e.fault() else { return };
    state()
        .lock()
        .expect("dbhealth state poisoned")
        .fault
        .observe(kind, e.reported_message(), Utc::now());
}

/// [`note_error`] for the tasks whose fallible work is `anyhow`-typed.
///
/// The health monitor and the stats sampler return `anyhow::Result`, so their
/// error arms hold a boxed error rather than a [`DbError`]. `anyhow` keeps the
/// concrete type, so a downcast recovers it — and an error that is *not* a
/// database error (a probe that timed out, a process that vanished) downcasts to
/// nothing and is correctly ignored.
pub fn note_reported(e: &anyhow::Error) {
    if let Some(db_error) = e.downcast_ref::<DbError>() {
        note_error(db_error);
    }
}

/// Record the outcome of one backup attempt.
///
/// Called by the scheduler for every arm that tried and failed and had somewhere
/// to fail *to* — including the one that cannot open the database, the arm whose
/// silence is why backups were dead for 17 hours with nothing saying so. The one
/// exception is `plan`'s `Db::default_path()` early return, which cannot be
/// reached with a `Db` already in hand and is left alone rather than given a
/// report that could never fire.
pub fn note_backup_failure(error: impl std::fmt::Display) {
    let mut st = state().lock().expect("dbhealth state poisoned");
    st.backup.last_attempt = Some(Utc::now());
    st.backup.last_error = Some(error.to_string());
    st.backup.consecutive_failures = st.backup.consecutive_failures.saturating_add(1);
}

/// Record a backup that worked. Clears the failure streak, and only this does.
pub fn note_backup_success(taken_at: DateTime<Utc>) {
    let mut st = state().lock().expect("dbhealth state poisoned");
    st.backup.last_attempt = Some(taken_at);
    st.backup.last_ok = Some(taken_at);
    st.backup.last_error = None;
    st.backup.consecutive_failures = 0;
}

/// Whether the database is recorded as **corrupt** right now.
///
/// The precondition on the restore endpoint, and deliberately narrower than "any
/// fault": putting an old copy back is the answer to a damaged file, and it is
/// not the answer to a full disk, a read-only volume or an I/O error — none of
/// which a restore fixes, all of which are usually transient, and any of which
/// would otherwise arm a destructive endpoint on a database whose contents are
/// perfectly good.
#[must_use]
pub fn corruption_recorded() -> bool {
    state().lock().expect("dbhealth state poisoned").fault.kind == Some(DbFault::Corrupt)
}

/// Whether the database has been **checked** and found not to be corrupt.
///
/// The precondition for destructive housekeeping (the GC's retention deletes and
/// its page reclaim), and deliberately *not* `!corruption_recorded()`.
///
/// **The difference is a race that fires on every single daemon start.**
/// `corruption_recorded()` is also false before the first probe has completed,
/// and this task and the GC are spawned together with intervals that both tick
/// immediately — so "no corruption recorded" is the state the first GC pass
/// reliably observes, whatever the file actually looks like. Measured on a
/// deliberately damaged database: the GC pass logged `4000 logs pruned` at
/// `06:50:04.058` and this probe recorded the fault at `06:50:04.075`, 17 ms
/// later. A gate on the absence of a verdict would have let exactly the pass it
/// exists to stop straight through, and on a daemon that is restarting (auto
/// recovery, `veld update`) it would let *every* pass through.
///
/// So housekeeping waits for a positive answer. The cost is at most one skipped
/// pass — retention has a 168-hour horizon and does not care about 600 seconds —
/// and `checked_at` is normally set within tens of milliseconds of startup.
///
/// A fault that is *not* corruption (a full disk, a read-only volume, an I/O
/// blip) does not block housekeeping: those are transient and usually sit on
/// perfectly good data, and pausing retention on them would let a healthy file
/// grow forever. See [`corruption_recorded`] for the same distinction.
#[must_use]
pub fn verified_not_corrupt() -> bool {
    let st = state().lock().expect("dbhealth state poisoned");
    housekeeping_allowed(st.checked_at.is_some(), st.fault.kind)
}

/// The gate itself, as a function of the two facts rather than of the
/// process-wide singleton — same reason as [`Fault::observe`]: the properties
/// that matter are testable without a global every other test in this binary
/// shares.
fn housekeeping_allowed(checked: bool, fault: Option<DbFault>) -> bool {
    checked && fault != Some(DbFault::Corrupt)
}

/// Whether the first integrity probe has completed, so a caller can tell
/// "not checked yet" from "checked and damaged" and say the right thing.
#[must_use]
pub fn health_checked() -> bool {
    state()
        .lock()
        .expect("dbhealth state poisoned")
        .checked_at
        .is_some()
}

/// Forget the current fault, **and that anybody was told about it**.
///
/// Called after a restore. Clearing the in-memory fault alone would have been
/// nearly pointless — the state is process-local and this process is about to
/// exit — while leaving the marker file's entry in place had a real cost: a
/// failing volume that damages the *restored* file within
/// [`RENOTIFY_AFTER_HOURS`] is the expected shape of the fault, not a freak
/// case, and its notification would have been suppressed as a duplicate.
pub fn clear_fault() {
    let kind = {
        let mut st = state().lock().expect("dbhealth state poisoned");
        let kind = st.fault.kind;
        st.fault = Fault::default();
        kind
    };
    if let Some(kind) = kind {
        forget_notified(kind.as_str());
    }
}

// ---------------------------------------------------------------------------
// The probe
// ---------------------------------------------------------------------------

/// Run the health probe. Loops forever.
pub async fn run_probe() {
    let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(PROBE_INTERVAL_SECS));
    // A laptop that slept owes one check, not one per missed tick — the same
    // reasoning as the GC and backup schedulers'.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        // `quick_check` walks pages and the file read is blocking work; a runtime
        // worker parked in it is a request nobody is answering.
        if let Err(e) = tokio::task::spawn_blocking(probe_once).await {
            tracing::warn!("db health probe panicked: {e}");
        }
    }
}

/// One probe pass.
fn probe_once() {
    let now = Utc::now();
    match Db::open() {
        Ok(db) => match db.integrity() {
            Ok(Integrity::Ok) => {
                let mut st = state().lock().expect("dbhealth state poisoned");
                st.checked_at = Some(now);
                // Only the probe may declare the database healthy again. A
                // passive observer sees one statement succeed, which says
                // nothing about the page that is broken.
                if st.fault.kind.is_some() {
                    tracing::info!(
                        "database integrity is back to ok — clearing the recorded fault"
                    );
                    st.fault = Fault::default();
                }
            }
            Ok(Integrity::Damaged(detail)) => {
                tracing::error!("database integrity check failed: {detail}");
                let mut st = state().lock().expect("dbhealth state poisoned");
                st.checked_at = Some(now);
                st.fault.observe(DbFault::Corrupt, detail, now);
            }
            // An error the classifier does not recognise as a file fault: worth a
            // line, not worth claiming the database is damaged.
            Err(e) => {
                tracing::warn!("database integrity check could not run: {e}");
                note_error(&e);
                state().lock().expect("dbhealth state poisoned").checked_at = Some(now);
            }
        },
        Err(e) => {
            tracing::warn!("db health probe could not open the database: {e}");
            note_error(&e);
        }
    }

    check_overdue_backup();
}

/// The deadman half: a backup that was due and did not happen.
///
/// Attempt tracking cannot see this — a scheduler that never ran made no
/// attempt to record. Judged from the newest artifact on disk, because that is
/// readable with no database at all, which is the state this is most likely to
/// be true in.
fn check_overdue_backup() {
    let (enabled, interval) = backup_prefs();
    if !enabled {
        return;
    }
    // **A database with nothing in it is not owed a backup**, and this guard is
    // the difference between a warning and a lie. `backup::plan` deliberately
    // declines to copy a database that `holds_user_state()` says is empty — a
    // fresh install with no repositories, no settings and no runs — and records
    // no attempt when it does, so without this check an idle new install crossed
    // two intervals and told its owner "Veld has stopped backing up", with a
    // system notification, about a machine behaving exactly as designed.
    //
    // Unreadable (`Err`) counts as "has state": a database that cannot answer
    // this question must not be able to silence the alarm about its own backups.
    let holds_state = Db::open()
        .and_then(|db| db.holds_user_state())
        .unwrap_or(true);
    if !holds_state {
        return;
    }
    let Some(dir) = backup_dir() else { return };
    let newest = backup::list(&dir).into_iter().next();
    // The shared rule, so `veld backup` and this probe agree on the word
    // "overdue". See `backup::overdue_after` for why it is not doctor's rule.
    let overdue_after = backup::overdue_after(interval);
    let now = Utc::now();
    let overdue = match &newest {
        // A future-dated artifact is not evidence of anything; treat it as no
        // artifact at all rather than as one that can never go stale.
        Some(a) if a.taken_at <= now => now - a.taken_at > overdue_after,
        Some(_) => uptime() > overdue_after,
        // Nothing at all is only overdue once this daemon has been up long
        // enough to have taken one. A fresh install has no backups and is not
        // broken.
        None => uptime() > overdue_after,
    };
    if !overdue {
        return;
    }
    let mut st = state().lock().expect("dbhealth state poisoned");
    // Do not overwrite a real error with the derived one — "the last attempt
    // said the disk is full" is more useful than "nothing happened".
    if st.backup.last_error.is_none() {
        st.backup.last_error = Some(match &newest {
            Some(a) => format!(
                "no backup has been written since {} — one was due every {interval} minute(s)",
                a.taken_at.to_rfc3339()
            ),
            None => "no backups have ever been written".to_string(),
        });
        st.backup.consecutive_failures = st.backup.consecutive_failures.max(1);
    }
}

/// How long this process has been up. Used only to keep a fresh install from
/// reporting "no backups" as a fault before it has had a chance to take one.
fn uptime() -> Duration {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    Duration::from_std(start.elapsed()).unwrap_or_else(|_| Duration::zero())
}

/// Mark the process start, so [`uptime`] measures from boot rather than from the
/// first time anything happened to ask.
pub fn mark_start() {
    let _ = uptime();
}

fn backup_prefs() -> (bool, i64) {
    match Db::open() {
        Ok(db) => {
            let prefs = db.backup_prefs();
            (prefs.enabled, prefs.interval_minutes.max(1))
        }
        // The database is the thing that is broken; assume backups are on (the
        // default) so a corrupt database cannot silence its own alarm.
        Err(_) => (true, veld_core::db::DEFAULT_BACKUP_INTERVAL_MINUTES),
    }
}

fn backup_dir() -> Option<PathBuf> {
    Db::open()
        .ok()
        .and_then(|db| db.backup_prefs().dir)
        .or_else(backup::default_dir)
}

// ---------------------------------------------------------------------------
// Notify-once memory
// ---------------------------------------------------------------------------

/// The one piece of state that has to outlive the process, kept out of the
/// database on purpose (see the module docs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct NotifiedFile {
    /// Fault id → when a human was told. Keyed by *kind*, not by the error
    /// text: a detail string that flaps between two pages would otherwise mint
    /// a fresh identity and re-notify on every probe.
    #[serde(default)]
    notified: std::collections::BTreeMap<String, DateTime<Utc>>,
}

/// Every notification id this module can mint.
///
/// **Enforced, not documented.** The claim endpoint used to bound its input by
/// length and carry a comment saying the ids are "a closed set of short words",
/// which is the shape of a guard that is not one: every distinct id a caller sent
/// became a permanent key in a file the daemon rewrites whole and re-parses on
/// every health poll. Any page that can reach this daemon could grow it without
/// limit. A list the code checks cannot drift from the list the code mints.
pub const NOTIFY_IDS: [&str; 4] = [
    // Derived from the fault kinds rather than spelled again: a fifth `DbFault`
    // variant would otherwise mint an id this list does not contain, the claim
    // endpoint would refuse it on every poll, and the OS notification for that
    // new fault would simply never fire — with nothing anywhere reporting the
    // mismatch.
    DbFault::Corrupt.as_str(),
    DbFault::Io.as_str(),
    BACKUPS_FAILING_ID,
    BACKUPS_OVERDUE_ID,
];

/// The two backup-side notification ids, named once so the minting site and
/// [`NOTIFY_IDS`] cannot spell them differently.
const BACKUPS_FAILING_ID: &str = "backups-failing";
const BACKUPS_OVERDUE_ID: &str = "backups-overdue";

/// Whether `id` is one of [`NOTIFY_IDS`].
#[must_use]
pub fn is_notify_id(id: &str) -> bool {
    NOTIFY_IDS.contains(&id)
}

/// Where the notify-once memory lives: beside the database, so a dev instance
/// keeps its own and cannot silence the installed one.
fn notified_path() -> Option<PathBuf> {
    let db = Db::default_path().ok()?;
    Some(db.parent()?.join("db-health.json"))
}

fn read_notified() -> NotifiedFile {
    notified_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Claim the right to tell a human about `id`, returning whether this caller won.
///
/// **A test-and-set, because "one banner between all the windows" is a promise.**
/// Every open IDE window sees the same pending notification on the same poll
/// tick, so an unconditional "record it" left them all believing they had the
/// right to raise a system banner — the docs claimed one notification and the
/// code delivered one per window. The whole read-modify-write happens under one
/// mutex, and this daemon is the only writer of the file, so exactly one caller
/// sees `true`.
///
/// **Fails open.** The condition that produces faults — a full disk, a failing
/// volume — is also the condition that stops this file being written, and the
/// only safe direction there is a duplicate notification rather than silence. So
/// a write failure still answers `true`.
pub fn claim_notified(id: &str) -> bool {
    let _held = marker_lock();

    let Some(path) = notified_path() else {
        return true;
    };
    let mut file = read_notified();
    // Someone already claimed it inside the cooldown: they are showing the
    // banner, and this caller must not show a second one.
    if let Some(when) = file.notified.get(id).copied()
        && Utc::now() - when < Duration::hours(RENOTIFY_AFTER_HOURS)
    {
        return false;
    }
    file.notified.insert(id.to_string(), Utc::now());
    let Ok(json) = serde_json::to_string_pretty(&file) else {
        return true;
    };
    // Atomic replace, so a crash mid-write cannot leave a truncated file that
    // parses as "never told anybody".
    //
    // **Created exclusively, which is the load-bearing half.** A plain
    // `fs::write` follows a symlink, so a name planted in the database's
    // directory — which `create_dir_all` leaves at the umask default, and which
    // `VELD_DB_PATH` can put anywhere — redirected the write. `create_new` is
    // `O_CREAT|O_EXCL`, which fails on both an existing file and a symlink: the
    // same reasoning, and the same fix, as `backup::create`'s artifact writer.
    //
    // The pid in the name is belt only. Concurrent writers within this process
    // are serialised by `marker_lock`, and two *processes* keep separate marker
    // files anyway (see `notified_path`).
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    // A leftover from a crashed write would make `create_new` fail forever.
    // Removing a symlink here removes the link, not its target, so this does not
    // reopen the hole `create_new` closes.
    let _ = std::fs::remove_file(&tmp);
    let written =
        write_new_owner_only(&tmp, json.as_bytes()).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!(
            "could not record that the database fault was announced ({}): {e} — the \
             next client may show the notification again",
            path.display()
        );
    }
    // Won the claim either way: an unrecorded claim costs a duplicate
    // notification, and silence is the one outcome that is not acceptable.
    true
}

/// Create `path`, failing if anything is already there, and never widening the
/// mode after the content lands.
///
/// The mode is set **in the open call**, not with a `set_permissions` afterwards:
/// the earlier version wrote the bytes first and chmodded second, which leaves a
/// window at the umask default. This file holds only fault ids and timestamps —
/// the previous comment here claimed it could name paths in the user's home,
/// which was simply false — but a file the daemon rewrites on a schedule is worth
/// creating the careful way regardless, and `O_EXCL` is what refuses a planted
/// symlink.
fn write_new_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Serialises every read-modify-write of the marker file. Not the file's own
/// lock across processes — a dev instance and the installed daemon keep separate
/// files, by [`notified_path`] — just this process's several windows racing one
/// poll tick.
fn marker_lock() -> std::sync::MutexGuard<'static, ()> {
    static MARKER: Mutex<()> = Mutex::new(());
    // A `Mutex<()>` guards nothing that can be left inconsistent, so recovering
    // from a poisoning is safe and refusing to would wedge the notification path.
    MARKER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drop one id from the notify-once memory, so the next occurrence of that fault
/// is announced again. See [`clear_fault`].
fn forget_notified(id: &str) {
    // **The same lock as `claim_notified`.** Both do a read-modify-write of one
    // file through a temp path built the same way, so without this a forget could
    // unlink a claim's in-flight temp — and a lost forget is exactly the
    // suppressed-notification bug this function exists to prevent.
    let _held = marker_lock();
    let Some(path) = notified_path() else { return };
    let mut file = read_notified();
    if file.notified.remove(id).is_none() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(&file) else {
        return;
    };
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) =
        write_new_owner_only(&tmp, json.as_bytes()).and_then(|()| std::fs::rename(&tmp, &path))
    {
        let _ = std::fs::remove_file(&tmp);
        // Failing to forget only costs a suppressed duplicate notification, so
        // this is a debug line rather than a warning.
        tracing::debug!("could not clear the announced database fault: {e}");
    }
}

// ---------------------------------------------------------------------------
// Reading it back
// ---------------------------------------------------------------------------

/// The newest artifact and the restorable candidate, recomputed at most once per
/// [`ARTIFACT_CACHE_SECS`].
///
/// **This is a cache because the endpoint is ungated and the work is not cheap.**
/// `GET /api/db-health` carries no CSRF header by design (it is a read), so any
/// page that can reach this daemon can call it, and every open IDE window polls
/// it every five seconds anyway. Uncached it did: two `Db::open()`s, two
/// `backup::list` directory walks, and — through `newest_restorable` — a **full
/// `PRAGMA integrity_check`**, which is the expensive check this module
/// deliberately avoids on its own probe. Worse in the fault state, where
/// `newest_passing` walks every artifact until one passes, and worse again when
/// `backup.dir` is the network folder its own help text suggests: a dead mount
/// parks a blocking-pool thread per poll.
///
/// The freshness cost is nil in practice — artifacts appear on the backup
/// schedule, measured in minutes.
fn artifacts_cached() -> (Option<backup::Artifact>, Option<backup::Artifact>) {
    /// Deliberately shorter than the probe interval, so a `veld backup now` taken
    /// by hand shows up quickly, and far longer than the UI's 5-second poll.
    const ARTIFACT_CACHE_SECS: u64 = 30;
    static CACHE: OnceLock<Mutex<Option<(std::time::Instant, ArtifactPair)>>> = OnceLock::new();

    let cache = CACHE.get_or_init(|| Mutex::new(None));
    {
        let held = cache.lock().expect("dbhealth artifact cache poisoned");
        if let Some((at, pair)) = &*held
            && at.elapsed() < std::time::Duration::from_secs(ARTIFACT_CACHE_SECS)
        {
            return pair.clone();
        }
    }

    // Computed outside the lock: this is the slow part, and holding the mutex
    // across it would serialise every window's poll behind one directory walk.
    // Two concurrent misses may both compute, which is a wasted walk and not a
    // wrong answer.
    let started = std::time::Instant::now();
    let dir = backup_dir();
    let newest = dir
        .as_ref()
        .and_then(|d| backup::list(d).into_iter().next());
    // The candidate is deep-checked, exactly like `veld backup restore` picks
    // one: offering a button that `backup::restore` will refuse is worse than
    // saying there is nothing to offer.
    let candidate = dir
        .as_ref()
        .and_then(|d| backup::newest_restorable(d, Utc::now()));
    let pair = (newest, candidate);

    // **Stamped from before the walk, and never overwriting a newer entry.** Two
    // concurrent misses would otherwise let the slower walk publish last, with a
    // fresh timestamp — so a 20-second walk on a dead network `backup.dir` could
    // serve results already 20 seconds old for another 30.
    let mut held = cache.lock().expect("dbhealth artifact cache poisoned");
    let newer_already_there = held.as_ref().is_some_and(|(at, _)| *at > started);
    if !newer_already_there {
        *held = Some((started, pair.clone()));
    }
    pair
}

/// What [`artifacts_cached`] holds: the newest artifact, and the newest one that
/// would survive a restore.
type ArtifactPair = (Option<backup::Artifact>, Option<backup::Artifact>);

/// Which word describes the state of backups.
///
/// **A function of its inputs, not of the process**, so the ordering below is
/// testable — which matters because the ordering *was* the bug. The first
/// version tested `consecutive_failures` before `fresh_on_disk`, so the
/// "a fresh copy counts, whoever wrote it" fallback was unreachable in every
/// state that had recorded a failure, i.e. in exactly the states where it is the
/// point: `check_overdue_backup` sets a failure, `veld backup now` then writes a
/// good copy, and the undismissable banner went on saying "Veld has stopped
/// backing up" until the daemon's own next successful pass — up to two intervals,
/// which at the maximum interval is two days.
///
/// So evidence on disk wins. A copy written minutes ago means the user is
/// protected, whoever wrote it; `last_error` still travels in the payload, so the
/// dialog can say what went wrong without the headline being wrong.
fn backup_state_word(state: &BackupState, enabled: bool, fresh_on_disk: bool) -> &'static str {
    if !enabled {
        return "off";
    }
    if fresh_on_disk {
        return "ok";
    }
    if state.consecutive_failures > 0 {
        // "Failing" means attempts are being made and erroring; "overdue" means
        // no attempt was recorded at all, which is what the deadman check
        // reports. Different causes, so they must not merge into one word.
        return if state.last_attempt.is_some() {
            "failing"
        } else {
            "overdue"
        };
    }
    if state.last_ok.is_some() {
        return "ok";
    }
    "unknown"
}

/// The command that starts this daemon again — **only when there is one that is
/// both unambiguous and safe to name.**
///
/// `None` for a dev instance, and that is a correction rather than a gap. The
/// obvious answer, which the CLI's own `restart_hint` gives and which this
/// function used to copy, is `veld start` — and a maintainer reading it out of
/// this dialog said plainly that they did not understand it. They were right not
/// to: on a dev instance the daemon is a *node of a veld run*, so "start the
/// daemon" is not a thing anybody does, and worse, the presets that start it
/// commonly carry `dev-db:fresh` or `dev-db:from-real`, either of which recreates
/// the database — replacing the copy the user has just finished restoring. A hint
/// that can undo the action it follows is worse than no hint, so this says
/// nothing and the UI explains the situation instead.
///
/// An installed-but-unmanaged daemon does have an unambiguous answer, and gets it.
pub fn restart_hint() -> Option<String> {
    if !Db::uses_installed_database() {
        return None;
    }
    Some(if cfg!(target_os = "macos") {
        "launchctl kickstart -k gui/$(id -u)/dev.veld.daemon".to_string()
    } else {
        "systemctl --user restart veld-daemon".to_string()
    })
}

/// Whether a service manager will start this daemon again if it exits.
///
/// **Not `Db::uses_installed_database()`, which was the first answer and is a
/// different question.** That one asks "am I pointed at the installed database
/// path", and it is wrong in both directions here: a `VELD_DB_PATH` set on a
/// daemon launchd really does own reports "you must start it yourself" (it will
/// come back), and — the one that costs something — an installed-path daemon
/// somebody started by hand, or on a Linux box with no user systemd, is told
/// "Veld restarts itself" immediately after its database has been replaced, and
/// then never comes back.
///
/// Asked of the environment the service manager itself sets, which is free and
/// needs no subprocess on a request path.
///
/// **"Is the variable set" is not the question, and getting that wrong is worse
/// than the bug it replaced.** launchd exports `XPC_SERVICE_NAME=0` to every
/// process descended from a GUI-launched app, so an ordinary Terminal — and
/// therefore a hand-started daemon, or the auto-bootstrapped one from the
/// zero-sudo install path — inherits it. A non-empty test is *true* there, which
/// is exactly the case this must refuse: the dialog would promise a restart that
/// never comes, immediately after replacing the database. So the value is matched
/// against the job's own label. (Verified: this repo's shell reports `"0"`, the
/// installed daemon reports `"dev.veld.daemon"`.)
///
/// systemd has the same inheritance problem — `INVOCATION_ID` reaches every
/// descendant of a unit, including a shell inside `gnome-terminal-server.service`
/// — so the unit is identified from the process's own cgroup instead.
///
/// `veld backup restore`'s `restart_hint` keeps inferring from the database path,
/// and that stays correct: it prints a suggestion, where this asserts what is
/// about to happen.
pub fn service_manager_will_restart() -> bool {
    if cfg!(target_os = "macos") {
        return std::env::var("XPC_SERVICE_NAME")
            .is_ok_and(|v| v == veld_core::setup::DAEMON_LABEL);
    }
    // The unit's own name appears in the cgroup path of its processes; a shell
    // that merely inherited `INVOCATION_ID` sits in a different unit's cgroup.
    std::fs::read_to_string("/proc/self/cgroup")
        .is_ok_and(|cgroup| cgroup.contains("veld-daemon.service"))
}

/// Build the view the API serves.
///
/// **`_blocking` in the name because a doc comment does not stop anybody.** This
/// walks a directory and can deep-check an artifact, so calling it inline on the
/// async router parks a runtime worker — and on a network `backup.dir` that has
/// gone away, parks it uninterruptibly. The name is the reminder at the call
/// site, where the mistake would be made.
pub fn view_blocking() -> HealthView {
    let (checked_at, fault, backup_state) = {
        let st = state().lock().expect("dbhealth state poisoned");
        (st.checked_at, st.fault.clone(), st.backup.clone())
    };

    let (enabled, interval_minutes) = backup_prefs();
    let (newest, candidate) = artifacts_cached();

    // **A fresh copy on disk counts, whoever wrote it.** This process's own
    // `last_ok` is empty for up to a whole interval after every restart, and
    // `veld backup now` writes an artifact this daemon never hears about — so
    // keying "ok" on our own memory alone reported `unknown` with a
    // minutes-old backup sitting right there, and put the IDE at odds with what
    // `veld backup` was showing on the same machine.
    // **`taken_at <= now` is half the test, and the half that was missing.** A
    // future-dated artifact — an NTP step, or a NAS folder written by a machine
    // whose clock runs ahead — makes `now - taken_at` negative, which satisfies
    // any upper bound. Since `backup_state_word` lets `fresh_on_disk` outrank a
    // recorded failure, one such file reported "backups ok" permanently while
    // every attempt failed: the incident's silence, rebuilt out of the fix for
    // it. `backup::newest_passing` already guards the same way, so the case was
    // known to be real in this codebase.
    let now = Utc::now();
    let fresh_on_disk = newest.as_ref().is_some_and(|a| {
        a.taken_at <= now && now - a.taken_at <= backup::overdue_after(interval_minutes)
    });

    let backups = BackupView {
        state: backup_state_word(&backup_state, enabled, fresh_on_disk),
        last_ok: backup_state.last_ok,
        last_error: backup_state.last_error.clone(),
        last_attempt: backup_state.last_attempt,
        consecutive_failures: backup_state.consecutive_failures,
        newest,
        interval_minutes,
    };

    let database = DatabaseView {
        state: fault.kind.map_or("ok", DbFault::as_str),
        detail: fault.detail.clone(),
        first_seen: fault.first_seen,
        last_seen: fault.last_seen,
        hits: fault.hits,
        path: Db::default_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    };

    let notify = pending_notification(&database, &backups);

    HealthView {
        checked_at,
        database,
        backups,
        restore: {
            let automatic = service_manager_will_restart();
            RestoreView {
                candidate,
                restarts_automatically: automatic,
                restart_hint: if automatic { None } else { restart_hint() },
            }
        },
        notify,
    }
}

/// What there is to say about this pair of views, before asking whether anybody
/// has been told yet.
///
/// A pure function of the two views — no clock, no filesystem, no database — so
/// the ranking and the wording are testable, which is the house pattern for "the
/// decision" in this repo. The cooldown lives in [`pending_notification`],
/// because that is the half that needs to read state.
fn notification_for(db: &DatabaseView, backups: &BackupView) -> Option<NotifyView> {
    // Corruption outranks a backup complaint: they are usually both true at
    // once (a database that cannot be read cannot be copied either) and the
    // actionable one is the database.
    if db.state != "ok" {
        return Some(NotifyView {
            id: db.state.to_string(),
            title: "Veld's database is damaged".to_string(),
            // **Not SQLite's words.** Quoting the detail here produced
            // *"\*\*\* in database main \*\*\*. Open Veld to restore the newest
            // backup."* on a real fault — `quick_check`'s output opens with a
            // banner line, so the first line is decoration and the finding is on
            // the second. Rather than teach a notification to parse pragma
            // output, say what it means for the reader; the raw detail is in the
            // dialog, where there is room for it and a reason to show it.
            body: "Your projects, worktrees and settings are at risk. \
                   Open Veld to restore the newest backup."
                .to_string(),
        });
    }
    if backups.state == "failing" || backups.state == "overdue" {
        return Some(NotifyView {
            id: if backups.state == "failing" {
                BACKUPS_FAILING_ID.to_string()
            } else {
                BACKUPS_OVERDUE_ID.to_string()
            },
            title: "Veld has stopped backing up".to_string(),
            body: backups
                .last_error
                .as_deref()
                .map(first_sentence)
                .unwrap_or_else(|| "No recent backup could be written.".to_string()),
        });
    }
    None
}

/// [`notification_for`], minus anything a human has already been told about
/// within [`RENOTIFY_AFTER_HOURS`].
fn pending_notification(db: &DatabaseView, backups: &BackupView) -> Option<NotifyView> {
    let notify = notification_for(db, backups)?;
    if let Some(when) = read_notified().notified.get(&notify.id).copied()
        && Utc::now() - when < Duration::hours(RENOTIFY_AFTER_HOURS)
    {
        return None;
    }
    Some(notify)
}

/// Keep a notification body to one readable line.
///
/// Used for the backup message, whose `last_error` really is a sentence
/// ("backup skipped — cannot open the database"). Deliberately *not* used for a
/// SQLite integrity detail: see the comment in [`notification_for`].
fn first_sentence(detail: &str) -> String {
    let one_line = detail.split(['\n', '\r']).next().unwrap_or(detail).trim();
    if one_line.chars().count() <= 140 {
        return one_line.to_string();
    }
    let cut: String = one_line.chars().take(137).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_view(state: &'static str) -> DatabaseView {
        DatabaseView {
            state,
            ..Default::default()
        }
    }

    fn backup_view(state: &'static str) -> BackupView {
        BackupView {
            state,
            ..Default::default()
        }
    }

    /// **The bug this gate was rewritten for, pinned.** Destructive housekeeping
    /// must wait for a positive answer, because "no corruption recorded" is also
    /// what an un-probed database looks like — and the GC and the probe are
    /// spawned together with intervals that both tick immediately, so the first
    /// GC pass of every daemon start lands in exactly that window.
    ///
    /// Measured on a deliberately damaged database before this was fixed: the
    /// pass logged `4000 logs pruned` 17 ms *before* the probe recorded the
    /// fault. Spell this `!corruption_recorded()` again and the whole fix
    /// silently reverts to letting the first pass through.
    #[test]
    fn housekeeping_waits_for_a_verdict_rather_than_for_the_absence_of_one() {
        assert!(
            !housekeeping_allowed(false, None),
            "nothing checked yet is NOT permission to delete and relocate pages — this is \
             the 17 ms window the first GC pass of every daemon start runs in"
        );
        assert!(
            !housekeeping_allowed(false, Some(DbFault::Corrupt)),
            "un-probed and already known damaged is emphatically not allowed"
        );
        assert!(
            housekeeping_allowed(true, None),
            "checked and clean is the only state that permits housekeeping"
        );
        assert!(
            !housekeeping_allowed(true, Some(DbFault::Corrupt)),
            "checked and corrupt must stop retention and the page reclaim"
        );
    }

    /// Corruption is the only fault that pauses housekeeping. An I/O blip or a
    /// full disk is transient and usually sits on perfectly good data, so
    /// pausing retention on one would let a healthy database grow without bound
    /// — trading the bug for a worse version of the bug the retention exists to
    /// prevent.
    #[test]
    fn a_transient_fault_does_not_pause_housekeeping() {
        assert!(housekeeping_allowed(true, Some(DbFault::Io)));
    }

    /// A healthy pair says nothing. The banner and the notification are the same
    /// decision, so "nothing to say" has to be representable.
    #[test]
    fn a_healthy_database_produces_no_notification() {
        assert!(notification_for(&db_view("ok"), &backup_view("ok")).is_none());
        assert!(notification_for(&db_view("ok"), &backup_view("off")).is_none());
        assert!(notification_for(&db_view("ok"), &backup_view("unknown")).is_none());
    }

    /// Corruption outranks a backup complaint: they will usually be true at the
    /// same time (a database that cannot be read cannot be copied either), and
    /// the actionable one is the database.
    #[test]
    fn the_database_fault_outranks_the_backup_fault() {
        let n = notification_for(&db_view("corrupt"), &backup_view("failing"))
            .expect("a corrupt database is worth saying");
        assert_eq!(n.id, "corrupt");
    }

    /// Both backup fault states are worth a word, and they are distinguishable —
    /// "every attempt is erroring" and "nothing has attempted" have different
    /// causes and the id must not merge them.
    #[test]
    fn failing_and_overdue_backups_are_announced_separately() {
        let failing = notification_for(&db_view("ok"), &backup_view("failing")).unwrap();
        let overdue = notification_for(&db_view("ok"), &backup_view("overdue")).unwrap();
        assert_eq!(failing.id, "backups-failing");
        assert_eq!(overdue.id, "backups-overdue");
        assert_ne!(failing.id, overdue.id);
    }

    /// **The branch order, which was the bug.** A fresh copy on disk means the
    /// user is protected, so it has to outrank a failure this process is still
    /// remembering — otherwise `check_overdue_backup` records a failure, a
    /// `veld backup now` fixes the actual problem, and the undismissable banner
    /// goes on saying "Veld has stopped backing up" until the daemon's own next
    /// successful pass (up to two intervals — two days at the maximum interval).
    #[test]
    fn a_fresh_copy_on_disk_outranks_a_remembered_failure() {
        let failing = BackupState {
            last_attempt: Some(Utc::now()),
            last_error: Some("cannot open the database".into()),
            consecutive_failures: 17,
            last_ok: None,
        };
        assert_eq!(
            backup_state_word(&failing, true, true),
            "ok",
            "a minutes-old artifact is evidence that backups work"
        );
        assert_eq!(
            backup_state_word(&failing, true, false),
            "failing",
            "with nothing fresh on disk the remembered failure is the answer"
        );
    }

    /// The two failure words describe different causes and must not merge:
    /// "failing" is attempts that error, "overdue" is no attempt at all.
    #[test]
    fn failing_and_overdue_are_told_apart_by_whether_anything_tried() {
        let attempted = BackupState {
            last_attempt: Some(Utc::now()),
            consecutive_failures: 1,
            ..Default::default()
        };
        let never_attempted = BackupState {
            consecutive_failures: 1,
            ..Default::default()
        };
        assert_eq!(backup_state_word(&attempted, true, false), "failing");
        assert_eq!(backup_state_word(&never_attempted, true, false), "overdue");
    }

    /// Switched off is a choice, and it outranks everything — including a stale
    /// failure recorded before the user turned backups off.
    #[test]
    fn disabled_backups_are_never_a_complaint() {
        let failing = BackupState {
            consecutive_failures: 5,
            last_attempt: Some(Utc::now()),
            ..Default::default()
        };
        assert_eq!(backup_state_word(&failing, false, false), "off");
        assert_eq!(
            backup_state_word(&BackupState::default(), false, false),
            "off"
        );
    }

    /// Nothing observed and nothing on disk is `unknown`, not a fault: a daemon
    /// that started a minute ago has simply not reached its first backup.
    #[test]
    fn nothing_observed_yet_is_not_a_fault() {
        assert_eq!(
            backup_state_word(&BackupState::default(), true, false),
            "unknown"
        );
    }

    /// The two `Default` impls exist because `""` reads as a fault to every
    /// client — `noticeFor` treats anything but a known-good word as damage.
    #[test]
    fn default_views_do_not_claim_a_fault() {
        assert_eq!(DatabaseView::default().state, "ok");
        assert_eq!(BackupView::default().state, "unknown");
        assert!(
            notification_for(&DatabaseView::default(), &BackupView::default()).is_none(),
            "a default-constructed pair must not announce anything"
        );
    }

    /// The claim endpoint's input is checked against the set the daemon mints,
    /// not against a length — the file it keys is rewritten whole and re-parsed
    /// on every poll, and this router is same-origin with any page a run serves.
    #[test]
    fn only_the_daemons_own_notification_ids_are_accepted() {
        for id in NOTIFY_IDS {
            assert!(is_notify_id(id), "{id} is minted by this module");
        }
        for id in [
            "",
            "corrupt ",
            "CORRUPT",
            "../../etc/passwd",
            "backups-",
            "x",
        ] {
            assert!(!is_notify_id(id), "{id:?} must be refused");
        }
    }

    /// Every id `notification_for` can mint has to be one the claim endpoint will
    /// accept. Nothing else ties the two lists together, and a drift would make
    /// the notification unclaimable — so the banner would re-fire every poll.
    #[test]
    fn every_minted_id_is_accepted_by_the_allowlist() {
        let cases = [
            (db_view("corrupt"), backup_view("ok")),
            (db_view("io"), backup_view("ok")),
            (db_view("ok"), backup_view("failing")),
            (db_view("ok"), backup_view("overdue")),
        ];
        for (db, backups) in cases {
            let minted = notification_for(&db, &backups)
                .unwrap_or_else(|| panic!("{}/{} should announce", db.state, backups.state));
            assert!(
                is_notify_id(&minted.id),
                "minted {:?} which the endpoint would refuse",
                minted.id
            );
        }
    }

    /// The notification about a damaged database must not quote SQLite.
    ///
    /// Driving this on a real corrupted file produced the body
    /// `"*** in database main ***. Open Veld to restore the newest backup."` —
    /// `quick_check` opens with a banner line and puts the finding on the next
    /// one, so "the first line" is decoration. The regression guarded here is
    /// the pragma's punctuation reaching a user's notification centre.
    #[test]
    fn the_damaged_notification_does_not_quote_sqlite() {
        let notice = notification_for(
            &DatabaseView {
                state: "corrupt",
                detail: "*** in database main ***\nTree 15 page 15: btreeInitPage() returns error code 11".into(),
                ..Default::default()
            },
            &backup_view("ok"),
        )
        .expect("a corrupt database is worth saying");
        assert!(
            !notice.body.contains("***"),
            "the banner line must not reach the body: {:?}",
            notice.body
        );
        assert!(
            !notice.body.contains("btreeInitPage"),
            "nor the finding — that belongs in the dialog: {:?}",
            notice.body
        );
        assert!(notice.body.contains("at risk"), "{:?}", notice.body);
    }

    /// A long SQLite detail must not be pasted whole into a system banner.
    #[test]
    fn a_long_detail_is_trimmed_to_one_line() {
        let long = format!("first line {}", "x".repeat(500));
        let trimmed = first_sentence(&format!("{long}\nsecond line"));
        assert!(trimmed.chars().count() <= 140, "got {}", trimmed.len());
        assert!(trimmed.ends_with('…'));
        assert!(!trimmed.contains("second line"), "only the first line");
    }

    /// The trimmer must not mangle a detail that already fits.
    #[test]
    fn a_short_detail_survives_intact() {
        assert_eq!(
            first_sentence("database disk image is malformed"),
            "database disk image is malformed"
        );
    }

    /// **The structural half of "no fault goes unrecorded", pinned.**
    ///
    /// This started as a source-scanning grep over every daemon file, in the
    /// spirit of `worktree_trash.rs`'s
    /// `only_one_function_runs_git_worktree_remove`, and it is worth writing down
    /// why that is gone rather than quietly deleted. It could not tell a
    /// `DbError` from anything else interpolated into a `warn!`: it flagged
    /// `"database restore panicked: {e}"` (a `JoinError`) as an unreported fault,
    /// and it missed `"ide state: cannot open the database"` because its phrase
    /// was `"cannot open database"` — one word of wording drift. A guard that
    /// cries wolf *and* misses trains people to ignore it, which costs more than
    /// having no guard at all.
    ///
    /// What replaced it is not a grep. `Db::open()` reports every failure itself
    /// (`veld_core::db::observe_open_failures`), so the 27 call sites that open
    /// the database — including the several that map the error straight to a
    /// status and discard it — are covered by construction rather than by
    /// discipline. This test pins the one line that arrangement depends on.
    ///
    /// Per-query failures still report at their own funnels, and that part is
    /// genuinely a convention: `desktop::db_err` covers the whole desktop router
    /// through its `Any` downcast, and the handful of routers with their own error
    /// shaping call `note_error` directly. A missed one there costs a delayed
    /// notification — the probe still finds the damage within its interval — not a
    /// silent fault.
    #[test]
    fn the_open_failure_observer_is_installed() {
        let main = include_str!("main.rs");
        assert!(
            main.contains("observe_open_failures"),
            "`main` must install the database open-failure observer — without it \
             every `Db::open()` failure in this process goes unrecorded unless its \
             own call site remembers to report it, which is the discipline that \
             failed in three consecutive review rounds"
        );
        assert!(
            main.contains("dbhealth::note_error"),
            "the observer must feed `dbhealth`, or it records nothing"
        );
    }

    /// `note_reported` reaches a `DbError` through anyhow's context layers.
    ///
    /// The health monitor and the stats sampler are `anyhow`-typed, so this
    /// downcast is the only thing that classifies their faults. Whether it
    /// survives a `.context("…")` was an open question rather than a known
    /// property — and "the first person to add context silently turns this off"
    /// has to be pinned rather than assumed, because nothing about adding context
    /// looks dangerous.
    ///
    /// Asserted on the **downcast**, not on the classification: which error codes
    /// mean corruption is `DbError::fault`'s business and is tested against a
    /// really-corrupted page in `veld-core`. The property at risk here is purely
    /// whether anyhow still hands back the concrete type, and that needs no
    /// SQLite failure to demonstrate (`rusqlite` is not a dependency of this
    /// crate, which is why this does not build one).
    #[test]
    fn a_context_wrapped_database_error_is_still_reachable() {
        use anyhow::Context;

        let wrapped = Err::<(), _>(DbError::AliasTaken("main".into()))
            .context("scanning health")
            .context("one more layer")
            .unwrap_err();

        assert!(
            wrapped.downcast_ref::<DbError>().is_some(),
            "anyhow must still find the DbError under its context layers — if this \
             fails, `note_reported` is silently doing nothing for the monitor and \
             the stats sampler, and the fix is to classify at the source instead"
        );
    }

    /// Repeat observations of the same fault count up without re-dating it —
    /// the 440-hits-since-06:19 shape the incident needed and did not have.
    #[test]
    fn repeat_observations_count_without_moving_first_seen() {
        let t0 = DateTime::parse_from_rfc3339("2026-08-26T06:19:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut fault = Fault::default();
        fault.observe(DbFault::Corrupt, "malformed".into(), t0);
        fault.observe(
            DbFault::Corrupt,
            "malformed".into(),
            t0 + Duration::hours(17),
        );

        assert_eq!(fault.hits, 2);
        assert_eq!(fault.first_seen, Some(t0));
        assert_eq!(fault.last_seen, Some(t0 + Duration::hours(17)));
    }

    /// A different kind starts over, so the reported `first_seen` dates the
    /// fault being reported. An I/O window that ends in corruption is two
    /// faults, and the corruption did not start when the I/O errors did.
    #[test]
    fn a_new_kind_supersedes_the_old_one() {
        let t0 = DateTime::parse_from_rfc3339("2026-08-26T06:19:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let later = t0 + Duration::hours(1);
        let mut fault = Fault::default();
        fault.observe(DbFault::Io, "disk I/O error".into(), t0);
        fault.observe(DbFault::Corrupt, "malformed".into(), later);

        assert_eq!(fault.kind, Some(DbFault::Corrupt));
        assert_eq!(fault.first_seen, Some(later), "dated from the corruption");
        assert_eq!(fault.hits, 1, "not carried over from the I/O window");
    }
}
