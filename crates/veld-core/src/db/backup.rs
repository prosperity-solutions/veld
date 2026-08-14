//! Periodic, self-managing copies of the central database.
//!
//! Every piece of Veld's state lives in one file. Losing it is not a degraded
//! experience, it is a fresh install: every registered repo gone, every worktree
//! unknown to the IDE, every lane, layout, marker and setting gone. It has
//! happened — a file that was 548,941,824 bytes and `integrity_check`-clean at
//! 11:05 was 8192 bytes shorter three minutes later with page 1 simply missing,
//! `sqlite3 .recover` returned nothing, and the only surviving copies were
//! leftovers a migration had taken weeks earlier at `user_version 9` against a
//! head of 16, with **zero settings rows**. Note which half of that hurt: the
//! copies opened fine. They just had nothing in them.
//!
//! # What an artifact is
//!
//! A **real `veld.db`**, at this binary's head schema, with every table present
//! and the bulk tables ([`EXCLUDED_TABLES`]) empty. Restoring is therefore a file
//! move, not an import, and a restored database is opened and migrated by the
//! ordinary path with no special case anywhere.
//!
//! It is built by creating a fresh database through the normal migration path
//! ([`Db::open_at`]), `ATTACH`ing it to a **dedicated** connection to the live
//! file, and copying table by table inside one `BEGIN DEFERRED`. Four properties
//! fall out of that, and each is the reason a more obvious mechanism was not used:
//!
//! - **Consistency.** In WAL mode a deferred transaction takes its read mark at
//!   the *first read* and holds it against every writer in every process until the
//!   commit, so all the tables are read at one instant even though the copy spans
//!   many statements. That the attached destination takes a read mark of its own is
//!   irrelevant: nothing else writes to it.
//! - **Cost.** `VACUUM INTO` is one statement and would be lovely, except it copies
//!   everything: on the machine above that is 523 MB per run to keep 4 MB of it,
//!   several times an hour, while holding a read transaction that pins the WAL for
//!   the duration. No statement here ever touches `log_lines`.
//! - **No serializer.** A logical dump (JSON, SQL text) avoids the size problem too
//!   and costs a hand-written encoder for SQLite's type system — BLOBs, `REAL`
//!   round-tripping, `NULL` versus `''`, `INTEGER PRIMARY KEY` rowid aliasing. Every
//!   corner of that is a wrong-data bug discovered during the one event this exists
//!   for. `INSERT INTO … SELECT` has SQLite do the encoding.
//! - **New tables are backed up by default.** Tables are enumerated from
//!   `sqlite_master` minus a denylist, so a table a future migration adds is copied
//!   without anyone remembering to say so. The failure direction is *inclusion* — a
//!   new high-volume table would make artifacts big, which is visible in
//!   `veld backup list`'s size column — never silent omission.
//!
//! # Verification is semantic, not structural
//!
//! `integrity_check` would have passed the stale copies that survived the real
//! incident. So [`create`] also re-counts every table it copied, in the finished
//! artifact, and fails the backup if a count disagrees. An artifact that opens and
//! is empty is the failure this feature exists to prevent, not a corner case.
//!
//! # Never delete the evidence
//!
//! [`restore`] moves the existing database aside rather than overwriting it — it is
//! the only file a user cannot regenerate and the only evidence of what went wrong.
//! Its `-wal` and `-shm` move with it, which is not tidiness: a stale WAL left
//! beside a restored database is replayed into it on the next open.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;
use thiserror::Error;

use super::{Db, DbError};

/// Tables deliberately left empty in an artifact.
///
/// All four are **high-volume and regenerable**, and all four are already pruned on
/// an age horizon by the daemon's GC. They are the entire difference between a
/// 523 MB file and a 4 MB one, and nobody has ever wanted a backup *for* them:
/// restoring last Tuesday's resource samples is not a recovery, it is noise.
///
/// The list is a **denylist on purpose** — see the module docs. Adding a table to
/// the schema and forgetting this file means the table is backed up, which is the
/// safe direction to be wrong in.
///
/// Every name here is the **head-schema** name, which is not always the name the
/// table was created under: migration v6 built `node_stats_v3` and renamed it over
/// the original, so `node_stats_v3` written here would match nothing and every
/// artifact would silently start carrying the samples.
/// `every_excluded_table_still_exists` is what catches that.
pub const EXCLUDED_TABLES: &[&str] = &[
    "log_lines",
    "node_stats",
    "node_process_stats",
    "feedback_screenshots",
];

/// The `kv` key an artifact carries its own provenance under.
///
/// Written **into the artifact**, not into a sidecar file, so a copy that has been
/// moved, renamed or emailed still says when it was taken, by which version, from
/// where, and how many rows of each table it holds. A sidecar is a second file to
/// lose.
pub const META_KEY: &str = "backup.meta";

/// The `kv` key the daemon records its last backup attempt under, in the **live**
/// database.
///
/// State, not a preference — which is why it is here and not a `backup.lastAt`
/// entry in the settings catalog. The catalog is a projection of things a user
/// *sets*, and a read-only pseudo-setting would be a key `veld settings set`
/// accepts and then cannot honour.
pub const LAST_RUN_KEY: &str = "backup.lastRun";

/// Filename prefix for an artifact. Nothing without it is ever deleted by
/// [`prune`] — see there.
const NAME_PREFIX: &str = "veld-";
const NAME_SUFFIX: &str = ".db";

/// Prefix for the partially-written file, chosen so it can never match
/// [`parse_name`]: pruning must not see a backup that is still being written, and
/// a leading dot also keeps it out of a user's file listing.
const TEMP_PREFIX: &str = ".veld-backup-tmp-";

/// How long a half-written file is left alone before it is treated as abandoned.
///
/// Covers both shapes of litter a killed run leaves: the claimed-but-empty final
/// name, and the temp file the copy was going into. Generous enough that a backup
/// actually in progress — a few seconds even on a large database — is never
/// mistaken for one that died.
const PARTIAL_GRACE: chrono::Duration = chrono::Duration::minutes(30);

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("failed to create the backup directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to write the backup file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("database error while backing up: {0}")]
    Db(#[from] DbError),

    #[error("sqlite error while backing up: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("{path} is not a SQLite database")]
    NotADatabase { path: PathBuf },

    /// A readable database that veld did not write. See [`inspect`].
    #[error("{path} is a database but not a veld backup — it carries no veld provenance")]
    NotAVeldBackup { path: PathBuf },

    #[error(
        "the database at {path} is schema v{found} but this binary's is v{head} — it was \
         migrated by a newer veld, so backing it up here would silently drop whatever that \
         version added. Run `veld update` first"
    )]
    SourceSchema {
        path: PathBuf,
        found: i64,
        head: i64,
    },

    #[error(
        "{path} was written by a newer veld (schema v{found}; this binary supports up to \
         v{supported}) — run `veld update` first"
    )]
    NewerSchema {
        path: PathBuf,
        found: i64,
        supported: i64,
    },

    /// The finished artifact did not hold what was just written into it. Fails the
    /// backup rather than shipping it — see the module docs on semantic
    /// verification.
    #[error("the backup written to {path} did not verify: {reason}")]
    Unverified { path: PathBuf, reason: String },
}

/// One artifact on disk, as [`list`] reports it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub path: PathBuf,
    pub taken_at: chrono::DateTime<chrono::Utc>,
    /// The schema version in the file's header, read without opening it.
    pub schema_version: i64,
    pub bytes: u64,
    /// Whether the file's mode grants nothing to group or other. `false` means
    /// the copy — and the relay tokens in it — is readable by anyone who can
    /// reach the volume, which is the normal state on a FAT or SMB drive.
    pub owner_only: bool,
}

/// What one [`create`] did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupReport {
    pub path: PathBuf,
    pub bytes: u64,
    pub schema_version: i64,
    pub taken_at: chrono::DateTime<chrono::Utc>,
    /// Rows copied, per table, for the tables that had any.
    pub rows: std::collections::BTreeMap<String, i64>,
    /// Artifacts deleted by the retention pass that followed.
    pub pruned: Vec<PathBuf>,
    /// Artifacts retention wanted to delete and could not.
    ///
    /// Reported rather than swallowed because retention is the **only** thing
    /// bounding disk here: a directory that has gone read-only, or a leftover
    /// owned by another user, otherwise accumulates forever while every backup
    /// reports success.
    pub prune_failed: Vec<PathBuf>,
    /// Artifacts retention deliberately left alone because it could not read them
    /// — see [`prune`]. Never deleted, and for the same reason never counted as
    /// bounded: these accumulate, and somebody has to be told.
    pub kept_unreadable: Vec<PathBuf>,
    /// Whether retention was skipped entirely because the caller had no numbers
    /// to apply.
    ///
    /// **Reported, because an empty `pruned` cannot say it.** "Retention ran and
    /// found nothing to delete" and "retention never ran" are the same JSON
    /// otherwise, and they are opposite facts to an agent — the second means the
    /// directory is unbounded until the settings can be read again.
    pub retention_skipped: bool,
    /// Whether the finished artifact is owner-readable only — see
    /// [`is_owner_only`].
    pub owner_only: bool,
}

/// What one [`restore`] did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub restored_from: PathBuf,
    pub restored_to: PathBuf,
    pub schema_version: i64,
    /// Where the database that was there before was moved to, if there was one.
    /// **Never deleted** — it is the only evidence of what went wrong.
    pub previous_moved_to: Option<PathBuf>,
}

