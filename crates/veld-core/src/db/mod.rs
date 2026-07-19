//! Central SQLite storage for all Veld CLI/daemon state.
//!
//! One database file per user (default `<data_dir>/veld/veld.db`, override
//! with `VELD_DB_PATH`) holds everything that used to live in scattered JSON
//! files: run state, the global project registry, service logs, feedback
//! threads, relay auth tokens, and small key/value state (hints, update/GC
//! stamps).
//!
//! Concurrency: the database runs in WAL mode with a busy timeout, so the
//! CLI, the daemon, and detached log-writer processes can read and write
//! concurrently without any advisory file locking.
//!
//! Schema evolution: `PRAGMA user_version` tracks the schema version and
//! [`MIGRATIONS`] upgrades older databases in order on open. A database
//! newer than this binary fails to open with [`DbError::NewerSchema`]
//! instead of corrupting data — running environments are never touched.

pub(crate) mod feedback;
mod import;
mod kv;
mod logs;
pub(crate) mod state;

pub use logs::{LogFilter, LogRow, LogStream};

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("could not determine the user data directory for the veld database")]
    NoDataDir,

    #[error("failed to open veld database {path}: {source}")]
    Open {
        path: PathBuf,
        source: rusqlite::Error,
    },

    #[error("failed to create database directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error(
        "the veld database (schema v{found}) was created by a newer veld version \
         (this binary supports up to v{supported}) — run `veld update` to upgrade"
    )]
    NewerSchema { found: i64, supported: i64 },

    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("failed to (de)serialize stored data: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("run \"{0}\" not found")]
    RunNotFound(String),
}

/// Handle to the central Veld database. Cheap to clone; all clones share one
/// connection guarded by a mutex (SQLite serializes writers anyway, and WAL
/// keeps other *processes* unblocked).
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// The `VELD_DB_PATH` override, if set to a non-empty value.
    fn path_override() -> Option<PathBuf> {
        std::env::var("VELD_DB_PATH")
            .ok()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
    }

    /// The default database path: `$VELD_DB_PATH` or `<data_dir>/veld/veld.db`.
    pub fn default_path() -> Result<PathBuf, DbError> {
        if let Some(p) = Self::path_override() {
            return Ok(p);
        }
        dirs::data_dir()
            .map(|d| d.join("veld").join("veld.db"))
            .ok_or(DbError::NoDataDir)
    }

    /// Open (and migrate) the central database at the default path.
    ///
    /// On first open of the default database this also runs a one-time
    /// best-effort import of pre-SQLite state files (registry, run state,
    /// relay tokens, hints) so environments started by an older veld remain
    /// visible and stoppable after the upgrade. The import is skipped when
    /// `VELD_DB_PATH` points somewhere custom (tests, sandboxes).
    pub fn open() -> Result<Self, DbError> {
        let path = Self::default_path()?;
        let db = Self::open_at(&path)?;
        // Same predicate as `default_path`: only the real default database
        // gets the import (an empty VELD_DB_PATH counts as unset there too).
        if Self::path_override().is_none() {
            db.import_legacy_files_once();
        }
        Ok(db)
    }

    /// Open (and migrate) a database at an explicit path. Used by tests.
    pub fn open_at(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DbError::CreateDir {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        // Create the file 0600 up front — it stores secrets (sensitive node
        // outputs, relay tokens). SQLite creates -wal/-shm with the same mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path);
        }

        let conn = Connection::open(path).map_err(|e| DbError::Open {
            path: path.to_path_buf(),
            source: e,
        })?;

        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        // auto_vacuum must be decided before the first table is created — it
        // cannot be enabled later without a full VACUUM. INCREMENTAL lets GC
        // reclaim pages freed by log/screenshot pruning (see `Db::vacuum`).
        // On an existing database this pragma is a no-op, which is fine.
        conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        // journal_mode returns the resulting mode as a row — use query_row.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Lock the shared connection. Panics only if a previous holder panicked.
    ///
    /// The mutex is NOT reentrant: while the guard is alive, calling any other
    /// `Db` method on the same thread deadlocks silently. Do all your SQL
    /// through the one guard, then drop it before calling other methods.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("veld db mutex poisoned")
    }

    /// The current schema version (`PRAGMA user_version`). For diagnostics.
    pub fn schema_version(&self) -> Result<i64, DbError> {
        let conn = self.lock();
        let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok(v)
    }

    /// Reclaim disk space after large deletes: move freed pages out of the
    /// file (incremental vacuum) and truncate the WAL. Called by GC after
    /// pruning; best-effort.
    pub fn vacuum(&self) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute_batch("PRAGMA incremental_vacuum;")?;
        // wal_checkpoint returns a result row — use query_row.
        let _: (i64, i64, i64) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Migrations
    // -----------------------------------------------------------------------

    fn migrate(&self) -> Result<(), DbError> {
        let supported = MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
        let conn = self.lock();

        // A future data-rewriting migration may hold the write lock longer
        // than the normal 10s budget — give concurrent openers more patience
        // while migrations might be running (reset after the loop).
        conn.busy_timeout(std::time::Duration::from_secs(60))?;

        let outcome = (|| -> Result<(), DbError> {
            loop {
                let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
                if version > supported {
                    return Err(DbError::NewerSchema {
                        found: version,
                        supported,
                    });
                }
                let Some(migration) = MIGRATIONS.iter().find(|m| m.version == version + 1) else {
                    return Ok(()); // up to date
                };

                // BEGIN IMMEDIATE serializes concurrent migrators (two processes
                // upgrading at once); the version is re-checked inside the
                // transaction so the loser of the race becomes a no-op.
                conn.execute_batch("BEGIN IMMEDIATE")?;
                let result = (|| -> Result<bool, rusqlite::Error> {
                    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
                    if v != version {
                        return Ok(false); // someone else migrated first
                    }
                    (migration.apply)(&conn)?;
                    conn.pragma_update(None, "user_version", migration.version)?;
                    Ok(true)
                })();

                match result {
                    Ok(applied) => {
                        conn.execute_batch("COMMIT")?;
                        if applied {
                            tracing::info!(
                                version = migration.version,
                                name = migration.name,
                                "applied veld database migration"
                            );
                        }
                    }
                    Err(e) => {
                        let _ = conn.execute_batch("ROLLBACK");
                        return Err(e.into());
                    }
                }
            }
        })();

        // Back to the normal per-operation budget.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        outcome
    }
}

