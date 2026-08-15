//! The timer that keeps `veld.db` backed up.
//!
//! Lives in the daemon for the same reason the GC does: it is the one process that
//! is always running, already owns a lifecycle, and is not tied to anybody's
//! terminal. The mechanism itself is [`veld_core::db::backup`] — this module is the
//! schedule, the settings read, and the record of what happened.
//!
//! **The tick is short and the interval is a setting, so the two are not the same
//! number.** The loop wakes on [`TICK_SECS`] and asks the database whether the
//! configured interval has elapsed, rather than building a `tokio::time::interval`
//! out of the setting — because a setting changed from 24 hours to 5 minutes must
//! take effect now, not after the 24-hour sleep the daemon already committed to.
//! That claim is made through [`Db::kv_try_claim_interval`], which is atomic, so a
//! second veld process doing the same thing cannot double up.

use std::path::PathBuf;

use veld_core::db::{Db, backup};

/// How often the loop wakes to ask whether a backup is due. Deliberately much
/// shorter than any interval a user can configure — see the module docs.
const TICK_SECS: u64 = 60;

/// Everything one due backup needs, decided from the database and nothing else.
///
/// **The decision is split from the effect on purpose.** `run_once` has to call
/// `Db::open()`, which resolves to the installed user's database — so a test of it
/// would be a test that writes production state, which this repo forbids
/// (`AGENTS.md` → never let a dev build touch the production database). [`plan`]
/// takes a `&Db` a test can hand it instead.
///
/// [`plan`] returns `None` for six reasons — backups switched off, a database that
/// knows about nothing, no directory could be derived, no database path, the
/// interval has not elapsed, and the claim itself failing — and `Some` otherwise.
/// The tests below cover the first, the second, the fifth and the `Some`, plus the
/// freshness of the settings read. The two that are **not** covered are the ones
/// with no reachable state to build in a test: a machine with no data directory at
/// all, and a failing `kv` write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPlan {
    pub source: PathBuf,
    pub dir: PathBuf,
    pub retention: backup::Retention,
}

/// Run the backup scheduler. Loops forever.
pub async fn run_backup_scheduler() {
    let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(TICK_SECS));
    // A laptop that slept for six hours owes one backup, not three hundred and
    // sixty ticks of them — same reasoning as the GC scheduler's.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tick.tick().await;
        // Blocking file and SQLite work, off the runtime's worker threads: a backup
        // holds a read transaction and writes a few megabytes, and a runtime thread
        // parked in `fsync` is a request nobody is answering.
        let outcome = tokio::task::spawn_blocking(run_once).await;
        if let Err(e) = outcome {
            tracing::warn!("backup task panicked: {e}");
        }
    }
}

/// One scheduler tick: take a backup if one is due.
///
/// Separated from the loop so it is callable — and testable — without a timer.
fn run_once() {
    // Opened per pass, like the GC's, so the daemon self-heals across a CLI upgrade
    // that migrated the schema underneath it.
    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => {
            // The one error worth being loud about: if the database cannot be
            // opened, the backups already on disk are the whole remaining story.
            tracing::warn!("backup skipped — cannot open the database: {e}");
            return;
        }
    };

    let Some(plan) = plan(&db) else {
        return;
    };

    let started = std::time::Instant::now();
    match backup::create(
        &plan.source,
        &plan.dir,
        Some(plan.retention),
        chrono::Utc::now(),
    ) {
        Ok(report) => {
            tracing::info!(
                "backup written: {} ({} KB, {} table(s), {} pruned) in {:?}",
                report.path.display(),
                report.bytes / 1024,
                report.rows.len(),
                report.pruned.len(),
                started.elapsed(),
            );
            // Retention is the only thing bounding this directory, so a delete that
            // failed is a disk filling up quietly. Warned rather than counted into
            // the success line, where it would read as normal.
            if !report.kept_unreadable.is_empty() {
                tracing::warn!(
                    "backup retention left {} unreadable artifact(s) in {} alone rather \
                     than destroy a damaged copy that might still be recoverable — they \
                     will not be cleaned up on their own",
                    report.kept_unreadable.len(),
                    plan.dir.display(),
                );
            }
            if !report.prune_failed.is_empty() {
                tracing::warn!(
                    "backup retention could not delete {} old artifact(s) in {} — the \
                     directory will keep growing until it can",
                    report.prune_failed.len(),
                    plan.dir.display(),
                );
            }
            // The mode veld asks for is not always the mode it gets: a FAT or SMB
            // volume cannot express one, and an artifact carries the relay tokens
            // and sensitive node outputs the live database is 0600 for.
            if !report.owner_only {
                tracing::warn!(
                    "backup {} is readable by more than its owner — {} cannot express \
                     file permissions, and a backup carries the same secrets the \
                     database does",
                    report.path.display(),
                    plan.dir.display(),
                );
            }
            record(&db, serde_json::json!({ "ok": true, "path": report.path }));
        }
        Err(e) => {
            // Loud, and recorded: a backup subsystem that fails silently is
            // indistinguishable from one that is working, right up until somebody
            // needs it. `veld doctor` reads this back.
            tracing::warn!("backup failed: {e}");
            record(
                &db,
                serde_json::json!({ "ok": false, "error": e.to_string() }),
            );
        }
    }
}