/// Where backups go when `backup.dir` is empty.
///
/// `<data_dir>/veld-backups` — a **sibling** of the `veld` data directory rather
/// than a child of it, which is the whole point. It is standard OS territory on
/// both platforms (`~/Library/Application Support/veld-backups`,
/// `~/.local/share/veld-backups`), it is inside Time Machine's default inclusion
/// set and inside any home-directory backup, it is not `~/.veld` (sockets and PTY
/// directories, all of it meant to be recreated), and it is not the directory
/// holding `veld.db` — so an `rm -rf` of Veld's data directory does not take the
/// copies with it.
///
/// **Say the honest thing about this, because the setting depends on it:** no local
/// default survives the volume dying. What this default buys is surviving the file
/// going bad and the app directory being wiped, plus being findable by a person who
/// needs it. `backup.dir` pointed at an external disk or a synced folder is the only
/// real answer to off-volume, which is why that setting exists.
///
/// **A cargo-built binary gets a directory beside its own dev database instead**,
/// by exactly the reasoning in [`Db::cargo_target_db`]: a `cargo test` that wrote
/// into the installed user's backup directory would pollute the copies they would
/// restore from, and the whole point of that guard is that no dev build touches
/// production state.
pub fn default_dir() -> Option<PathBuf> {
    if Db::uses_installed_database() {
        dirs::data_dir().map(|d| d.join("veld-backups"))
    } else {
        Db::default_path().ok()?.parent().map(|p| p.join("backups"))
    }
}

/// The name an artifact taken at `at`, from a database at `schema_version`, gets.
///
/// Sortable, unambiguous, and self-describing on a machine where nothing works:
/// `ls` alone answers "how old is the newest one, and what schema is it".
fn artifact_name(at: chrono::DateTime<chrono::Utc>, schema_version: i64) -> String {
    format!(
        "{NAME_PREFIX}{}-v{schema_version}{NAME_SUFFIX}",
        at.format("%Y%m%dT%H%M%SZ")
    )
}

/// The inverse of [`artifact_name`], or `None` for a name veld did not write.
///
/// **This is the guard that makes a user-supplied `backup.dir` safe.** Retention
/// deletes files, and the directory may be one the user shares with something else
/// — so a file veld cannot prove it wrote itself is never a candidate.
fn parse_name(name: &str) -> Option<(chrono::DateTime<chrono::Utc>, i64)> {
    let rest = name.strip_prefix(NAME_PREFIX)?.strip_suffix(NAME_SUFFIX)?;
    let (stamp, version) = rest.rsplit_once("-v")?;
    let at = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ").ok()?;
    let version: i64 = version.parse().ok()?;
    Some((at.and_utc(), version))
}

/// The `user_version` in a SQLite file's header, read **without opening it**.
///
/// Bytes 0..16 are the magic string and bytes 60..64 are `user_version`, big-endian
/// — a documented, stable part of the file format. Reading it this way is what lets
/// a restore be refused *before* the live database is touched, on a binary that
/// could not open the artifact at all. `Ok(None)` means the file is not a SQLite
/// database.
///
/// **It reads the main file only, so on a database in WAL mode with an
/// un-checkpointed migration pending it can be behind.** That is fine for what this
/// is used on — an artifact is written with a rollback journal and has no WAL, by
/// construction — and it is why [`restore`] re-reads the version from the opened
/// connection before acting, rather than trusting this alone.
pub fn header_schema_version(path: &Path) -> std::io::Result<Option<i64>> {
    use std::io::Read;
    let mut header = [0u8; 64];
    let mut file = std::fs::File::open(path)?;
    if file.read_exact(&mut header).is_err() {
        return Ok(None);
    }
    if &header[..16] != b"SQLite format 3\0" {
        return Ok(None);
    }
    Ok(Some(i64::from(u32::from_be_bytes([
        header[60], header[61], header[62], header[63],
    ]))))
}

/// Every artifact in `dir`, newest first. A missing directory is an empty list,
/// not an error — nothing has been backed up yet is a normal state.
pub fn list(dir: &Path) -> Vec<Artifact> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Artifact> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let (taken_at, named_version) = parse_name(entry.file_name().to_str()?)?;
            // `std::fs::metadata`, not the `DirEntry`'s: that one does **not** follow
            // a symlink, so it reports a link's own size and mode rather than the
            // artifact's, and excluded symlinks along with directories — quietly
            // hiding a backup somebody parked on another disk and linked in here from
            // listing, from the restore pick and from retention.
            //
            // **Regular files only, on the followed metadata.** Narrowing this to
            // "not a directory" was the first attempt and it opened a worse hole than
            // it closed: a FIFO named like an artifact — the name pattern is
            // documented and guessable — then reached `header_schema_version`, whose
            // `File::open` on a pipe with no writer **blocks forever**, wedging
            // `veld backup`, the daemon's retention tick and the restore pick alike.
            //
            // Deleting stays safe either way: `remove_file` removes a link, never its
            // target.
            let meta = std::fs::metadata(&path).ok()?;
            if !meta.is_file() {
                return None;
            }
            // The header wins over the name: the name is a label somebody could
            // have renamed, the header is the file. Falling back to the name keeps
            // a listing useful for a file that has become unreadable, which is
            // exactly when somebody is reading this list.
            let schema_version = header_schema_version(&path)
                .ok()
                .flatten()
                .unwrap_or(named_version);
            Some(Artifact {
                path,
                taken_at,
                schema_version,
                bytes: meta.len(),
                owner_only: is_owner_only(&meta),
            })
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.taken_at));
    out
}

/// Column names of `table` in the attached database `schema`, in declaration
/// order.
fn columns(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name FROM {}.pragma_table_info(?1)",
        quote_ident(schema)
    ))?;
    let rows = stmt.query_map([table], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Every ordinary table in the attached database `schema`.
///
/// `sqlite_%` is SQLite's own reserved prefix (`sqlite_sequence`, `sqlite_stat1`);
/// those are maintained by SQLite for the destination's own content and copying one
/// would describe the source's.
fn tables(conn: &Connection, schema: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name FROM {}.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
        quote_ident(schema)
    ))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// A SQL identifier, double-quoted with embedded quotes doubled.
///
/// Every name reaching this comes from our own `sqlite_master`, so this is not the
/// difference between safe and unsafe today — it is what keeps that true if a
/// future table is ever named after a keyword.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// What one retention pass did. See [`prune`].
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneReport {
    pub deleted: Vec<PathBuf>,
    pub failed: Vec<PathBuf>,
    pub kept_unreadable: Vec<PathBuf>,
}

/// How many artifacts survive a retention pass. See [`prune`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub keep: i64,
    pub keep_daily: i64,
}

/// Create a file that did not exist a moment ago, owner-readable only.
///
/// `create_new` is `O_CREAT|O_EXCL`, which fails on an existing file **and on a
/// symlink**, so this is what stops a planted symlink in a shared `backup.dir`
/// turning a write here into a write somewhere else. Plain `create(true)` — which
/// is what `Db::open_at` does, and therefore what this has to get in front of —
/// follows one.
#[cfg(unix)]
fn create_exclusive(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(|_| ())
}

#[cfg(not(unix))]
fn create_exclusive(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
}

/// Write one backup of the database at `source` into `dir`, then apply retention.
///
/// `retention` is `None` for "do not prune", which is not a convenience: a caller
/// that could not read the settings does not know the user's `keep`, and pruning to
/// a *default* would delete artifacts somebody deliberately configured to keep — at
/// the one moment they are the whole remaining story.
///
/// `now` is a parameter rather than read inside so a test can pin the naming and
/// the retention arithmetic.
pub fn create(
    source: &Path,
    dir: &Path,
    retention: Option<Retention>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BackupReport, BackupError> {
    // 0700 on the components **veld creates**, and nothing else. The obvious
    // shape — `create_dir_all` then `set_permissions` — chmods a directory the
    // user pointed at, every single run: point `backup.dir` at a shared or synced
    // folder and veld silently strips group access from it on the hour, undoing a
    // manual `chmod` within the interval, while this feature's own help text
    // promises the folder is "safe to share". It also follows a symlink, so the
    // target rather than the link gets it.
    #[cfg(unix)]
    let built = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    };
    #[cfg(not(unix))]
    let built = std::fs::DirBuilder::new().recursive(true).create(dir);
    built.map_err(|e| BackupError::CreateDir {
        path: dir.to_path_buf(),
        source: e,
    })?;

    // `ATTACH` takes a string, so a non-UTF-8 directory would be lossily converted
    // and SQLite would open a *different* path — writing a stray database
    // somewhere the user never named, while this run fails later with a confusing
    // "schema is v0". Refused up front instead. (`backup.dir` always arrives as
    // valid UTF-8 through JSON; `--dir` takes an `OsString`.)
    if dir.to_str().is_none() {
        return Err(BackupError::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "backup directory path is not valid UTF-8",
            ),
        });
    }

    // Sweep abandoned temp files before adding another. They are deliberately named
    // so `parse_name` cannot match them — which keeps retention from ever deleting a
    // backup in progress, and left nothing at all to clean them up: three interrupted
    // runs put 208 MB of orphans in a directory `veld backup` reported as 138 MB.
    // Retention is the only thing bounding this folder, so it has to reach these too.
    sweep_abandoned_temp_files(dir, now);

    // The name is decided **before** anything is written, because deciding it
    // afterwards means `rename` — which replaces silently on Unix — is what
    // discovers a collision, and by then the artifact it replaced is gone. Two
    // backups inside one second is not hypothetical: `veld backup now` run twice,
    // or a manual one landing in the same second as the scheduled one, both do it.
    //
    // And the name is **claimed**, not merely probed. `exists()` followed by a
    // rename is a check-then-act across two processes, and the two producers this
    // guards against are two processes: the daemon's tick claims an interval in the
    // database, `veld backup now` claims nothing. `create_exclusive` makes the
    // claim atomic, and the rename below then replaces veld's own placeholder.
    //
    // An artifact is always at head, because its schema comes from running the
    // whole migration list against an empty file — so the version is knowable here
    // and `write_artifact` asserts it stayed true.
    let version = Db::supported_schema_version();
    let mut at = now;
    let mut final_path = dir.join(artifact_name(at, version));
    // A second per step, and this is the only loop that can be entered by having
    // done a lot of backups very fast; two minutes of them is well past any real
    // case and the refusal below is better than an unbounded search.
    let mut claimed = false;
    for _ in 0..120 {
        match create_exclusive(&final_path) {
            Ok(()) => {
                claimed = true;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                at += chrono::Duration::seconds(1);
                final_path = dir.join(artifact_name(at, version));
            }
            Err(e) => {
                return Err(BackupError::Io {
                    path: final_path,
                    source: e,
                });
            }
        }
    }
    if !claimed {
        return Err(BackupError::Io {
            path: final_path,
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a backup already exists for every second in the next two minutes",
            ),
        });
    }

    // Unpredictable, and claimed the same exclusive way. A guessable temp name in a
    // directory somebody else can write is a symlink waiting to be planted: the
    // next thing that touches this path is `Db::open_at`, which opens with
    // `create(true)` and would follow one — turning a backup into a write over
    // whatever the link pointed at.
    let tmp = dir.join(format!(
        "{TEMP_PREFIX}{}{NAME_SUFFIX}",
        uuid::Uuid::new_v4().simple()
    ));
    if let Err(e) = create_exclusive(&tmp) {
        let _ = std::fs::remove_file(&final_path);
        return Err(BackupError::Io {
            path: tmp,
            source: e,
        });
    }

    let report = match write_artifact(source, &tmp, at) {
        Ok(report) => report,
        Err(e) => {
            cleanup_temp(&tmp);
            let _ = std::fs::remove_file(&final_path);
            return Err(e);
        }
    };
    // The name was chosen from `supported_schema_version()`; if the artifact came
    // out at anything else, the file would be labelled with a version it does not
    // have, which is the one lie a restore reads before it acts.
    if report.schema_version != version {
        cleanup_temp(&tmp);
        let _ = std::fs::remove_file(&final_path);
        return Err(BackupError::Unverified {
            path: final_path,
            reason: format!(
                "artifact is schema v{} but v{version} was expected",
                report.schema_version
            ),
        });
    }

    if let Err(e) = std::fs::rename(&tmp, &final_path) {
        cleanup_temp(&tmp);
        let _ = std::fs::remove_file(&final_path);
        return Err(BackupError::Io {
            path: final_path,
            source: e,
        });
    }

    let meta = std::fs::metadata(&final_path).ok();
    let swept = match retention {
        Some(r) => prune(dir, r, now),
        None => PruneReport::default(),
    };
    Ok(BackupReport {
        pruned: swept.deleted,
        prune_failed: swept.failed,
        kept_unreadable: swept.kept_unreadable,
        retention_skipped: retention.is_none(),
        bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
        owner_only: meta.as_ref().is_none_or(is_owner_only),
        path: final_path,
        ..report
    })
}