/// A single schema migration step. `version` is the `user_version` the
/// database has *after* applying it; steps must be consecutive from 1
/// (enforced by the `migrations_are_consecutive` test).
///
/// NEVER modify a migration that has shipped in a release: existing databases
/// are already past it and will never re-run it — your change would apply
/// only to fresh databases and every upgraded user would be missing it
/// (e.g. "no such column" at runtime). Schema changes are always a NEW
/// migration appended to `MIGRATIONS`.
struct Migration {
    version: i64,
    name: &'static str,
    /// The migration body. Runs inside an IMMEDIATE transaction; may execute
    /// arbitrary SQL and Rust (e.g. rewrite JSON payloads row by row).
    apply: fn(&Connection) -> rusqlite::Result<()>,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial-schema",
    apply: migrate_v1_initial,
}];

fn migrate_v1_initial(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE projects (
            root TEXT PRIMARY KEY,
            name TEXT NOT NULL
        );

        CREATE TABLE runs (
            id INTEGER PRIMARY KEY,
            project_root TEXT NOT NULL REFERENCES projects(root) ON DELETE CASCADE,
            name TEXT NOT NULL,
            run_id TEXT NOT NULL,
            status TEXT NOT NULL,
            execution_order TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            stopped_at TEXT,
            UNIQUE(project_root, name)
        );

        CREATE TABLE nodes (
            id INTEGER PRIMARY KEY,
            run_row INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            node_key TEXT NOT NULL,
            node_name TEXT NOT NULL,
            variant TEXT NOT NULL,
            status TEXT NOT NULL,
            pid INTEGER,
            port INTEGER,
            url TEXT,
            outputs TEXT NOT NULL DEFAULT '{}',
            readiness_phases TEXT NOT NULL DEFAULT '[]',
            recovery_count INTEGER NOT NULL DEFAULT 0,
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            last_liveness_error TEXT,
            sensitive_keys TEXT NOT NULL DEFAULT '[]',
            UNIQUE(run_row, node_key)
        );
        CREATE INDEX idx_nodes_run ON nodes(run_row);

        -- AUTOINCREMENT (not plain rowid) so ids stay strictly monotonic even
        -- after pruning deletes the highest rows — follow mode uses the id as
        -- a watermark across processes and must never see an id reused.
        CREATE TABLE log_lines (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_root TEXT NOT NULL,
            run_name TEXT NOT NULL,
            node TEXT,
            variant TEXT,
            stream TEXT NOT NULL,
            ts TEXT NOT NULL,
            line TEXT NOT NULL
        );
        CREATE INDEX idx_log_lines_scope ON log_lines(project_root, run_name, id);
        CREATE INDEX idx_log_lines_ts ON log_lines(ts);

        CREATE TABLE feedback_threads (
            project_root TEXT NOT NULL,
            run_name TEXT NOT NULL,
            id TEXT NOT NULL,
            payload TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (project_root, run_name, id)
        );

        CREATE TABLE feedback_events (
            project_root TEXT NOT NULL,
            run_name TEXT NOT NULL,
            seq INTEGER NOT NULL,
            payload TEXT NOT NULL,
            ts TEXT NOT NULL,
            PRIMARY KEY (project_root, run_name, seq)
        );

        CREATE TABLE feedback_sessions (
            project_root TEXT NOT NULL,
            run_name TEXT NOT NULL,
            status TEXT NOT NULL,
            last_heartbeat TEXT NOT NULL,
            ended_at TEXT,
            PRIMARY KEY (project_root, run_name)
        );

        CREATE TABLE feedback_screenshots (
            project_root TEXT NOT NULL,
            run_name TEXT NOT NULL,
            filename TEXT NOT NULL,
            data BLOB NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (project_root, run_name, filename)
        );

        CREATE TABLE relay_tokens (
            relay_url TEXT PRIMARY KEY,
            token TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE kv (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )
}

// ---------------------------------------------------------------------------
// Timestamp helpers — one canonical format for every TEXT timestamp column
// (RFC 3339, UTC, microsecond precision, `Z` suffix) so lexicographic
// comparison equals chronological comparison.
// ---------------------------------------------------------------------------

pub(crate) fn ts_to_str(ts: chrono::DateTime<chrono::Utc>) -> String {
    ts.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

pub(crate) fn now_str() -> String {
    ts_to_str(chrono::Utc::now())
}

pub(crate) fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

#[cfg(test)]
pub(crate) fn test_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
    (dir, db)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_consecutive() {
        // `migrate()` walks version+1 steps and would silently stop at a gap;
        // `supported` assumes the list is sorted. Enforce both.
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                m.version,
                (i + 1) as i64,
                "MIGRATIONS[{i}] ('{}') must have version {} — steps are consecutive from 1",
                m.name,
                i + 1
            );
        }
    }

    #[test]
    fn timestamps_sort_lexicographically() {
        // `logs_since` (ts >= ?) and GC pruning compare timestamp TEXT
        // columns as strings — ts_to_str must keep lexicographic order equal
        // to chronological order (fixed width, UTC, Z suffix).
        let base = chrono::Utc::now();
        let mut prev = ts_to_str(base - chrono::Duration::microseconds(10));
        for us in [-5i64, -1, 0, 1, 999, 1_000_000, 60_000_000] {
            let next = ts_to_str(base + chrono::Duration::microseconds(us));
            assert!(prev < next, "{prev} !< {next}");
            prev = next;
        }
    }

    #[test]
    fn open_creates_schema_and_reopens() {
        let (dir, db) = test_db();
        drop(db);
        // Re-open: migrations are idempotent (no-op at latest version).
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
        let v: i64 = db
            .lock()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.last().unwrap().version);
    }

    #[test]
    fn newer_schema_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("veld.db");
        let db = Db::open_at(&path).unwrap();
        db.lock().pragma_update(None, "user_version", 9999).unwrap();
        drop(db);
        match Db::open_at(&path) {
            Err(DbError::NewerSchema { found, .. }) => assert_eq!(found, 9999),
            Err(e) => panic!("expected NewerSchema, got {e}"),
            Ok(_) => panic!("expected NewerSchema, got Ok"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn db_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _db) = test_db();
        let mode = std::fs::metadata(dir.path().join("veld.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "db holds secrets and must be private");
    }
}