/// Decide whether a backup is due, and with what. `None` means "not now".
///
/// **Claiming the interval is part of the decision, not a side effect of it.** The
/// claim is a write, and it is what makes this at-most-once-per-interval across
/// every process — so a caller cannot ask "is it due?" without also taking the
/// slot, which is the only shape that does not race.
pub fn plan(db: &Db) -> Option<BackupPlan> {
    let prefs = db.backup_prefs();
    if !prefs.enabled {
        return None;
    }
    // **Never back up a database that knows about nothing.**
    //
    // `Db::open()` creates and migrates, so on a machine where `veld.db` is *absent*
    // rather than corrupt — removed, lost to a disk repair, a partial restore, a
    // restored home directory — an empty one is minted and then backed up. That
    // artifact carries a valid provenance row, passes every check, wins
    // `newest_usable`, and has `veld doctor` report "Database backup OK … newest 0
    // minute(s) old": the exact second failure of the incident this feature was
    // filed against, manufactured by the feature itself. Worse, the lost database
    // took `backup.dir` with it, so the empties land in the derived default folder
    // while the real copies sit elsewhere, unmentioned.
    //
    // **Checking that the file existed first is not enough**, which is what the
    // first version of this guard did: `monitor.rs` and `stats.rs` open the database
    // on a 5-second timer, so a deleted one is re-minted long before this tick comes
    // round a minute later. The question has to be about *content* — and about
    // **every** table's content, not a chosen few, which is what the second version
    // of this guard got wrong: `projects` is derived from run rows the GC deletes
    // after seven days, and `repos` is only ever written by the Desktop worktree
    // registry, so a CLI-only user who had not started a run for a week was refused
    // a backup while their settings, relay tokens and lanes were all still there.
    // See `Db::holds_user_state`.
    //
    // Because the backup is skipped, `prune` does not run either, so the real copies
    // from before a loss are not aged out from underneath the user while they sort
    // it out. A read error means "carry on": failing to answer this question is not
    // a reason to stop backing somebody up.
    if !db.holds_user_state().unwrap_or(true) {
        tracing::debug!("backup skipped — the database holds nothing yet");
        return None;
    }
    let Some(dir) = prefs.dir.clone() else {
        tracing::warn!("backup skipped — no backup directory could be determined");
        return None;
    };
    let source = Db::default_path().ok()?;

    let interval = std::time::Duration::from_secs(prefs.interval_minutes.max(1) as u64 * 60);
    // Atomic: the stamp is claimed and bumped in one transaction, so a second daemon
    // racing this tick loses cleanly instead of both writing.
    match db.kv_try_claim_interval(backup::LAST_RUN_KEY, interval) {
        Ok(false) => return None,
        Ok(true) => {}
        Err(e) => {
            tracing::warn!("backup skipped — could not claim the interval: {e}");
            return None;
        }
    }

    Some(BackupPlan {
        source,
        dir,
        retention: backup::Retention {
            keep: prefs.keep,
            keep_daily: prefs.keep_daily,
        },
    })
}