/// Whether a file's mode grants nothing to group or other.
///
/// **Checked rather than assumed**, because the mode veld asks for is not always
/// the mode it gets: on the exFAT/FAT external drive and the SMB share that
/// `backup.dir`'s own help text recommends, permission bits do not exist, the
/// request is a silent no-op, and every artifact — carrying the relay tokens and
/// sensitive node outputs the live database is 0600 for — is readable by anyone
/// with the volume. Somebody has to be told, so somebody has to look.
#[cfg(unix)]
fn is_owner_only(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o077 == 0
}

#[cfg(not(unix))]
fn is_owner_only(_meta: &std::fs::Metadata) -> bool {
    true
}

/// Delete temp files from runs that died before they could clean up after
/// themselves.
///
/// Keyed on the file's **mtime** rather than on its name: the name carries a UUID
/// precisely so it is unguessable, which leaves nothing in it to date. Best-effort
/// throughout — a temp file that cannot be deleted is not a reason to fail the
/// backup that noticed it.
fn sweep_abandoned_temp_files(dir: &Path, now: chrono::DateTime<chrono::Utc>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(TEMP_PREFIX) {
            continue;
        }
        let abandoned = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t) < now - PARTIAL_GRACE)
            .unwrap_or(false);
        if abandoned {
            cleanup_temp(&entry.path());
        }
    }
}

/// Remove a partial artifact and any journal SQLite left beside it.
fn cleanup_temp(tmp: &Path) {
    let _ = std::fs::remove_file(tmp);
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = tmp.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
    }
}

/// Build the artifact at `tmp`. Everything about the mechanism lives here; the
/// caller owns naming, renaming and retention.
fn write_artifact(
    source: &Path,
    tmp: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BackupReport, BackupError> {
    // The destination's schema comes from the migration list, which is the only
    // definition of it that exists — so an artifact is at head by construction and
    // there is no second copy of the DDL to keep in step.
    let schema_version = {
        let dest = Db::open_at(tmp)?;
        dest.schema_version()?
    };
    {
        // `Db::open_at` leaves the destination in WAL mode, which would have the
        // finished artifact be three files. A rollback journal is deleted at commit,
        // so the artifact is the single file somebody can copy off the machine.
        //
        // **The result is checked, not discarded.** `PRAGMA journal_mode` reports
        // the mode it ended up in rather than failing, so a switch that does not
        // take leaves the copy going into `tmp-wal` — and then `verify` passes on
        // the pre-rename file, the rename moves only the main file, and the shipped
        // artifact is empty while the report says success.
        let conn = Connection::open(tmp)?;
        let mode: String = conn.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("delete") {
            return Err(BackupError::Unverified {
                path: tmp.to_path_buf(),
                reason: format!(
                    "could not put the backup file in rollback-journal mode (it is {mode:?}), \
                     so it would not be a single self-contained file"
                ),
            });
        }
    }
    cleanup_temp_sidecars(tmp);

    // A **dedicated** connection, never the shared `Arc<Mutex<Connection>>`: this
    // holds a read transaction for the length of the copy, and doing that on the
    // handle every other caller in the process shares would block them all.
    let conn = Connection::open(source)?;
    // Longer than the usual 10s: a backup that gives up because a log writer held
    // the lock is a backup that quietly stops happening on the busiest machines,
    // which are the ones with the most to lose.
    conn.busy_timeout(std::time::Duration::from_secs(60))?;
    // Off for the copy, and it must be set *outside* a transaction to take effect.
    // Tables are copied whole from a database where the constraints already hold, so
    // there is nothing to enforce — but rows arrive in `sqlite_master` order, which
    // is not dependency order, and enforcing per-statement would reject a child that
    // simply arrived before its parent. `foreign_key_check` on the finished artifact
    // is what confirms this was safe rather than assumed.
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;

    // **Refuse a source this binary is behind.** The copy takes the columns the two
    // schemas share and skips a source table the destination lacks, so a database a
    // *newer* veld already migrated would be quietly truncated into an artifact that
    // then verifies clean and gets named with this binary's version — silent
    // omission, which the module docs above claim this mechanism cannot produce.
    // Reachable from the CLI, which (unlike the daemon) carries on when it could not
    // open the database through `Db`.
    let source_version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if source_version != schema_version {
        return Err(BackupError::SourceSchema {
            path: source.to_path_buf(),
            found: source_version,
            head: schema_version,
        });
    }

    conn.execute("ATTACH DATABASE ?1 AS bak", [&tmp.to_string_lossy()])?;

    let copy = copy_tables(&conn, now, schema_version);
    // Give the transaction up whatever happened, so the destination file is closed
    // before the caller renames or deletes it. `ROLLBACK` rather than `COMMIT`: on
    // the error path committing would write a half-copied artifact, and on the
    // success path `copy_tables` has already committed, so this simply reports
    // "no transaction is active" and is ignored.
    let _ = conn.execute_batch("ROLLBACK");
    let _ = conn.execute_batch("DETACH DATABASE bak");
    let rows = copy?;

    verify(tmp, schema_version, &rows)?;

    Ok(BackupReport {
        path: tmp.to_path_buf(),
        bytes: std::fs::metadata(tmp).map(|m| m.len()).unwrap_or(0),
        schema_version,
        taken_at: now,
        rows,
        pruned: Vec::new(),
        prune_failed: Vec::new(),
        kept_unreadable: Vec::new(),
        retention_skipped: false,
        owner_only: true,
    })
}

fn cleanup_temp_sidecars(tmp: &Path) {
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = tmp.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
    }
}

/// Copy every non-excluded table from `main` into the attached `bak`, inside one
/// deferred read transaction, and stamp the artifact's provenance.
///
/// Returns rows copied per table, counted from `changes()` rather than from a
/// second `COUNT(*)` scan of the source.
fn copy_tables(
    conn: &Connection,
    now: chrono::DateTime<chrono::Utc>,
    schema_version: i64,
) -> Result<std::collections::BTreeMap<String, i64>, BackupError> {
    conn.execute_batch("BEGIN DEFERRED")?;

    // The first read of `main`, and therefore the instant the snapshot is taken.
    // Everything below sees the database as it was here, however long the copy runs
    // and whatever any other process commits meanwhile.
    let source_tables = tables(conn, "main")?;
    let dest_tables = tables(conn, "bak")?;

    let mut rows = std::collections::BTreeMap::new();
    for table in &source_tables {
        if EXCLUDED_TABLES.contains(&table.as_str()) || !dest_tables.contains(table) {
            continue;
        }
        // Columns common to both, named explicitly rather than `SELECT *`. Belt and
        // braces at the same schema version — but it is what makes a copy from a
        // database that is *behind* head degrade to "the shared columns" instead of
        // failing outright on a count mismatch.
        let dest_cols = columns(conn, "bak", table)?;
        let source_cols = columns(conn, "main", table)?;
        let shared: Vec<String> = dest_cols
            .into_iter()
            .filter(|c| source_cols.contains(c))
            .map(|c| quote_ident(&c))
            .collect();
        if shared.is_empty() {
            continue;
        }
        let list = shared.join(", ");
        let quoted = quote_ident(table);
        let copied = conn.execute(
            &format!("INSERT INTO bak.{quoted} ({list}) SELECT {list} FROM main.{quoted}"),
            [],
        )?;
        if copied > 0 {
            rows.insert(table.clone(), copied as i64);
        }
    }

    // Provenance, written into the artifact itself. Deliberately last, so its row
    // count is not part of what `verify` re-counts for `kv` — that count is taken
    // after this statement, from the same transaction.
    let meta = serde_json::json!({
        "takenAt": now.to_rfc3339(),
        "schemaVersion": schema_version,
        "veldVersion": env!("CARGO_PKG_VERSION"),
        "excludedTables": EXCLUDED_TABLES,
        "rows": rows,
    });
    conn.execute(
        "INSERT INTO bak.kv (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        rusqlite::params![META_KEY, meta.to_string(), super::ts_to_str(now)],
    )?;
    // Re-read what `kv` actually holds now, so `verify` is checking the artifact
    // against the truth rather than against an arithmetic guess about this row.
    let kv_rows: i64 = conn.query_row("SELECT COUNT(*) FROM bak.kv", [], |r| r.get(0))?;
    rows.insert("kv".to_string(), kv_rows);

    conn.execute_batch("COMMIT")?;
    Ok(rows)
}

