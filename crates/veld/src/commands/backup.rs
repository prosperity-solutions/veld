//! `veld backup` — see the copies, take one, put one back.
//!
//! **Everything here has to work on a machine whose database does not open**, which
//! is the only day this command matters. So nothing in this file requires
//! `Db::open()` to succeed: the backup directory falls back to the derived default
//! ([`backup::default_dir`]) when the settings cannot be read, and `restore` never
//! opens the live database at all.
//!
//! Restore is deliberately its own verb rather than a flag on `veld doctor`. Doctor
//! is a read-only diagnostic somebody runs when they are already worried, and a
//! destructive flag on it is a different kind of command wearing the same name.
//! Doctor's job here is to *report* the backups and name this command, which it
//! does.

use std::path::PathBuf;

use veld_core::db::{Db, backup};

use crate::output;

/// Where to look for artifacts, and whether the answer came from the user's
/// settings or from the built-in default.
///
/// A database that will not open is exactly the case this command exists for, so a
/// failed `Db::open()` is a fallback, not an error. The cost is real and worth
/// stating: somebody who pointed `backup.dir` at an external disk and then lost
/// their database is shown the *default* directory, so the caller says which one it
/// used.
fn resolve_dir(explicit: Option<PathBuf>) -> Option<(PathBuf, &'static str)> {
    if let Some(dir) = explicit {
        return Some((dir, "--dir"));
    }
    if let Some(dir) = open_existing_db().and_then(|db| db.backup_prefs().dir) {
        return Some((dir, "backup.dir"));
    }
    backup::default_dir().map(|dir| (dir, "default"))
}

/// The database, but only if there already is one.
///
/// **`Db::open()` creates and migrates**, which is right for every other command
/// and wrong for this one: somebody whose database has just vanished would have
/// `veld backup` mint a fresh empty one as a side effect of asking what they can
/// restore, and the restore would then file that empty database away as the
/// evidence of what went wrong. So the existence check comes first, and a missing
/// database simply means the settings are unreadable and the defaults apply.
fn open_existing_db() -> Option<Db> {
    let path = Db::default_path().ok()?;
    if !path.exists() {
        return None;
    }
    Db::open().ok()
}