/// Record the outcome of the last attempt, at the stamp the *next* one is measured
/// from.
///
/// Written on failure too, and that is the point: the interval is measured from the
/// last attempt rather than the last success, so a backup that fails every time
/// fails on its own schedule instead of retrying every minute forever — and
/// `veld doctor` has something to show other than silence.
fn record(db: &Db, outcome: serde_json::Value) {
    if let Err(e) = db.kv_set(backup::LAST_RUN_KEY, &outcome.to_string()) {
        tracing::debug!("could not record the backup outcome: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// A database with something in it.
    ///
    /// The repo row is not decoration: `plan` refuses a database that knows about
    /// no code at all, so a genuinely empty one plans nothing and every test below
    /// would be asserting the wrong branch.
    fn test_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
        db.upsert_repo(&dir.path().join("repo"), "repo").unwrap();
        (dir, db)
    }

    fn patch(pairs: &[(&str, Value)]) -> std::collections::BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    /// The switch is a switch: off means no claim is taken either, so turning
    /// backups back on does not have to wait out an interval nothing used.
    #[test]
    fn a_disabled_backup_plans_nothing_and_claims_nothing() {
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("backup.enabled", Value::from(false))]))
            .unwrap();

        assert_eq!(plan(&db), None);
        assert_eq!(db.kv_get(backup::LAST_RUN_KEY).unwrap(), None);
    }

    /// A database that knows about nothing is not backed up.
    ///
    /// `Db::open()` creates and migrates, and `monitor.rs` and `stats.rs` call it on
    /// a 5-second timer — so on a machine whose `veld.db` was deleted, an empty one
    /// is back long before this tick. Backing *that* up produces an artifact that
    /// passes every check, wins `newest_usable` and turns the `veld doctor` row
    /// green: the incident's own second failure, manufactured by the feature meant
    /// to prevent it. Checking the file merely *exists* — which is what the first
    /// version of this guard did — cannot see that.
    #[test]
    fn a_database_that_knows_about_nothing_is_not_backed_up() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();

        assert_eq!(
            plan(&db),
            None,
            "a freshly minted database has nothing to lose"
        );
        assert_eq!(
            db.kv_get(backup::LAST_RUN_KEY).unwrap(),
            None,
            "and no interval was claimed, so a real backup is due the moment there is one"
        );

        // **A changed setting is enough**, and this is the half that matters. The
        // guard's second version asked only about projects and repositories: a
        // CLI-only user has neither once the GC has aged out their last run, and
        // `repos` is written only by the Desktop worktree registry — so that version
        // silently stopped backing up exactly the person this feature is for, whose
        // own loss in the filed incident was settings rows.
        db.patch_settings(&patch(&[("terminal.fontSize", Value::from(15))]))
            .unwrap();
        assert!(
            plan(&db).is_some(),
            "one changed setting is state a person would miss"
        );
    }

    /// Due once, then not again until the interval has passed — the property the
    /// 60s tick depends on, since it asks this question every minute.
    #[test]
    fn a_plan_is_taken_once_per_interval() {
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("backup.intervalMinutes", Value::from(60))]))
            .unwrap();

        let first = plan(&db).expect("the first tick after a boot is always due");
        assert_eq!(first.retention.keep, veld_core::db::DEFAULT_BACKUP_KEEP);
        assert_eq!(
            first.retention.keep_daily,
            veld_core::db::DEFAULT_BACKUP_KEEP_DAILY
        );
        assert_eq!(plan(&db), None, "the second tick is inside the interval");
    }

    /// Every tick reads the settings afresh, so a change made while the daemon is
    /// running is honoured without restarting it.
    ///
    /// This is the reason the loop wakes on a fixed 60s tick and asks the database,
    /// rather than building a `tokio::time::interval` out of `backup.intervalMinutes`
    /// — a timer built once at boot would sleep out the *old* interval before
    /// noticing a new one, which for the 24-hour end of the range means a day.
    ///
    /// Asserted on the retention numbers rather than on the interval: the interval's
    /// own effect is only observable by letting wall-clock time pass, while both
    /// come from the same per-tick `backup_prefs()` read, so a plan carrying a
    /// stale `keep` and one carrying a stale interval are the same defect.
    #[test]
    fn every_tick_reads_the_settings_again() {
        let (_dir, db) = test_db();
        db.patch_settings(&patch(&[("backup.keep", Value::from(3))]))
            .unwrap();
        assert_eq!(plan(&db).unwrap().retention.keep, 3);

        // The claim above blocks the next tick, so clear it the way a lapsed
        // interval would and change the setting underneath.
        db.kv_delete(backup::LAST_RUN_KEY).unwrap();
        db.patch_settings(&patch(&[("backup.keep", Value::from(30))]))
            .unwrap();
        assert_eq!(plan(&db).unwrap().retention.keep, 30);
    }

    /// A failure is recorded at the stamp the next attempt is measured from, so a
    /// backup that fails every time fails on its own schedule rather than retrying
    /// every minute — and `veld doctor` has something to show.
    #[test]
    fn a_failure_is_recorded_where_doctor_reads_it() {
        let (_dir, db) = test_db();
        record(
            &db,
            serde_json::json!({ "ok": false, "error": "disk full" }),
        );
        let raw = db.kv_get(backup::LAST_RUN_KEY).unwrap().unwrap();
        let recorded: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(recorded["ok"], Value::Bool(false));
        assert_eq!(recorded["error"], "disk full");
    }
}