/// Confirm the finished artifact holds what was just written into it.
///
/// **Structural checks alone would have passed the copies that survived the real
/// incident** — they opened, they were merely empty and eight schema versions
/// behind. So the row counts are re-counted here, in the file, and a disagreement
/// fails the backup rather than shipping it.
fn verify(
    path: &Path,
    expected_version: i64,
    rows: &std::collections::BTreeMap<String, i64>,
) -> Result<(), BackupError> {
    let fail = |reason: String| BackupError::Unverified {
        path: path.to_path_buf(),
        reason,
    };

    let conn = Connection::open(path)?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(fail(format!("integrity_check said {integrity:?}")));
    }
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let violations = stmt.query_map([], |r| r.get::<_, String>(0))?.count();
    if violations > 0 {
        return Err(fail(format!("{violations} foreign key violation(s)")));
    }
    drop(stmt);
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != expected_version {
        return Err(fail(format!(
            "schema is v{version}, expected v{expected_version}"
        )));
    }
    for (table, expected) in rows {
        let got: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM {}", quote_ident(table)),
            [],
            |r| r.get(0),
        )?;
        if got != *expected {
            return Err(fail(format!(
                "{table} holds {got} row(s), {expected} were copied"
            )));
        }
    }
    Ok(())
}

/// Delete artifacts beyond the retention policy, returning what was deleted.
///
/// **Two horizons, because one is not enough.** `keep` most-recent bounds the disk
/// and not the calendar — twelve copies at a five-minute interval is an hour of
/// history, so a corruption noticed the next morning has nothing to go back to.
/// `keep_daily` days keeps the oldest surviving artifact of each of the last N
/// days, which is what covers "I only noticed on Monday". A day count alone would
/// be unbounded on a machine backing up every five minutes.
///
/// **Only files whose names veld can prove it wrote are candidates.** `backup.dir`
/// is user-supplied and may be a folder holding other things; anything
/// [`parse_name`] does not recognise is not deleted, not listed and not counted.
///
/// Returns `(deleted, failed)`. The second half is not decoration: retention is the
/// only thing bounding disk here, so a delete that keeps failing has to reach
/// somebody rather than leaving every backup reporting success while the directory
/// grows without limit.
/// **A survivor has to be a database, and a file veld cannot read is never
/// deleted.** Both halves were learned the expensive way, one round apart. The name
/// of the artifact being written is claimed with an empty file *before* the copy
/// starts (see [`create`]), so a run killed in between leaves a 0-byte file with a
/// perfectly valid artifact name: it parses, it sorts, it counts against `keep`, and
/// being the earliest of its day it becomes that day's **permanent** survivor while
/// every real backup from that day is deleted around it. The obvious fix — elect
/// survivors only from files whose header reads — then deleted the rest, which
/// included the one file this feature exists to protect: a backup whose page 1 is
/// damaged. See the partition below for the three states that resolves to.
pub fn prune(dir: &Path, retention: Retention, now: chrono::DateTime<chrono::Utc>) -> PruneReport {
    let all = list(dir); // newest first
    // **Three states, not two, and the third is the one that matters.**
    //
    // - `sound` — the header says it is a database. Only these may be survivors.
    // - `litter` — veld's own half-written leftover: readable, and **empty**. This
    //   is exactly what `create` leaves when it is killed between claiming the name
    //   and writing the copy, and it is the only thing here that is ever deleted
    //   outside the retention rules.
    // - everything else — a file that is not empty and whose header does not read.
    //   **Never a survivor and never deleted.** A backup whose page 1 was damaged
    //   lands here, and that is the precise shape of the incident this whole
    //   feature exists for: `sqlite3 .recover` still gets data out of one, so it is
    //   the last file veld may destroy. So does a copy that is merely unreadable
    //   *at this instant* — the wrong mode, or an evicted file on the synced folder
    //   `backup.dir`'s own help recommends, where `unlink` would succeed even
    //   though the read did not.
    //
    // An earlier version partitioned on `header_schema_version(..).is_some()`,
    // which collapsed the third state into the second and deleted both.
    let mut sound: Vec<&Artifact> = Vec::new();
    let mut litter: Vec<&Artifact> = Vec::new();
    let mut kept_unreadable: Vec<PathBuf> = Vec::new();
    for a in &all {
        match header_schema_version(&a.path) {
            Ok(Some(_)) => sound.push(a),
            // Readable and empty: ours, and worthless.
            Ok(None) if a.bytes == 0 => litter.push(a),
            // Readable but not a database, or not readable at all. Left alone —
            // **and reported**, because leaving it alone means retention is no
            // longer bounding this folder and nothing else would say so. Silence
            // here is how a directory grows by a copy an hour with every backup
            // reporting success.
            _ => kept_unreadable.push(a.path.clone()),
        }
    }
    let artifacts = sound;

    let keep_recent = retention.keep.max(0) as usize;
    let daily_cutoff = now - chrono::Duration::days(retention.keep_daily.max(0));

    let mut keep_paths: std::collections::HashSet<PathBuf> = artifacts
        .iter()
        .take(keep_recent)
        .map(|a| a.path.clone())
        .collect();

    if retention.keep_daily > 0 {
        // The *oldest* artifact of each day survives, not the newest: it is the one
        // furthest from whatever went wrong later that day, and keeping the newest
        // would make a day's survivor drift as the day fills up.
        //
        // The day is the **user's**, not UTC's. Artifact names are UTC because that
        // is what sorts, but "keep one per day for a fortnight" is a promise about
        // the calendar somebody lives in: bucketing on UTC puts a New Zealand
        // afternoon into the previous day and prunes it with that day's copies.
        let mut per_day: std::collections::BTreeMap<chrono::NaiveDate, &Artifact> =
            std::collections::BTreeMap::new();
        for artifact in &artifacts {
            if artifact.taken_at < daily_cutoff {
                continue;
            }
            let day = artifact.taken_at.with_timezone(&chrono::Local).date_naive();
            per_day
                .entry(day)
                .and_modify(|kept| {
                    if artifact.taken_at < kept.taken_at {
                        *kept = artifact;
                    }
                })
                .or_insert(artifact);
        }
        keep_paths.extend(per_day.values().map(|a| a.path.clone()));
    }

    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    // Litter first, and only once it has stopped being written: a `create` running
    // right now owns a placeholder that looks exactly like this, and deleting it
    // out from under that run would have its `rename` recreate the name anyway —
    // harmless, but the grace makes it impossible rather than merely survivable.
    for artifact in &litter {
        if now - artifact.taken_at < PARTIAL_GRACE {
            continue;
        }
        match std::fs::remove_file(&artifact.path) {
            Ok(()) => deleted.push(artifact.path.clone()),
            Err(_) => failed.push(artifact.path.clone()),
        }
    }
    for artifact in &artifacts {
        if keep_paths.contains(&artifact.path) {
            continue;
        }
        match std::fs::remove_file(&artifact.path) {
            Ok(()) => deleted.push(artifact.path.clone()),
            Err(_) => failed.push(artifact.path.clone()),
        }
    }
    PruneReport {
        deleted,
        failed,
        kept_unreadable,
    }
}

/// Everything [`restore`] checks before it touches anything, on its own.
///
/// Returns the artifact's schema version. Separated out so "would this restore
/// work?" is answerable without performing one — which is what lets
/// `veld backup list` label each candidate and what lets a restore with no path
/// named pick the newest artifact that is actually **usable** rather than the newest
/// file. On the day this matters the newest file may be the one that recorded the
/// damage.
pub fn inspect(artifact: &Path) -> Result<i64, BackupError> {
    inspect_with(artifact, "quick_check")
}

/// [`inspect`], with SQLite's **full** integrity check.
///
/// What `restore` uses, and the difference is not pedantry. `quick_check` is right
/// for the sweep — it runs over every artifact in the folder on every
/// `veld backup` and every `veld doctor` — but it deliberately skips the
/// index-versus-table comparison, so a copy whose index no longer agrees with its
/// rows passes it. That file being installed as the user's entire state is the one
/// outcome worth paying a full scan to avoid, and "it was verified when it was
/// written" is no argument here: the premise of this whole feature is that a file
/// goes bad *after* it was written.
pub fn inspect_deep(artifact: &Path) -> Result<i64, BackupError> {
    inspect_with(artifact, "integrity_check")
}