/// `veld backup` — list what is on disk, newest first.
pub fn list(dir: Option<PathBuf>, json: bool) -> i32 {
    let Some((dir, source)) = resolve_dir(dir) else {
        output::print_error("Could not determine a backup directory.", json);
        return 1;
    };
    let artifacts = backup::list(&dir);
    // Every candidate is checked, not just listed. A list of filenames answers
    // "what is there"; the question actually being asked is "what can I go back
    // to", and those differ precisely when it matters.
    let checked: Vec<(&backup::Artifact, Result<i64, backup::BackupError>)> = artifacts
        .iter()
        .map(|a| (a, backup::inspect(&a.path)))
        .collect();

    if json {
        let rows: Vec<serde_json::Value> = checked
            .iter()
            .map(|(a, verdict)| {
                serde_json::json!({
                    "path": a.path,
                    "takenAt": a.taken_at.to_rfc3339(),
                    "schemaVersion": a.schema_version,
                    "bytes": a.bytes,
                    "usable": verdict.is_ok(),
                    // The human table shows this as "ok, exposed"; an agent could
                    // not see it at all until it was here.
                    "ownerOnly": a.owner_only,
                    "problem": verdict.as_ref().err().map(|e| e.to_string()),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "directory": dir,
                "directorySource": source,
                "supportedSchemaVersion": Db::supported_schema_version(),
                "backups": rows,
            })
        );
        return 0;
    }

    println!("{} {}", output::bold("Backups in"), dir.display());
    // **Say when this is a guess.** The fallback exists for the case where the
    // database will not open — which is also the case where `backup.dir` cannot be
    // read, so somebody who pointed their copies at an external drive is being shown
    // the *default* folder with no hint that their real ones are somewhere else.
    if source == "default" {
        output::print_info(
            "  (your backup.dir setting could not be read, so this is the default \
             folder — if you pointed backups somewhere else, pass --dir)",
        );
    }
    if artifacts.is_empty() {
        println!();
        output::print_info(
            "  No backups yet. The daemon writes one on the interval in \
             `veld settings backup`; `veld backup now` takes one immediately.",
        );
        return 0;
    }
    println!();
    // The **state** is one word in the table and its reason is a line underneath.
    // `BackupError`'s message names the path, which is right for a one-line failure
    // and wrong in a column beside a FILE column: a rejected artifact stretched the
    // table past 200 characters and wrapped every other row into noise.
    let rows: Vec<Vec<String>> = checked
        .iter()
        .map(|(a, verdict)| {
            let state = match (verdict, a.owner_only) {
                (Err(_), _) => "unusable",
                (Ok(_), false) => "ok, exposed",
                (Ok(_), true) => "ok",
            };
            vec![
                a.taken_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                format!("v{}", a.schema_version),
                output::fmt_bytes(a.bytes),
                state.to_string(),
                a.path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    output::print_table(&["TAKEN", "SCHEMA", "SIZE", "STATE", "FILE"], &rows);

    for (a, verdict) in &checked {
        let name = a.path.file_name().unwrap_or_default().to_string_lossy();
        if let Err(e) = verdict {
            println!();
            output::print_info(&format!("  {name}: {e}"));
        } else if !a.owner_only {
            println!();
            output::print_info(&format!(
                "  {name}: readable by more than you. A backup carries the same secrets \
                 the database does, and this filesystem cannot express file permissions."
            ));
        }
    }

    println!();
    // Names the newest **usable** one, which is also what a no-path restore would
    // pick. Naming `artifacts[0]` instead offered the very file the table had just
    // called unusable — and a future-dated name sorts first, so that is not a rare
    // shape.
    // `newest_restorable`, not `newest_usable`: this line offers a command, so it has
    // to name a file that command will accept. The shallow sweep's pick can be one
    // `backup::restore` refuses.
    match backup::newest_restorable(&dir, chrono::Utc::now()) {
        Some(a) => output::print_info(&format!(
            "  Restore the newest usable one with `veld backup restore`, or that exact \
             one with `veld backup restore {}`.",
            a.path.display()
        )),
        None => output::print_info(
            "  None of these can be restored — see the reasons above. \
             `veld backup now` takes a fresh one if the database still opens.",
        ),
    }
    0
}

/// `veld backup now` — take one immediately, outside the daemon's schedule.
pub fn now(dir: Option<PathBuf>, json: bool) -> i32 {
    let Some((dir, _)) = resolve_dir(dir) else {
        output::print_error("Could not determine a backup directory.", json);
        return 1;
    };
    // Retention applies **only when the user's own numbers could be read.**
    //
    // The tempting fallback — prune with the defaults — is the dangerous one, and it
    // is dangerous precisely on the path that reaches it: the settings are
    // unreadable because the database is gone or broken, which is the moment
    // `veld doctor` tells somebody to run this. A user who set `backup.keep` to 200
    // would have up to 188 of their remaining artifacts deleted by the command they
    // were sent to for help. Not pruning costs an unbounded directory for somebody
    // looping this by hand; the scheduled backup prunes it back the next time the
    // settings can be read.
    let retention = open_existing_db().map(|db| {
        let prefs = db.backup_prefs();
        backup::Retention {
            keep: prefs.keep,
            keep_daily: prefs.keep_daily,
        }
    });
    let source = match Db::default_path() {
        Ok(p) => p,
        Err(e) => {
            output::print_error(&format!("Could not locate the database: {e}"), json);
            return 1;
        }
    };
    if !source.exists() {
        output::print_error(
            &format!("There is no database at {} to back up.", source.display()),
            json,
        );
        return 1;
    }

    match backup::create(&source, &dir, retention, chrono::Utc::now()) {
        Ok(report) => {
            if json {
                println!("{}", serde_json::to_string(&report).unwrap_or_default());
            } else {
                output::print_success(&format!(
                    "Backed up to {} ({})",
                    report.path.display(),
                    output::fmt_bytes(report.bytes)
                ));
                if !report.pruned.is_empty() {
                    output::print_info(&format!(
                        "  {} older backup(s) removed by the retention policy.",
                        report.pruned.len()
                    ));
                }
                if report.retention_skipped {
                    output::print_info(
                        "  Retention was skipped: your backup settings could not be read, \
                         and pruning to defaults could delete copies you configured to keep.",
                    );
                }
                // **Warnings, not errors, because the backup succeeded.** These were
                // printed with the red cross `print_error` draws while the command
                // still exited 0 — a failure to look at and a success to a script,
                // which is the shape this repo's "verify by exit code" rule exists
                // to refuse. The backup *was* written; what these say is that
                // something around it needs attention, and `--json`'s
                // `pruneFailed` / `ownerOnly` are how a caller acts on that without
                // reading prose.
                if !report.kept_unreadable.is_empty() {
                    eprintln!(
                        "{} {} backup(s) here cannot be read, so retention left them \
                         alone rather than risk destroying a damaged copy that might \
                         still be recoverable. They will not be cleaned up on their own.",
                        output::yellow("Warning:"),
                        report.kept_unreadable.len()
                    );
                }
                if !report.prune_failed.is_empty() {
                    eprintln!(
                        "{} {} old backup(s) could not be deleted — retention is the only \
                         thing bounding this folder's size.",
                        output::yellow("Warning:"),
                        report.prune_failed.len()
                    );
                }
                if !report.owner_only {
                    eprintln!(
                        "{} {} is readable by more than you. A backup carries the same \
                         secrets the database does — relay tokens, node outputs — and this \
                         filesystem cannot express file permissions.",
                        output::yellow("Warning:"),
                        report.path.display()
                    );
                }
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Backup failed: {e}"), json);
            1
        }
    }
}

/// `veld backup restore` — put a backup back in place.
pub async fn restore(
    path: Option<PathBuf>,
    dir: Option<PathBuf>,
    force: bool,
    assume_yes: bool,
    json: bool,
) -> i32 {
    let target = match Db::default_path() {
        Ok(p) => p,
        Err(e) => {
            output::print_error(&format!("Could not locate the database: {e}"), json);
            return 1;
        }
    };

    // A daemon that is still up keeps writing through the descriptor it already
    // holds, so its work after this point lands in the file being replaced and is
    // lost. That is not corruption — the restored file is never written by the old
    // handle — but it is surprising enough to be worth refusing by default.
    if !force && daemon_is_up().await {
        // Names the command, because "stop it" is not an instruction anybody can
        // follow here: there is no `veld stop-daemon`, the daemon is a
        // service-manager job, and the person reading this arrived from
        // `veld doctor` with a broken install.
        // Names *this* database's daemon. On a dev instance that is a node of a
        // veld run, not the installed launchd job — see `stop_hint`.
        let whose = if Db::uses_installed_database() {
            ""
        } else {
            " (the run that owns this database)"
        };
        output::print_error(
            &format!(
                "The veld daemon is running. Its work would go to the database being \
                 replaced. Stop it with `{}`{whose}, or pass --force and restart it \
                 afterwards with `{}`.",
                stop_hint(),
                restart_hint()
            ),
            json,
        );
        return 1;
    }

    let chosen = match path {
        Some(p) => {
            // The **deep** check, the same one `backup::restore` gates on. This
            // pre-check exists to refuse before the destructive confirm prompt, and
            // a shallower one here meant a user was asked to confirm, answered yes,
            // and only then told the artifact could not be restored.
            if let Err(e) = backup::inspect_deep(&p) {
                output::print_error(&format!("{} cannot be restored: {e}", p.display()), json);
                return 1;
            }
            p
        }
        None => {
            let Some((dir, _)) = resolve_dir(dir) else {
                output::print_error("Could not determine a backup directory.", json);
                return 1;
            };
            let artifacts = backup::list(&dir);
            if artifacts.is_empty() {
                output::print_error(&format!("No backups found in {}.", dir.display()), json);
                return 1;
            }
            // The newest one that is actually restorable, not simply the newest. On
            // the day this runs, the newest file may be the one that copied the
            // damage — or one somebody else dropped in a shared folder under a name
            // claiming next year. `newest_restorable` decides both, and it is the
            // *deep*-checked pick on purpose: choosing on the cheap check that the
            // listing uses would let this pick a file `backup::restore` then refuses,
            // with nothing to fall through to.
            let Some(usable) = backup::newest_restorable(&dir, chrono::Utc::now()) else {
                output::print_error(
                    &format!(
                        "None of the {} backup(s) in {} can be restored — see \
                         `veld backup` for why.",
                        artifacts.len(),
                        dir.display()
                    ),
                    json,
                );
                return 1;
            };
            usable.path.clone()
        }
    };

    // **`--json` is an output format, not consent**, and neither is the absence of a
    // terminal. The first version read `!assume_yes && !json && is_tty()`, which
    // made `veld backup restore --json` replace the live database silently and made
    // any piped invocation do the same — a destructive default nothing announced,
    // in a command whose whole subject is a file you cannot regenerate. `-y` exists
    // to say yes; without a terminal to ask, the answer is no.
    if !assume_yes {
        if !attended() {
            output::print_error(
                "Restoring replaces the live database. There is no terminal to confirm \
                 on — pass -y to say so explicitly.",
                json,
            );
            return 1;
        }
        use std::io::{BufRead, Write};
        eprintln!(
            "{} This replaces {} with {}.",
            output::yellow("Warning:"),
            target.display(),
            chosen.display()
        );
        eprintln!("  The database that is there now is kept, renamed, not deleted.");
        eprint!("Continue? [y/N] ");
        std::io::stderr().flush().ok();
        let line = match std::io::stdin().lock().lines().next() {
            Some(Ok(l)) => l,
            _ => return 1,
        };
        if !matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
            output::print_info("Cancelled.");
            return 1;
        }
    }

    match backup::restore(&chosen, &target) {
        Ok(report) => {
            if json {
                // The restart instruction is part of the *result*, not chrome. An
                // agent driving this surface has no other signal that the daemon is
                // now holding a file that no longer exists — and with `--force`,
                // still writing to it.
                let mut value = serde_json::to_value(&report).unwrap_or_default();
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("restartDaemon".into(), restart_hint().into());
                }
                println!("{value}");
            } else {
                output::print_success(&format!(
                    "Restored {} to {} (schema v{})",
                    report.restored_from.display(),
                    report.restored_to.display(),
                    report.schema_version
                ));
                if let Some(old) = &report.previous_moved_to {
                    output::print_info(&format!(
                        "  The database that was there is kept at {} — it is the only \
                         evidence of what went wrong, so veld never deletes it.",
                        old.display()
                    ));
                }
                output::print_info(&format!("  Restart the daemon: {}", restart_hint()));
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Restore failed: {e}"), json);
            1
        }
    }
}

/// Whether a daemon that **owns the database about to be replaced** is running.
///
/// The ownership half is the part that is easy to get wrong, and it has been wrong
/// in both directions. `VELD_DB_PATH` and `VELD_DAEMON_SOCK` are independent
/// overrides, so "some daemon is up" is not the question:
///
/// - The **installed** database is owned by the daemon on the **default** socket.
///   That is true whatever socket *this process* was pointed at, which is why this
///   asks the default socket rather than `daemon_socket()`. Reading the instance
///   socket here would let `VELD_DAEMON_SOCK=… veld backup restore` skip the check
///   entirely while the installed daemon carried on writing the file being
///   replaced.
/// - A **non-installed** database (a `VELD_DB_PATH` override, a cargo build's own)
///   is owned by whichever daemon was handed the same override, and the only case
///   where that is knowable is a matching `VELD_DAEMON_SOCK` — the dev stack's
///   pairing. With no socket override there is no daemon this process can name, and
///   refusing on the installed daemon's socket would block a restore of a scratch
///   database for a daemon that has never opened it.
///
/// The probe itself is the one every other "is it alive" question in veld uses —
/// connect, and close again immediately. A socket file that exists but refuses a
/// connection is a leftover, not a daemon.
async fn daemon_is_up() -> bool {
    let sock = if Db::uses_installed_database() {
        veld_core::instance::default_daemon_socket()
    } else if veld_core::instance::daemon_socket_is_default() {
        return false;
    } else {
        veld_core::instance::daemon_socket()
    };
    sock.exists() && tokio::net::UnixStream::connect(&sock).await.is_ok()
}

/// Whether a human is there to answer the confirmation prompt.
///
/// **Gated on the two streams this prompt actually uses, and on neither of the ones
/// it does not** — the rule `veld start` already arrived at the hard way
/// (`commands/start.rs`, and its comment is worth reading before changing this).
/// Not `output::is_tty()`, which reads **stdout**: the question goes to stderr, so
/// `veld backup restore | tee log` from a real terminal would be declared
/// unattended and refused with a human sitting right there. Not stdin alone
/// either, which is the trap on the way back — a process launched from a terminal
/// can hand its tty to a child whose stderr is a log file nobody reads, and the
/// prompt would then block forever on a question nobody saw.
fn attended() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && std::env::var("VELD_NON_INTERACTIVE")
            .map(|v| v.is_empty())
            .unwrap_or(true)
}

/// How to bring the daemon back onto the restored file.
///
/// Printed rather than done: the installed daemon is a service-manager job, and a
/// CLI that booted it out and back in would be reimplementing `veld setup`'s
/// launchd choreography for one line of convenience.
///
/// **Which daemon this is depends on which database is being restored**, and
/// getting that wrong is worse than saying nothing. A dev instance's daemon is a
/// node of a veld *run* (`dev-daemon`), not a launchd job — so naming
/// `launchctl kickstart … dev.veld.daemon` there points at the **installed**
/// daemon, which does not own this database and whose restart would disturb the
/// environments the developer actually has running. Found by restoring against a
/// live `veld start --preset dev-headless` stack, which is the only place the two
/// differ.
fn restart_hint() -> String {
    if !Db::uses_installed_database() {
        return "veld start".to_string();
    }
    if cfg!(target_os = "macos") {
        "launchctl kickstart -k gui/$(id -u)/dev.veld.daemon".to_string()
    } else {
        "systemctl --user restart veld-daemon".to_string()
    }
}

/// How to stop the daemon before a restore. See [`restart_hint`] for why this is
/// not one fixed string.
///
/// On macOS the installed job carries `KeepAlive` **and** a `WatchPaths` on its
/// binary, so `bootout` is what actually stops it — a plain kill is relaunched
/// within seconds, which would look like the refusal was wrong.
fn stop_hint() -> String {
    if !Db::uses_installed_database() {
        return "veld stop".to_string();
    }
    if cfg!(target_os = "macos") {
        "launchctl bootout gui/$(id -u)/dev.veld.daemon".to_string()
    } else {
        "systemctl --user stop veld-daemon".to_string()
    }
}