fn inspect_with(artifact: &Path, check: &str) -> Result<i64, BackupError> {
    let supported = Db::supported_schema_version();
    let header = header_schema_version(artifact)
        .map_err(|e| BackupError::Io {
            path: artifact.to_path_buf(),
            source: e,
        })?
        .ok_or_else(|| BackupError::NotADatabase {
            path: artifact.to_path_buf(),
        })?;
    if header > supported {
        return Err(BackupError::NewerSchema {
            path: artifact.to_path_buf(),
            found: header,
            supported,
        });
    }

    // Opened read-only in the sense that matters: a plain connection with no
    // migration, so a candidate is inspected without being written to. `Db::open_at`
    // here would migrate the *backup*, mutating the one file that must stay as it
    // was found.
    let conn = Connection::open(artifact)?;
    // **This file is untrusted.** `backup.dir` is a setting whose own help text
    // suggests a synced or shared folder, so anything in it may have been put there
    // by somebody else — and the checks below parse its schema. These are SQLite's
    // own two switches for reading a database you did not write: DEFENSIVE forbids
    // writes to schema structures, and turning `trusted_schema` off stops the
    // schema's own expressions (index expressions, generated columns, views) from
    // running functions during a check. The bundled build defaults `trusted_schema`
    // **on**, so this is a real change and not a restatement of the default.
    let _ = conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true);
    let _ = conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA,
        false,
    );
    // Which check is the caller's decision — see [`inspect`] and [`inspect_deep`].
    // The sweep gets `quick_check` because it runs over every artifact in the folder
    // on every `veld backup` and every `veld doctor`, and a full check on a synced
    // folder also force-materialises every evicted file; `restore` gets the full one
    // because it is about to install the result as the user's whole state.
    let integrity: String = conn.query_row(&format!("PRAGMA {check}"), [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(BackupError::Unverified {
            path: artifact.to_path_buf(),
            reason: format!("{check} said {integrity:?}"),
        });
    }
    // Authoritative, unlike the header read above, which cannot see an
    // un-checkpointed WAL. Re-checked rather than assumed equal: this is the number
    // that decides whether the restored file will open at all, and the check has to
    // happen while the live database is still in place.
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > supported {
        return Err(BackupError::NewerSchema {
            path: artifact.to_path_buf(),
            found: version,
            supported,
        });
    }
    // **Being a readable SQLite file is not being a veld backup**, and everything
    // above only established the former. Without this, any database — another
    // application's, or one built by hand — passes every check and `restore` then
    // installs it as the user's entire state. That state decides what the daemon
    // spawns (`terminal.shell` is an absolute path read straight out of it), so
    // "whatever SQLite will open" is far too wide a door for a directory the
    // setting's own help suggests putting on a shared drive.
    //
    // The provenance row is what `copy_tables` writes into every artifact for
    // exactly this, so the check costs one query and no new format. A file with no
    // `kv` table fails the query and lands in the same arm.
    let is_ours = conn
        .query_row("SELECT COUNT(*) FROM kv WHERE key = ?1", [META_KEY], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0)
        > 0;
    if !is_ours {
        return Err(BackupError::NotAVeldBackup {
            path: artifact.to_path_buf(),
        });
    }
    Ok(version)
}

/// The newest artifact in `dir` that can actually be restored, or `None`.
///
/// **Newest *usable*, not newest**, and the distinction is the whole function: on
/// the day somebody runs this, the newest file may be the one that copied the
/// damage, or one somebody else dropped in the folder.
///
/// Artifacts stamped in the **future** relative to `now` are **de-prioritised, not
/// disqualified**, and the difference is load-bearing in both directions. A name
/// claiming next year sorts to the top of [`list`] and stays there permanently, so
/// letting one win would pin every restore to a file chosen by asserting a date
/// rather than by being recent. But making it *unusable* is worse: a machine whose
/// clock is behind its own artifacts — an unsynced RTC at boot, a restored VM
/// snapshot — has every backup in the future, and the only command that matters
/// then reports that none of them can be restored. That dead end shipped for one
/// review round, introduced by the fix for the first problem.
pub fn newest_usable(dir: &Path, now: chrono::DateTime<chrono::Utc>) -> Option<Artifact> {
    newest_passing(dir, now, inspect)
}

/// The newest artifact that will actually **survive a restore**, or `None`.
///
/// The same rule as [`newest_usable`] but gated on [`inspect_deep`], and the two
/// exist separately because the checks are not the same strength: splitting them
/// meant a no-path `veld backup restore` could pick a candidate on the cheap check
/// and then be refused by the strict one, with no fall-through to the next — a
/// restore that fails *after* choosing, on the day it is the only thing that
/// matters. Choosing with the same check that gates the act is what makes that
/// impossible.
pub fn newest_restorable(dir: &Path, now: chrono::DateTime<chrono::Utc>) -> Option<Artifact> {
    newest_passing(dir, now, inspect_deep)
}

fn newest_passing(
    dir: &Path,
    now: chrono::DateTime<chrono::Utc>,
    check: fn(&Path) -> Result<i64, BackupError>,
) -> Option<Artifact> {
    let artifacts = list(dir);
    artifacts
        .iter()
        .find(|a| a.taken_at <= now && check(&a.path).is_ok())
        .or_else(|| artifacts.iter().find(|a| check(&a.path).is_ok()))
        .cloned()
}

/// Put `artifact` in place at `target`, moving whatever was there aside.
///
/// Refuses **before touching `target`** when the artifact is not a SQLite database,
/// is at a schema this binary cannot open, or does not survive an integrity check —
/// discovering any of those after the move would mean the live database was already
/// replaced by something worse than what it had.
///
/// The caller is responsible for the daemon: a running one holds an open descriptor
/// on the old file and keeps writing to it after the rename.
pub fn restore(artifact: &Path, target: &Path) -> Result<RestoreReport, BackupError> {
    // The deep check, not the sweep's: this is the call that installs a file as the
    // user's entire state, and it is the only place the extra scan is worth paying
    // for. See [`inspect_deep`].
    let found = inspect_deep(artifact)?;

    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let aside = sibling(target, &format!(".replaced-{stamp}"));
    let previous_moved_to = if target.exists() {
        std::fs::rename(target, &aside).map_err(|e| BackupError::Io {
            path: aside.clone(),
            source: e,
        })?;
        Some(aside.clone())
    } else {
        None
    };

    // The `-wal` and `-shm` travel with it, and this runs **whether or not there
    // was a database to move aside** — which is the half that matters most. A WAL
    // beside the restored file is replayed into it on the next open, silently and
    // with `integrity_check` still saying `ok`, grafting the pages of the database
    // being replaced onto the one replacing it. "Main file gone, `-wal` still
    // there" is not a corner case here: it is the disaster this feature exists for,
    // and it is where `veld doctor` sends people. Doing this only on the
    // `target.exists()` branch shipped in the first draft of this function and was
    // caught by review with a live repro.
    //
    // Moved, never deleted, for the same reason the database is: it is evidence.
    for suffix in ["-wal", "-shm"] {
        let from = sibling(target, suffix);
        if from.exists() {
            let _ = std::fs::rename(&from, sibling(&aside, suffix));
        }
    }

    // Copied, not moved: the backup stays a backup. A restore that consumed the
    // artifact would leave somebody one failed attempt away from having nothing.
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| BackupError::CreateDir {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    std::fs::copy(artifact, target).map_err(|e| BackupError::Io {
        path: target.to_path_buf(),
        source: e,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o600));
    }

    Ok(RestoreReport {
        restored_from: artifact.to_path_buf(),
        restored_to: target.to_path_buf(),
        schema_version: found,
        previous_moved_to,
    })
}

/// `path` with `suffix` appended to its filename — `veld.db` + `-wal`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
            .and_utc()
    }

    fn keep(keep: i64, keep_daily: i64) -> Option<Retention> {
        Some(Retention { keep, keep_daily })
    }

    /// A stand-in artifact for the retention tests: a real 64-byte SQLite header
    /// and nothing else.
    ///
    /// The header has to be **complete**. These fixtures were the magic string
    /// alone until `prune` learned to tell a database from a half-written
    /// placeholder — at which point every one of them was correctly reclassified as
    /// litter, and four retention tests failed. A 16-byte file claiming to be a
    /// database is exactly what that rule exists to catch, so the fixture was the
    /// thing that was wrong.
    fn fake_artifact(path: &Path, version: u32) {
        let mut header = [0u8; 64];
        header[..16].copy_from_slice(b"SQLite format 3\0");
        header[60..64].copy_from_slice(&version.to_be_bytes());
        std::fs::write(path, header).unwrap();
    }

    /// A populated source, backed up, produces an artifact that opens as an
    /// ordinary database and holds the state — and none of the bulk rows.
    #[test]
    fn a_backup_keeps_the_state_and_drops_the_bulk() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        let out = dir.path().join("backups");
        {
            let db = Db::open_at(&source).unwrap();
            db.kv_set("hello", "world").unwrap();
            let conn = db.lock();
            conn.execute(
                "INSERT INTO log_lines (project_root, run_name, node, stream, ts, line) \
                 VALUES ('/tmp/p', 'dev', 'n', 'stdout', '2026-01-01T00:00:00Z', 'noise')",
                [],
            )
            .unwrap();
        }

        let report = create(&source, &out, keep(5, 0), utc(2026, 8, 14, 11, 5)).unwrap();
        assert_eq!(
            report.path.file_name().unwrap().to_str().unwrap(),
            format!(
                "veld-20260814T110500Z-v{}.db",
                Db::supported_schema_version()
            )
        );

        // A real veld.db: opened by the ordinary path, no import, no special case.
        let restored = Db::open_at(&report.path).unwrap();
        assert_eq!(restored.kv_get("hello").unwrap().as_deref(), Some("world"));
        assert_eq!(restored.schema_version().unwrap(), report.schema_version);
        let conn = restored.lock();
        let logs: i64 = conn
            .query_row("SELECT COUNT(*) FROM log_lines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(logs, 0, "log_lines must not be copied");
    }

    /// The artifact says what it is, inside itself — so a copy that has been moved
    /// or renamed still answers "when, by what, from what schema".
    #[test]
    fn an_artifact_carries_its_own_provenance() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        Db::open_at(&source).unwrap();

        let report = create(&source, dir.path(), keep(5, 0), utc(2026, 8, 14, 11, 5)).unwrap();
        let db = Db::open_at(&report.path).unwrap();
        let raw = db.kv_get(META_KEY).unwrap().expect("no provenance row");
        let meta: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(meta["schemaVersion"], report.schema_version);
        assert_eq!(meta["veldVersion"], env!("CARGO_PKG_VERSION"));
        assert!(
            meta["excludedTables"]
                .as_array()
                .unwrap()
                .contains(&serde_json::Value::from("log_lines"))
        );
    }

    /// Two backups in the same second are two files.
    ///
    /// `rename` replaces silently on Unix, so the second one used to overwrite the
    /// first — losing a generation from the retention set with nothing reported.
    /// Reachable by running `veld backup now` twice, or by a manual backup landing
    /// in the same second as the scheduled one.
    #[test]
    fn two_backups_in_one_second_are_two_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        Db::open_at(&source).unwrap();
        let out = dir.path().join("backups");
        let at = utc(2026, 8, 14, 11, 5);

        let first = create(&source, &out, keep(10, 0), at).unwrap();
        let second = create(&source, &out, keep(10, 0), at).unwrap();

        assert_ne!(first.path, second.path);
        assert!(first.path.exists() && second.path.exists());
        assert_eq!(list(&out).len(), 2);
        // The second one's recorded timestamp matches its name, so a listing is
        // never labelled with an instant the file does not claim.
        assert_eq!(
            second.path.file_name().unwrap().to_str().unwrap(),
            artifact_name(second.taken_at, second.schema_version)
        );
    }

    /// Every excluded table is a table that exists.
    ///
    /// The denylist is the one place a *rename* fails silently in the expensive
    /// direction: a `log_lines` renamed by a future migration would go on matching
    /// nothing here, and every artifact would quietly carry hundreds of megabytes
    /// of log rows. Nothing in the type system connects the two.
    #[test]
    fn every_excluded_table_still_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
        let conn = db.lock();
        let present = tables(&conn, "main").unwrap();
        for table in EXCLUDED_TABLES {
            assert!(
                present.contains(&table.to_string()),
                "{table} is on the backup denylist but is not in the schema — if it was \
                 renamed, rename it here; if it was dropped, drop it here. Otherwise every \
                 backup silently starts carrying it."
            );
        }
    }

    /// Retention keeps the recent ones *and* one per day, and touches nothing else
    /// in the folder.
    #[test]
    fn retention_keeps_recent_and_daily_and_nothing_elses_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = utc(2026, 8, 14, 12, 0);
        // Four on the current day, one on each of the two before it.
        let stamps = [
            utc(2026, 8, 14, 11, 0),
            utc(2026, 8, 14, 10, 0),
            utc(2026, 8, 14, 9, 0),
            utc(2026, 8, 14, 8, 0),
            utc(2026, 8, 13, 9, 0),
            utc(2026, 8, 12, 9, 0),
        ];
        for at in stamps {
            fake_artifact(&dir.path().join(artifact_name(at, 16)), 16);
        }
        let bystander = dir.path().join("please-do-not-delete-me.db");
        std::fs::write(&bystander, b"mine").unwrap();

        let deleted = prune(
            dir.path(),
            Retention {
                keep: 2,
                keep_daily: 7,
            },
            now,
        )
        .deleted;

        // Kept: the two most recent, plus the oldest of each day inside the window.
        let left: Vec<String> = list(dir.path())
            .into_iter()
            .map(|a| a.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            left,
            vec![
                artifact_name(utc(2026, 8, 14, 11, 0), 16),
                artifact_name(utc(2026, 8, 14, 10, 0), 16),
                artifact_name(utc(2026, 8, 14, 8, 0), 16),
                artifact_name(utc(2026, 8, 13, 9, 0), 16),
                artifact_name(utc(2026, 8, 12, 9, 0), 16),
            ]
        );
        assert_eq!(deleted.len(), 1);
        assert!(
            bystander.exists(),
            "pruning must never delete a file veld did not write — backup.dir is the \
             user's folder"
        );
    }

    /// The daily tail is what makes a slow-noticed corruption recoverable, so it
    /// has to survive a `keep` that is smaller than one day's worth of backups.
    #[test]
    fn a_daily_survivor_outlives_the_recent_window() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = utc(2026, 8, 14, 12, 0);
        for at in [utc(2026, 8, 14, 11, 0), utc(2026, 8, 1, 9, 0)] {
            fake_artifact(&dir.path().join(artifact_name(at, 16)), 16);
        }
        // Two weeks of dailies covers the 1st; `keep: 1` alone would not.
        prune(
            dir.path(),
            Retention {
                keep: 1,
                keep_daily: 14,
            },
            now,
        );
        assert_eq!(list(dir.path()).len(), 2);
        // With the tail off, only the recent window survives.
        prune(
            dir.path(),
            Retention {
                keep: 1,
                keep_daily: 0,
            },
            now,
        );
        assert_eq!(list(dir.path()).len(), 1);
    }

    /// The header read is what lets a restore be refused without opening anything.
    #[test]
    fn a_schema_version_is_readable_from_the_header() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("veld.db");
        // Closed first: the header read cannot see an un-checkpointed WAL, and a
        // freshly-migrated database has its `user_version` bump sitting in one.
        let version = {
            let db = Db::open_at(&path).unwrap();
            db.schema_version().unwrap()
        };
        assert_eq!(header_schema_version(&path).unwrap(), Some(version));

        let not_a_db = dir.path().join("notes.txt");
        std::fs::write(&not_a_db, b"this is not a database, it is a text file").unwrap();
        assert_eq!(header_schema_version(&not_a_db).unwrap(), None);
    }

    /// Restoring replaces the database and **keeps** the one it replaced, along
    /// with its WAL — which would otherwise be replayed into the new file.
    #[test]
    fn restoring_moves_the_old_database_aside_with_its_wal() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        {
            let db = Db::open_at(&source).unwrap();
            db.kv_set("state", "backed-up").unwrap();
        }
        let report = create(&source, dir.path(), keep(5, 0), utc(2026, 8, 14, 11, 5)).unwrap();

        // The live database moves on, then goes wrong.
        {
            let db = Db::open_at(&source).unwrap();
            db.kv_set("state", "after-the-backup").unwrap();
        }
        std::fs::write(sibling(&source, "-wal"), b"a stale write-ahead log").unwrap();

        let restored = restore(&report.path, &source).unwrap();
        let aside = restored.previous_moved_to.expect("nothing was kept");
        assert!(aside.exists(), "the replaced database is the only evidence");
        assert!(
            !sibling(&source, "-wal").exists(),
            "a stale WAL beside the restored file is replayed into it"
        );
        assert!(sibling(&aside, "-wal").exists(), "the WAL moves with it");
        assert!(
            report.path.exists(),
            "a restore must not consume the backup"
        );

        let db = Db::open_at(&source).unwrap();
        assert_eq!(db.kv_get("state").unwrap().as_deref(), Some("backed-up"));
    }

    /// An artifact from a newer veld is refused **before** the live database is
    /// touched — the failure the two stale copies in the real incident would have
    /// produced, discovered at the worst possible moment.
    #[test]
    fn a_newer_artifact_is_refused_without_touching_the_target() {
        let dir = tempfile::TempDir::new().unwrap();
        let artifact = dir.path().join("veld-20260814T110500Z-v999.db");
        {
            let db = Db::open_at(&artifact).unwrap();
            let conn = db.lock();
            conn.pragma_update(None, "user_version", 999).unwrap();
        }
        let target = dir.path().join("veld.db");
        std::fs::write(&target, b"the live database, untouched").unwrap();

        let err = restore(&artifact, &target).unwrap_err();
        assert!(matches!(err, BackupError::NewerSchema { found: 999, .. }));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"the live database, untouched"
        );
    }

    /// A file that is not a database is refused, rather than replacing a working
    /// database with something that cannot be opened at all.
    #[test]
    fn a_file_that_is_not_a_database_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let artifact = dir.path().join("veld-20260814T110500Z-v16.db");
        std::fs::write(&artifact, b"truncated, or never a database").unwrap();
        let target = dir.path().join("veld.db");
        std::fs::write(&target, b"live").unwrap();

        assert!(matches!(
            restore(&artifact, &target).unwrap_err(),
            BackupError::NotADatabase { .. }
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"live");
    }

    /// A stale WAL is cleared even when there is **no database to move aside**.
    ///
    /// The disaster this feature serves is "the database is gone or unopenable",
    /// and `veld doctor` sends people straight here — so "main file missing, `-wal`
    /// still present" is the *normal* input, not a corner. Handling the sidecars
    /// only on the `target.exists()` branch shipped in the first draft: SQLite then
    /// replays the old WAL into the restored file on the next open, silently, with
    /// `integrity_check` still reporting `ok`.
    #[test]
    fn restoring_clears_a_stale_wal_even_when_the_database_is_gone() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        {
            let db = Db::open_at(&source).unwrap();
            db.kv_set("state", "backed-up").unwrap();
        }
        let report = create(&source, dir.path(), keep(5, 0), utc(2026, 8, 14, 11, 5)).unwrap();

        // The database itself is gone; its write-ahead log is not.
        std::fs::remove_file(&source).unwrap();
        std::fs::write(sibling(&source, "-wal"), b"a stale write-ahead log").unwrap();
        std::fs::write(sibling(&source, "-shm"), b"and its index").unwrap();

        let restored = restore(&report.path, &source).unwrap();
        assert!(
            restored.previous_moved_to.is_none(),
            "there was no database to keep"
        );
        assert!(
            !sibling(&source, "-wal").exists() && !sibling(&source, "-shm").exists(),
            "a stale WAL beside the restored file is replayed into it"
        );
        let db = Db::open_at(&source).unwrap();
        assert_eq!(db.kv_get("state").unwrap().as_deref(), Some("backed-up"));
    }

    /// A perfectly healthy SQLite database that veld did not write is refused.
    ///
    /// Being openable is not being ours, and the gap is not academic: `backup.dir`
    /// is a setting whose help suggests a synced or shared folder, and the database
    /// a restore installs decides what the daemon spawns. Anything else with
    /// `user_version` 0 passes the header, the integrity check and the version
    /// check.
    #[test]
    fn a_database_that_is_not_a_veld_backup_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let impostor = dir.path().join("veld-20260814T110500Z-v0.db");
        {
            let conn = Connection::open(&impostor).unwrap();
            conn.execute_batch("CREATE TABLE notes (t TEXT); INSERT INTO notes VALUES ('hi');")
                .unwrap();
        }
        assert!(matches!(
            inspect(&impostor).unwrap_err(),
            BackupError::NotAVeldBackup { .. }
        ));

        let target = dir.path().join("veld.db");
        std::fs::write(&target, b"live").unwrap();
        assert!(restore(&impostor, &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"live");
    }

    /// A future-dated artifact never wins the no-path restore.
    ///
    /// [`list`] sorts on the name's timestamp, so a name claiming next year sorts
    /// first **permanently** — pinning the restore target to a file that won by
    /// asserting a date rather than by being recent. Reachable by a clock that
    /// jumped as well as by somebody else writing into a shared folder.
    #[test]
    fn a_future_dated_artifact_never_wins_the_newest_usable_pick() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        {
            let db = Db::open_at(&source).unwrap();
            db.kv_set("state", "the real one").unwrap();
        }
        let now = utc(2026, 8, 14, 11, 5);
        let real = create(&source, dir.path(), keep(5, 0), now).unwrap();

        // A genuine artifact, stamped a year out.
        let future = dir.path().join(artifact_name(
            utc(2027, 8, 14, 11, 5),
            Db::supported_schema_version(),
        ));
        std::fs::copy(&real.path, &future).unwrap();

        assert_eq!(list(dir.path())[0].path, future, "it does sort first");
        assert_eq!(
            newest_usable(dir.path(), now).map(|a| a.path),
            Some(real.path),
            "but it must not be what a restore picks"
        );
    }

    /// A directory veld did not create keeps the mode it already had.
    ///
    /// The obvious shape — `create_dir_all` then `set_permissions` — chmods the
    /// user's own folder to 0700 on every backup, hourly, undoing a deliberate
    /// `chmod` within the interval, while this feature's own help promises the
    /// folder is safe to share with something else.
    #[cfg(unix)]
    #[test]
    fn an_existing_backup_directory_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        Db::open_at(&source).unwrap();
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();

        let report = create(&source, &shared, keep(5, 0), utc(2026, 8, 14, 11, 5)).unwrap();

        let mode = std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "veld must not chmod a directory it did not create"
        );
        // The artifact itself is still owner-only, which is the part that carries
        // the secrets.
        let file_mode = std::fs::metadata(&report.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert!(report.owner_only);
    }

    /// A directory veld *does* create is owner-only.
    #[cfg(unix)]
    #[test]
    fn a_backup_directory_veld_creates_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        Db::open_at(&source).unwrap();
        let out = dir.path().join("mine");

        create(&source, &out, keep(5, 0), utc(2026, 8, 14, 11, 5)).unwrap();

        let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// A source this binary is *behind* is refused rather than silently truncated.
    ///
    /// The copy takes the columns the two schemas share, so a database a newer veld
    /// already migrated would lose whatever that version added — into an artifact
    /// that then verifies clean and is labelled with this binary's version. Silent
    /// omission, which is exactly what the mechanism claims it cannot produce.
    #[test]
    fn a_source_ahead_of_this_binarys_schema_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        {
            let db = Db::open_at(&source).unwrap();
            let conn = db.lock();
            conn.pragma_update(None, "user_version", 9999).unwrap();
        }
        let err = create(
            &source,
            &dir.path().join("backups"),
            keep(5, 0),
            utc(2026, 8, 14, 11, 5),
        )
        .unwrap_err();
        assert!(matches!(err, BackupError::SourceSchema { found: 9999, .. }));
        // Nothing half-written is left behind.
        assert!(list(&dir.path().join("backups")).is_empty());
    }

    /// Retention says what it could not delete.
    ///
    /// Pruning is the only thing bounding disk here, so a delete that keeps failing
    /// — a read-only directory, a leftover owned by somebody else — must not leave
    /// every backup reporting success while the folder grows without limit.
    #[cfg(unix)]
    #[test]
    fn retention_reports_what_it_could_not_delete() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let now = utc(2026, 8, 14, 12, 0);
        for at in [utc(2026, 8, 14, 11, 0), utc(2026, 8, 1, 9, 0)] {
            fake_artifact(&dir.path().join(artifact_name(at, 16)), 16);
        }
        // A directory nothing can be unlinked from.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let swept = prune(
            dir.path(),
            Retention {
                keep: 1,
                keep_daily: 0,
            },
            now,
        );
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(swept.deleted.is_empty());
        assert_eq!(
            swept.failed.len(),
            1,
            "the failure has to be reported, not swallowed"
        );
    }

    /// A half-written artifact never becomes a day's survivor.
    ///
    /// The regression this pins was **caused by an earlier fix**: claiming the
    /// final name with an empty file (so two producers cannot collide) means a run
    /// killed before the copy finishes leaves a 0-byte file with a perfectly valid
    /// artifact name. It parsed, it sorted, it counted against `keep`, and being
    /// the earliest of its day it became that day's permanent survivor while every
    /// real backup from that day was deleted around it.
    #[test]
    fn a_half_written_artifact_is_not_a_survivor() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = utc(2026, 8, 14, 12, 0);
        // The placeholder is the *oldest* of its day, which is exactly what the
        // daily rule would otherwise elect.
        let placeholder = dir.path().join(artifact_name(utc(2026, 8, 14, 1, 0), 16));
        std::fs::write(&placeholder, b"").unwrap();
        let real: Vec<PathBuf> = [utc(2026, 8, 14, 9, 0), utc(2026, 8, 14, 10, 0)]
            .iter()
            .map(|at| {
                let p = dir.path().join(artifact_name(*at, 16));
                fake_artifact(&p, 16);
                p
            })
            .collect();

        let deleted = prune(
            dir.path(),
            Retention {
                keep: 1,
                keep_daily: 14,
            },
            now,
        );

        assert!(
            !placeholder.exists(),
            "veld's own half-written litter must be cleaned up, not enshrined"
        );
        assert!(deleted.deleted.contains(&placeholder));
        for p in &real {
            assert!(
                p.exists(),
                "{} was deleted to keep an empty file",
                p.display()
            );
        }
    }

    /// The pick a restore acts on is made with the check that gates the act.
    ///
    /// `inspect` (the folder sweep) and `inspect_deep` (the restore gate) are not
    /// the same strength, so choosing with the first and acting on the second means
    /// a no-path `veld backup restore` can fail *after* choosing, with no
    /// fall-through — on the day it is the only thing that matters. Pinned by
    /// making the two picks disagree: an artifact only `inspect_deep` refuses must
    /// be skipped by `newest_restorable` while `newest_usable` still offers it.
    #[test]
    fn the_restore_pick_uses_the_check_that_gates_the_restore() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        {
            let db = Db::open_at(&source).unwrap();
            db.kv_set("state", "sound").unwrap();
        }
        let older = create(&source, dir.path(), keep(9, 0), utc(2026, 8, 14, 10, 0)).unwrap();
        let newer = create(&source, dir.path(), keep(9, 0), utc(2026, 8, 14, 11, 0)).unwrap();

        // Both picks agree while both artifacts are sound.
        let now = utc(2026, 8, 14, 12, 0);
        assert_eq!(
            newest_restorable(dir.path(), now).map(|a| a.path),
            Some(newer.path.clone())
        );

        // Make the newer one fail *only* the deep check.
        corrupt_an_index_key(&newer.path);

        // **Assert the premise before relying on it.** The first version of this
        // test used a fixture that failed *both* checks, which made the whole thing
        // vacuous: the buggy shallow pick would have skipped that artifact too, so
        // the test passed with the regression fully restored. If a future SQLite
        // stops distinguishing these two pragmas, this fails loudly here rather
        // than quietly becoming a test of nothing.
        {
            let conn = Connection::open(&newer.path).unwrap();
            let quick: String = conn
                .query_row("PRAGMA quick_check", [], |r| r.get(0))
                .unwrap();
            let full: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(quick, "ok", "the fixture must pass the shallow check");
            assert_ne!(full, "ok", "and fail the deep one");
        }

        // The shallow sweep still offers it — that is what `veld backup` lists —
        // while the pick a restore acts on skips it for the older, sound one.
        assert_eq!(
            newest_usable(dir.path(), now).map(|a| a.path),
            Some(newer.path)
        );
        assert_eq!(
            newest_restorable(dir.path(), now).map(|a| a.path),
            Some(older.path.clone())
        );
        assert!(restore(&older.path, &dir.path().join("restored.db")).is_ok());
    }

    /// Flip one byte of an index key so the index no longer matches its table.
    ///
    /// This is the one corruption `quick_check` is documented not to catch — it
    /// skips comparing index content against table content — so it is what makes
    /// the two checks actually disagree. Structural damage (a bad rootpage, a
    /// truncated header) fails both and proves nothing about the difference.
    fn corrupt_an_index_key(path: &Path) {
        const KEY: &[u8] = b"cavefloor";
        let (page_size, rootpage) = {
            let conn = Connection::open(path).unwrap();
            conn.execute_batch(
                "CREATE TABLE probe (v TEXT);
                 INSERT INTO probe VALUES ('cavefloor'), ('riverbend');
                 CREATE INDEX probe_v ON probe(v);",
            )
            .unwrap();
            let page_size: i64 = conn
                .query_row("PRAGMA page_size", [], |r| r.get(0))
                .unwrap();
            let rootpage: i64 = conn
                .query_row(
                    "SELECT rootpage FROM sqlite_master WHERE name = 'probe_v'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (page_size, rootpage)
        };

        let mut bytes = std::fs::read(path).unwrap();
        let base = ((rootpage - 1) * page_size) as usize;
        let page = &mut bytes[base..base + page_size as usize];
        let at = page
            .windows(KEY.len())
            .position(|w| w == KEY)
            .expect("the index key is on its own root page");
        page[at] = b'z';
        std::fs::write(path, &bytes).unwrap();
    }

    /// Only regular files are artifacts — a symlink to one included, anything that
    /// is not a file excluded.
    ///
    /// Both halves matter and they were got wrong in turn. Excluding on the
    /// *unfollowed* metadata dropped symlinked artifacts, hiding a backup parked on
    /// another disk from listing, restore and retention. Narrowing the exclusion to
    /// directories then let a **FIFO** through, and `File::open` on a pipe with no
    /// writer blocks forever — so a single named pipe in the backup folder wedged
    /// `veld backup`, the daemon's retention tick and the restore pick.
    #[cfg(unix)]
    #[test]
    fn only_regular_files_are_artifacts_and_a_symlinked_one_counts() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("elsewhere.db");
        fake_artifact(&real, 16);

        // A symlink to a genuine artifact: visible.
        let linked = dir.path().join(artifact_name(utc(2026, 8, 14, 9, 0), 16));
        std::os::unix::fs::symlink(&real, &linked).unwrap();

        // A directory and a FIFO wearing artifact names: neither is one, and the
        // FIFO must never be opened.
        std::fs::create_dir(dir.path().join(artifact_name(utc(2026, 8, 14, 10, 0), 16))).unwrap();
        // Via the shell tool rather than `libc::mkfifo`: veld-core does not depend
        // on `libc`, and adding one for a test is a worse trade than a process.
        let fifo = dir.path().join(artifact_name(utc(2026, 8, 14, 11, 0), 16));
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("mkfifo")
                .success()
        );

        let listed = list(dir.path());
        assert_eq!(
            listed.iter().map(|a| a.path.clone()).collect::<Vec<_>>(),
            vec![linked],
            "a symlinked artifact counts; a directory and a FIFO do not"
        );
        // Would hang rather than fail if the FIFO were still a candidate.
        assert!(newest_usable(dir.path(), utc(2026, 8, 14, 12, 0)).is_none());
    }

    /// A damaged backup is never a survivor and is **never deleted**.
    ///
    /// The regression this pins was caused by the fix for the one above. Electing
    /// survivors only from files whose header reads is right; deleting everything
    /// else is not, because "the header does not read" is also true of a backup
    /// whose page 1 was damaged — the precise shape of the incident this whole
    /// feature exists for, and a file `sqlite3 .recover` can still get data out of.
    /// It is also true of a copy that is merely unreadable at this instant, on the
    /// synced folder `backup.dir`'s help recommends, where `unlink` would have
    /// succeeded anyway.
    #[test]
    fn a_damaged_backup_is_neither_elected_nor_deleted() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = utc(2026, 8, 14, 12, 0);

        // Page 1 gone: 216 KB of real content behind an unreadable header.
        let damaged = dir.path().join(artifact_name(utc(2026, 8, 14, 1, 0), 16));
        std::fs::write(&damaged, vec![0u8; 4096]).unwrap();
        let sound = dir.path().join(artifact_name(utc(2026, 8, 14, 9, 0), 16));
        fake_artifact(&sound, 16);

        let deleted = prune(
            dir.path(),
            Retention {
                keep: 1,
                keep_daily: 14,
            },
            now,
        );

        assert!(
            damaged.exists(),
            "a backup veld cannot read may still be recoverable — it is the last file \
             veld may destroy"
        );
        assert!(!deleted.deleted.contains(&damaged));
        assert!(
            sound.exists(),
            "and the damaged one must not have taken the day's survivor slot"
        );
    }

    /// A backup that is still being written is left alone.
    ///
    /// The other side of the rule above: the placeholder a `create` is holding
    /// right now looks identical to abandoned litter, and only its age tells them
    /// apart.
    #[test]
    fn a_backup_in_flight_is_not_swept() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = utc(2026, 8, 14, 12, 0);
        let in_flight = dir.path().join(artifact_name(now, 16));
        std::fs::write(&in_flight, b"").unwrap();

        prune(
            dir.path(),
            Retention {
                keep: 1,
                keep_daily: 0,
            },
            now,
        );
        assert!(in_flight.exists());
    }

    /// Temp files from runs that died are swept; a fresh one is not.
    ///
    /// They are deliberately named so `parse_name` cannot match them — which is
    /// what keeps retention from deleting a backup in progress, and left nothing at
    /// all to clean them up. Three interrupted runs put 208 MB of orphans in a
    /// folder `veld backup` reported as 138 MB.
    #[test]
    fn abandoned_temp_files_are_swept_and_fresh_ones_are_not() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        Db::open_at(&source).unwrap();

        let orphan = dir
            .path()
            .join(format!("{TEMP_PREFIX}deadbeef{NAME_SUFFIX}"));
        std::fs::write(&orphan, b"a copy that never finished").unwrap();

        // `now` far enough ahead of the file's real mtime to be past the grace.
        let later = chrono::Utc::now() + PARTIAL_GRACE + chrono::Duration::minutes(1);
        create(&source, dir.path(), keep(5, 0), later).unwrap();
        assert!(!orphan.exists());

        let fresh = dir.path().join(format!("{TEMP_PREFIX}cafe{NAME_SUFFIX}"));
        std::fs::write(&fresh, b"a copy in progress right now").unwrap();
        create(&source, dir.path(), keep(5, 0), chrono::Utc::now()).unwrap();
        assert!(
            fresh.exists(),
            "a temp file inside the grace is somebody's live backup"
        );
    }

    /// A clock behind its own artifacts still restores.
    ///
    /// The other regression an earlier fix caused: skipping future-dated artifacts
    /// stopped an invented date winning the pick, and also made *every* backup
    /// unusable on a machine whose clock is behind them — an unsynced RTC at boot,
    /// a restored VM snapshot — so the one command that matters reported that
    /// nothing could be restored. De-prioritised, not disqualified.
    #[test]
    fn a_clock_behind_the_artifacts_still_finds_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        {
            let db = Db::open_at(&source).unwrap();
            db.kv_set("state", "recoverable").unwrap();
        }
        let taken = utc(2026, 8, 14, 11, 5);
        let artifact = create(&source, dir.path(), keep(5, 0), taken).unwrap();

        // The machine now believes it is a year earlier than its own backups.
        let behind = utc(2025, 8, 14, 11, 5);
        assert_eq!(
            newest_usable(dir.path(), behind).map(|a| a.path),
            Some(artifact.path.clone()),
            "a backup must not become unrestorable because the clock is wrong"
        );

        // And the ordering rule still holds when the clock is right: a genuine
        // artifact dated in the future loses to one that is not.
        let future = dir.path().join(artifact_name(utc(2027, 1, 1, 0, 0), 16));
        std::fs::copy(&artifact.path, &future).unwrap();
        assert_eq!(
            newest_usable(dir.path(), utc(2026, 8, 14, 12, 0)).map(|a| a.path),
            Some(artifact.path)
        );
    }

    /// Only veld's own artifacts are recognised — the property that makes a
    /// user-supplied `backup.dir` safe to prune in.
    #[test]
    fn only_velds_own_artifact_names_parse() {
        assert!(parse_name("veld-20260814T110500Z-v16.db").is_some());
        for name in [
            "veld.db",
            "veld-backup.db",
            "veld-20260814T110500Z-v16.db.bak",
            "veld-not-a-date-v16.db",
            "veld-20260814T110500Z-vsixteen.db",
            ".veld-backup-tmp-1234-20260814T110500123Z.db",
            "my-taxes.db",
        ] {
            assert!(parse_name(name).is_none(), "{name} must not parse");
        }
    }

    /// A deferred read transaction does not see a commit made after its first read.
    ///
    /// **Scoped to what it actually checks.** This pins the SQLite semantics the
    /// copy rests on — the read mark is taken at the first read of `main` and held
    /// against every other connection until the commit — using two real
    /// connections. It does **not** drive `create`, and the name says so: an
    /// earlier name claimed "a concurrent write does not land in the artifact",
    /// which is the property, but the body never produced an artifact. Testing it
    /// through `create` would need the copy to be interrupted at a chosen
    /// statement, which nothing here can arrange deterministically; the honest
    /// alternative is to name the layer that *is* covered.
    #[test]
    fn a_deferred_read_transaction_does_not_see_a_later_commit() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("veld.db");
        let db = Db::open_at(&source).unwrap();
        db.kv_set("before", "in the backup").unwrap();

        // A second process's connection, writing while the copy is in flight, is
        // simulated by the tightest version available in one test: hold the
        // snapshot open, write, then finish.
        let conn = Connection::open(&source).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        conn.execute_batch("BEGIN DEFERRED").unwrap();
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv", [], |r| r.get(0))
            .unwrap();
        db.kv_set("during", "must not be in the backup").unwrap();
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv", [], |r| r.get(0))
            .unwrap();
        conn.execute_batch("COMMIT").unwrap();

        let before: i64 = 1;
        assert_eq!(
            after, before,
            "a deferred read transaction must not see a commit made after its first read"
        );
    }
}
