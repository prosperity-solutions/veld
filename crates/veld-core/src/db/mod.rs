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
mod panes;
mod settings;
pub(crate) mod state;
mod stats;
mod var_overrides;
mod worktrees;

pub use logs::{LogFilter, LogRow, LogStream, stream_is_per_node};
pub use panes::{PaneSession, mint_pane_token};
pub use settings::{DEFAULT_DETACH_GRACE_MINUTES, LogTimeZone, MAX_RUN_HISTORY_DAYS};
pub use var_overrides::{OverrideScope, VarOverride};
pub use worktrees::{
    DiscoveredWorktree, LaneRecord, MAX_LANE_NAME_LEN, MAX_ORDER_LEN, RepoRecord, WORKTREE_COLORS,
    WORKTREE_EMOJI, WorktreeRecord, default_alias, is_worktree_color, is_worktree_emoji,
};

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

    #[error("{0:?} is not one of the curated worktree glyphs")]
    InvalidEmoji(String),

    #[error("{0:?} is not a usable worktree marker colour")]
    InvalidColor(String),

    #[error("{value} is not a valid value for setting {key:?}")]
    InvalidSetting { key: String, value: String },

    #[error("another checkout of this repo is already called {0:?} — pick a different alias")]
    AliasTaken(String),

    #[error("this repo has no lane called {0:?}")]
    UnknownLane(String),

    #[error("this repo already has a lane called {0:?}")]
    LaneTaken(String),

    #[error(
        "a lane name must be 1–{max} characters",
        max = crate::db::worktrees::MAX_LANE_NAME_LEN
    )]
    InvalidLaneName(String),

    #[error("this repo already has the maximum of {0} lanes")]
    TooManyLanes(usize),

    #[error(
        "a reorder may list at most {max} entries (got {0})",
        max = crate::db::worktrees::MAX_ORDER_LEN
    )]
    OrderTooLong(usize),

    #[error("refusing to remove the main checkout — remove the repo instead")]
    RefusingMainWorktree,

    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("failed to (de)serialize stored data: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("run \"{0}\" not found")]
    RunNotFound(String),

    #[error(
        "environment \"{0}\" already has a run in progress (starting/running/stopping) — \
         stop it first or wait for its teardown to finish"
    )]
    EnvironmentBusy(String),

    #[error("run id prefix \"{0}\" matches more than one run — use more characters")]
    AmbiguousRunId(String),
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

    /// A cargo-built binary's own dev database, or `None` for an installed one.
    ///
    /// **Why this exists.** `Db::open()` used to resolve straight to
    /// `<data_dir>/veld/veld.db` — the developer's real database — for *every*
    /// caller, including `cargo test`. Nothing guarded it: no `.cargo/config.toml`,
    /// no `[env]`, nothing in CI. A test that built an axum router and drove one
    /// request through it would migrate the production schema, and since an older
    /// binary refuses a newer `user_version` ([`DbError::NewerSchema`]), a single
    /// `cargo test` could leave the installed veld unable to open its own database
    /// at all. That is exactly what happened when the terminal detach grace became
    /// a setting: reading it moved a `Db::open()` into the session-spawn path, and
    /// twelve existing PTY tests migrated the real database as a side effect.
    ///
    /// A `#[cfg(test)]` guard cannot fix this. `veld-core` is compiled *without*
    /// `cfg(test)` when `veld-daemon`'s tests link it, so the panic would never
    /// fire for the callers that matter.
    ///
    /// So the rule is a property of the *binary*: anything cargo built lives under
    /// a target directory, and cargo marks that directory with `CACHEDIR.TAG`. An
    /// installed binary (`/usr/local/bin/veld`, a `.app` bundle) does not, so
    /// production behaviour is untouched. Dev and test get a dev database
    /// automatically, with no env var to remember.
    ///
    /// **It sits beside the `justfile`'s `dev_db` but is not the same file.**
    /// `just dev` and `just dev-daemon` set `VELD_DB_PATH` explicitly
    /// (`justfile:46`, `:73`) and own `.veld-dev/veld.db`; a cargo-built binary gets
    /// `.veld-dev/veld-cargo.db`. Same gitignored directory, so `just dev-db-reset`
    /// territory and nothing new to explain — but a separate file, because
    /// `cargo test --workspace` would otherwise migrate and write the database a
    /// *running* dev daemon owns, and a `cargo test` between
    /// `just dev-db-from-real` and `just dev` would silently migrate the snapshot
    /// to head so the rehearsal verified nothing. Caught in review; the first
    /// version shared one file on the reasoning that one dev database is simpler,
    /// which was true and wrong.
    ///
    /// The `justfile` beside the target directory's parent is what identifies the
    /// worktree; without it (a vendored build, a `CARGO_TARGET_DIR` outside the
    /// tree) it falls back inside the target directory, which is still never the
    /// user's real database.
    ///
    /// `VELD_DB_PATH` still wins over this, so a test wanting true isolation keeps
    /// saying so explicitly — and `Db::open_at` remains the right call for a test
    /// that wants a tempdir.
    fn cargo_target_db() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        // `target/debug/veld`, `target/release/veld`, `target/debug/deps/<test>`,
        // and `target/<triple>/debug/veld` all sit below the marked directory, so
        // walk up until the marker is found rather than assuming a depth.
        // Bounded: the walk stops at the home directory rather than climbing to
        // `/`. An unbounded walk means any tagged ancestor — a stray
        // `CACHEDIR.TAG` in `~/Library/Caches`, say — silently diverts an
        // *installed* binary onto a dev database, presenting as empty state, which
        // is the failure mode this guard was written in response to.
        let home = dirs::home_dir();
        let mut dir = exe.parent()?;
        let marked = loop {
            // The bound is checked *first*: a `CACHEDIR.TAG` sitting at `$HOME`
            // itself must not divert an installed binary, which is the whole point
            // of bounding the walk. Probing before stopping honoured exactly the
            // case the bound exists to refuse.
            if home.as_deref() == Some(dir) {
                return None;
            }
            if dir.join("CACHEDIR.TAG").is_file() {
                break dir;
            }
            dir = dir.parent()?;
        };
        // The worktree root is the marked directory's parent for a default
        // `target/`; require the justfile before claiming it, since that is what
        // names the dev directory these two files share.
        if let Some(root) = marked.parent() {
            if root.join("justfile").is_file() {
                return Some(root.join(".veld-dev").join("veld-cargo.db"));
            }
        }
        Some(marked.join("veld-dev.db"))
    }

    /// The default database path: `$VELD_DB_PATH`, else a cargo-built binary's
    /// [`cargo_target_db`], else `<data_dir>/veld/veld.db`.
    pub fn default_path() -> Result<PathBuf, DbError> {
        if let Some(p) = Self::path_override() {
            return Ok(p);
        }
        if let Some(p) = Self::cargo_target_db() {
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
    /// `VELD_DB_PATH` points somewhere custom (tests, sandboxes) and when this is
    /// a cargo-built binary using its own [`cargo_target_db`] — in both cases the
    /// database is not the one the user's real state belongs in, and importing into
    /// it would both waste the work and mark the one-time import as done.
    pub fn open() -> Result<Self, DbError> {
        let path = Self::default_path()?;
        let db = Self::open_at(&path)?;
        // Mirrors `default_path`'s precedence: only the genuine `<data_dir>`
        // database gets the import (an empty VELD_DB_PATH counts as unset there
        // too).
        if Self::path_override().is_none() && Self::cargo_target_db().is_none() {
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

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial-schema",
        apply: migrate_v1_initial,
    },
    Migration {
        version: 2,
        name: "node-process-stats",
        apply: migrate_v2_node_stats,
    },
    Migration {
        version: 3,
        name: "environments-and-runs",
        apply: migrate_v3_environments_and_runs,
    },
    Migration {
        version: 4,
        name: "run-graph-snapshot",
        apply: migrate_v4_graph_snapshot,
    },
    Migration {
        version: 5,
        name: "desktop-repos-worktrees",
        apply: migrate_v5_desktop_worktrees,
    },
    Migration {
        version: 6,
        name: "worktree-emoji",
        apply: migrate_v6_worktree_emoji,
    },
    Migration {
        version: 7,
        name: "detailed-process-stats",
        apply: migrate_v7_detailed_process_stats,
    },
    Migration {
        version: 8,
        name: "settings",
        apply: migrate_v8_settings,
    },
    Migration {
        version: 9,
        name: "worktree-marker-color",
        apply: migrate_v9_worktree_marker_color,
    },
    Migration {
        version: 10,
        name: "rail-lanes-and-worktree-trash",
        apply: migrate_v10_rail_lanes_and_trash,
    },
    Migration {
        version: 11,
        name: "pane-sessions",
        apply: migrate_v11_pane_sessions,
    },
    Migration {
        version: 12,
        name: "machine-var-overrides",
        apply: migrate_v12_var_overrides,
    },
    Migration {
        version: 13,
        name: "node-per-port-urls",
        apply: migrate_v13_node_urls,
    },
];

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

/// v2: per-node process resource stats (CPU/memory/process-count time series).
///
/// Kept in its own table, not as columns on `nodes`: `save_run` rewrites every
/// node row on each state change, which would clobber volatile samples, and a
/// separate table lets samples accumulate as a time series that GC prunes by
/// age. Rows cascade-delete with their run (same `run_row` FK as `nodes`).
fn migrate_v2_node_stats(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE node_stats (
            id INTEGER PRIMARY KEY,
            run_row INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            node_key TEXT NOT NULL,
            cpu_percent REAL NOT NULL,
            memory_bytes INTEGER NOT NULL,
            process_count INTEGER NOT NULL,
            sampled_at TEXT NOT NULL
        );
        -- Serves per-node latest/history lookups (run_row + node_key, newest
        -- first via the trailing sampled_at).
        CREATE INDEX idx_node_stats_lookup ON node_stats(run_row, node_key, sampled_at);
        -- Serves the age-based GC prune that scans across all runs.
        CREATE INDEX idx_node_stats_sampled ON node_stats(sampled_at);
        "#,
    )
}

/// v3: split "runs" into environments (the durable named slot) × runs (one
/// execution instance each, keyed by `run_id`). Stopped/crashed runs become
/// retention-bounded history instead of being deleted, and `log_lines` gains
/// per-instance scoping.
///
/// Rebuild mechanics: SQLite cannot alter constraints, and `PRAGMA
/// foreign_keys=OFF` is a no-op inside this already-open transaction — a
/// naive `DROP TABLE runs` would cascade-delete every `nodes`/`node_stats`
/// row through their `ON DELETE CASCADE` FKs. So all three tables are rebuilt
/// in dependency order: create the new shapes, copy rows (preserving
/// `runs.id` so `nodes.run_row` values stay valid), drop children before the
/// parent, then rename — `ALTER TABLE ... RENAME` rewrites the referencing FK
/// clauses to follow.
fn migrate_v3_environments_and_runs(conn: &Connection) -> rusqlite::Result<()> {
    // Guard: `run_id` becomes UNIQUE. Duplicates can only exist in a DB whose
    // rows predate the SQLite import. Re-key them with fresh UUIDs (not a
    // text suffix — the value must stay a parseable UUID, or the row becomes
    // unaddressable by every run_id-keyed operation after loading as nil).
    {
        let dup_ids: Vec<i64> = conn
            .prepare(
                "SELECT id FROM runs
                 WHERE id NOT IN (SELECT MIN(id) FROM runs GROUP BY run_id)",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in dup_ids {
            conn.execute(
                "UPDATE runs SET run_id = ?1 WHERE id = ?2",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), id],
            )?;
        }
    }
    conn.execute_batch(
        r#"
        CREATE TABLE environments (
            id INTEGER PRIMARY KEY,
            project_root TEXT NOT NULL REFERENCES projects(root) ON DELETE CASCADE,
            name TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(project_root, name)
        );

        CREATE TABLE runs_v3 (
            id INTEGER PRIMARY KEY,
            environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
            run_id TEXT NOT NULL UNIQUE,
            status TEXT NOT NULL,
            end_reason TEXT,
            end_detail TEXT,
            execution_order TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            -- When begin_ending moved the run to 'stopping' — the daemon's
            -- stale-stopping reaper uses this as its grace-period clock.
            ending_at TEXT,
            ended_at TEXT
        );

        INSERT INTO environments (project_root, name, created_at)
            SELECT project_root, name, created_at FROM runs;

        -- Copy each old run, preserving its rowid. Status normalization:
        -- live statuses carry over with end_reason NULL; terminal rows get the
        -- matching end_reason; anything outside the known set (a persisted
        -- 'recovering', which is never written in practice) is normalized to
        -- stopped so it cannot sit outside both the live set and every
        -- reaper's gate forever.
        INSERT INTO runs_v3 (id, environment_id, run_id, status, end_reason, end_detail,
                             execution_order, created_at, ended_at)
            SELECT r.id, e.id, r.run_id,
                   CASE WHEN r.status IN ('starting','running','stopping','failed') THEN r.status
                        ELSE 'stopped' END,
                   CASE WHEN r.status IN ('starting','running') THEN NULL
                        WHEN r.status = 'stopping' THEN NULL
                        WHEN r.status = 'failed' THEN 'failed'
                        ELSE 'stopped' END,
                   CASE WHEN r.status IN ('starting','running','stopping','failed','stopped') THEN NULL
                        ELSE '{"message":"status normalized by v3 migration"}' END,
                   r.execution_order, r.created_at, r.stopped_at
            FROM runs r
            JOIN environments e ON e.project_root = r.project_root AND e.name = r.name;

        CREATE TABLE nodes_v3 (
            id INTEGER PRIMARY KEY,
            run_row INTEGER NOT NULL REFERENCES runs_v3(id) ON DELETE CASCADE,
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
        INSERT INTO nodes_v3 SELECT * FROM nodes;

        CREATE TABLE node_stats_v3 (
            id INTEGER PRIMARY KEY,
            run_row INTEGER NOT NULL REFERENCES runs_v3(id) ON DELETE CASCADE,
            node_key TEXT NOT NULL,
            cpu_percent REAL NOT NULL,
            memory_bytes INTEGER NOT NULL,
            process_count INTEGER NOT NULL,
            sampled_at TEXT NOT NULL
        );
        INSERT INTO node_stats_v3 SELECT * FROM node_stats;

        -- Children before parent, so nothing cascades.
        DROP TABLE node_stats;
        DROP TABLE nodes;
        DROP TABLE runs;

        ALTER TABLE runs_v3 RENAME TO runs;
        ALTER TABLE nodes_v3 RENAME TO nodes;
        ALTER TABLE node_stats_v3 RENAME TO node_stats;

        CREATE INDEX idx_nodes_run ON nodes(run_row);
        CREATE INDEX idx_node_stats_lookup ON node_stats(run_row, node_key, sampled_at);
        CREATE INDEX idx_node_stats_sampled ON node_stats(sampled_at);
        CREATE INDEX idx_runs_env ON runs(environment_id, created_at);

        -- The one-live-run invariant, enforced by the engine: a second
        -- concurrent `veld start` fails atomically instead of racing a
        -- check-then-act in application code.
        CREATE UNIQUE INDEX idx_runs_one_live ON runs(environment_id)
            WHERE status IN ('starting','running','stopping');

        ALTER TABLE log_lines ADD COLUMN run_id TEXT;
        "#,
    )?;

    // Prune before indexing: the run_id index build scans the whole table,
    // which can hold a week of logs — shrink it first (same 168h policy GC
    // applies) so the migration stays inside the 60s busy budget.
    let cutoff = ts_to_str(chrono::Utc::now() - chrono::Duration::hours(168));
    conn.execute("DELETE FROM log_lines WHERE ts < ?1", [&cutoff])?;
    conn.execute_batch("CREATE INDEX idx_log_lines_run_id ON log_lines(run_id, id);")?;
    Ok(())
}

/// v4: per-run graph snapshot (config forensics). A separate migration — NOT
/// folded into v3 — because v3 already existed on this branch before the
/// column did, so a database that migrated to v3 under an earlier build must
/// still gain the column ("schema changes are always a NEW migration").
///
/// The JSON (see `GraphSnapshot`) is pre-interpolation by design:
/// placeholders stay `${...}`, env is names-only — no resolved value (port,
/// URL, secret output) ever lands here.
fn migrate_v4_graph_snapshot(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("ALTER TABLE runs ADD COLUMN graph_snapshot TEXT;")
}

/// v5: the desktop app's repo/worktree registry.
///
/// A desktop "repo" is a git repository the user imported (keyed by the main
/// checkout root); "worktrees" are its `git worktree` checkouts, each with a
/// user-editable alias. This is deliberately separate from `projects`: veld
/// keys projects by "any directory containing a veld.json", so every worktree
/// with a config is its own veld project — the desktop model sits one level
/// above and joins run state by path (`worktrees.path` = `projects.root`).
fn migrate_v5_desktop_worktrees(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE repos (
            root TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE worktrees (
            id INTEGER PRIMARY KEY,
            repo_root TEXT NOT NULL REFERENCES repos(root) ON DELETE CASCADE,
            path TEXT NOT NULL UNIQUE,
            branch TEXT NOT NULL,
            alias TEXT NOT NULL,
            is_main INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE INDEX idx_worktrees_repo ON worktrees(repo_root);
        "#,
    )
}

/// v6: per-worktree emoji identifier for the desktop UI's collapsed rail.
/// Assigned on insert (unique across all worktrees while free choices
/// remain); existing rows get one lazily via backfill in the desktop layer —
/// an empty string means "not assigned yet".
fn migrate_v6_worktree_emoji(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("ALTER TABLE worktrees ADD COLUMN emoji TEXT NOT NULL DEFAULT '';")
}

/// v7: detailed memory breakdown on the node aggregate, plus a per-process
/// table so a node's subprocesses can be graphed individually.
///
/// The new `node_stats` columns are all nullable with no default, and that is
/// deliberate: a row written before this migration genuinely has no footprint
/// reading, and `NULL` is how a reader tells that from a real zero. `footprint`
/// falls back to `memory_bytes` at read time (see `stats_from_row`), so old
/// samples keep plotting on the default metric instead of collapsing to a flat
/// zero line halfway through a graph.
///
/// Note this ALTERs the table that **v3 rebuilt** (`node_stats_v3`, renamed to
/// `node_stats`) — not the one v2 created. v2's body must stay untouched.
///
/// `node_process_stats` is a separate table rather than a JSON blob on
/// `node_stats` because the point of storing it is per-PID time series: "graph
/// this one subprocess over the last hour" has to be a `WHERE pid = ?` with an
/// index behind it, not a scan that deserializes every tree in the window. It
/// uses `AUTOINCREMENT` for the same reason `log_lines` does — ids stay strictly
/// monotonic across pruning, so nothing that watermarks on id can go backwards.
fn migrate_v7_detailed_process_stats(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        ALTER TABLE node_stats ADD COLUMN cpu_seconds REAL;
        ALTER TABLE node_stats ADD COLUMN footprint INTEGER;
        ALTER TABLE node_stats ADD COLUMN virtual_bytes INTEGER;
        ALTER TABLE node_stats ADD COLUMN private_clean INTEGER;
        ALTER TABLE node_stats ADD COLUMN private_dirty INTEGER;
        ALTER TABLE node_stats ADD COLUMN shared_clean INTEGER;
        ALTER TABLE node_stats ADD COLUMN shared_dirty INTEGER;
        ALTER TABLE node_stats ADD COLUMN swap INTEGER;
        ALTER TABLE node_stats ADD COLUMN wired INTEGER;

        CREATE TABLE node_process_stats (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_row INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            node_key TEXT NOT NULL,
            sampled_at TEXT NOT NULL,
            pid INTEGER NOT NULL,
            parent_pid INTEGER,
            depth INTEGER NOT NULL,
            name TEXT NOT NULL,
            cmd TEXT,
            cpu_percent REAL NOT NULL,
            cpu_seconds REAL NOT NULL,
            memory_bytes INTEGER NOT NULL,
            footprint INTEGER NOT NULL,
            virtual_bytes INTEGER NOT NULL,
            private_clean INTEGER,
            private_dirty INTEGER,
            shared_clean INTEGER,
            shared_dirty INTEGER,
            swap INTEGER,
            wired INTEGER,
            started_at TEXT
        );
        -- The window query: one node's processes over a time range.
        CREATE INDEX idx_node_process_stats_lookup
            ON node_process_stats(run_row, node_key, sampled_at);
        -- The GC query: age-based pruning across every run.
        CREATE INDEX idx_node_process_stats_sampled ON node_process_stats(sampled_at);
        "#,
    )
}

/// v8: user preferences, one row per setting.
///
/// **One row per setting, not one JSON document.** The grain of the write is the
/// grain of the conflict: two UI clients saving different fields both win, and a
/// genuine same-key collision is last-write-wins, which is the right answer for a
/// font size. A single document would make every save a read-modify-write over
/// everything, so the last tab to close would silently revert the other's edit.
///
/// **`value` is a JSON scalar and the store is untyped at rest.** Typing lives in
/// the accessors (`settings.rs`), which validate and clamp *known* keys. An
/// unknown key is stored and echoed back verbatim rather than rejected, because a
/// downgrade is a real scenario — `DbError::NewerSchema` exists precisely because
/// users move between builds — and a newer client's preference must survive a
/// round trip through an older daemon instead of being deleted by it.
///
/// **`scope` ships now, with every first-wave row `global`.** Per-project
/// overrides are the shape a future "configurable vars" feature needs, and the
/// column costs one TEXT plus a composite key today versus a migration and a
/// rewrite of every accessor later. It carries no information yet; that is the
/// price, and it is stated rather than discovered.
fn migrate_v8_settings(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE settings (
            scope      TEXT NOT NULL DEFAULT 'global',
            key        TEXT NOT NULL,
            value      TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (scope, key)
        );
        "#,
    )
}

/// v9: the colour half of a worktree's marker, stored as a literal `#rrggbb`.
///
/// A marker is a **composite** — this colour and the `emoji` glyph — and the
/// `worktree.markerStyle` setting picks which face renders. Both faces are stored
/// permanently, so switching colour → emoji and back is lossless. Modelling it as a
/// tagged union would turn a rendering choice into a data migration and let a
/// preference destroy a hand-picked glyph.
///
/// **The colour itself, not an index into a palette.** The first version stored an
/// index, which was wrong three ways: retuning the palette silently repainted every
/// existing worktree (a marker is an identity — it must not change under the user);
/// a custom colour could not be expressed at all, so allowing one later would have
/// needed another migration; and the index created a Rust↔CSS coupling that had to
/// be held together by a drift test, which is a gate existing to defend a shape
/// rather than a property worth having. Storing the value also makes the two marker
/// channels symmetric — `emoji` has always stored the glyph, not an offset into
/// [`WORKTREE_EMOJI`].
///
/// The empty string means "not assigned yet" and is backfilled lazily on the next
/// worktree sync, exactly as v6's `emoji` sentinel is. Nothing derives a marker from
/// `worktrees.id`: rowids are reused, so an id-derived marker would be inherited by
/// an unrelated worktree.
fn migrate_v9_worktree_marker_color(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("ALTER TABLE worktrees ADD COLUMN marker_color TEXT NOT NULL DEFAULT '';")
}

/// v10: rail organisation (`lanes`, `worktrees.lane`, `worktrees.sort_position`)
/// and worktree trash (`worktrees.trashed_at`, `worktrees.trash_error`).
///
/// **Why this is not in the `settings` table.** The `scope` column added in v8
/// exists for per-project *user* state and this is per-project user state, so the
/// settings store is the obvious home and it is the wrong one. Lanes are a
/// relation, not a scalar: `settings.validate` is built for clamped numbers and
/// `one_of` enums, a lane document has no referential relationship to `worktrees`
/// and so no cascade, and drag-and-drop is the highest-concurrency gesture in a
/// two-window app while a document is the coarsest possible write grain — the
/// exact property v8's one-row-per-setting shape was chosen to get. The settings
/// store keeps the *scalars* this feature needs (`worktree.evictAfterDays`).
///
/// **Lane membership lives on the worktree row, and there is no join key.** Both
/// hazards this codebase has already paid for — rowid reuse (three stores got it
/// wrong in #201) and path reuse — exist because user state was stored *beside* a
/// worktree and had to point at it. Storing the assignment *on* the row it
/// describes removes the pointer, so a reused rowid cannot inherit a stale lane,
/// and a trashed worktree keeps its lane and position for the whole pending-removal
/// window for free.
///
/// **`lane` holds the name, not a surrogate id.** A rename is
/// `UPDATE lanes SET name` plus `UPDATE worktrees SET lane` — two statements, but
/// both tables live in this one database, so one IMMEDIATE transaction makes it
/// atomic and the usual "denormalising the name means a non-atomic N-row rewrite"
/// objection does not apply. Skipping the surrogate id also avoids reintroducing
/// rowid reuse in a brand-new table, which is what an `INTEGER PRIMARY KEY` on
/// `lanes` would have done.
///
/// **`sort_position` is NULL for "the user has not placed this one".** Unplaced
/// worktrees render as an alias-sorted tail, so the reconcile pass never has to
/// invent user intent when it discovers a checkout, and a new worktree can never
/// appear silently wedged into the middle of a hand-made order. With no lanes
/// defined and nothing placed, the rail order is byte-identical to v9's.
///
/// **Trash is a holding area, and `trashed_at` is its clock.**
///
/// Removing a worktree marks the row and **leaves the checkout on disk**. It shows
/// in the rail as trashed, restoring it is a real undo rather than a race, and the
/// actual `git worktree remove` happens when the retention period
/// (`worktree.trashRetentionDays`) expires, or when the user asks for it now. A
/// recycle bin, not a progress indicator on a deletion already underway.
///
/// That is what makes `trashed_at` a *timestamp* rather than a flag: it is the only
/// thing that says when the grace period started, so it is both the record of intent
/// and the retention clock.
///
/// The reconcile pass has to know about it. `git worktree list --porcelain` keeps
/// reporting the path for the whole retention window — the checkout is genuinely
/// still there — so without checking `trashed_at` the next poll would resurrect the
/// row it was just asked to trash.
///
/// **The removal is always git's, and always un-forced.** Relocating the checkout
/// instead would be O(1) and tempting, and it would bypass every safety check
/// `git worktree remove` exists to enforce while pulling the directory out from
/// under the PTY sessions, runs and browser panes rooted at that path — failing
/// immediately and invisibly rather than at removal time.
///
/// `trash_error` is the other half: a removal that fails after the user has moved
/// on takes the row back *out* of trash with the reason attached, because a
/// background job that fails silently is worse than a blocking one that reports.
fn migrate_v10_rail_lanes_and_trash(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        ALTER TABLE worktrees ADD COLUMN lane TEXT NOT NULL DEFAULT '';
        ALTER TABLE worktrees ADD COLUMN sort_position INTEGER;
        ALTER TABLE worktrees ADD COLUMN trashed_at TEXT NOT NULL DEFAULT '';
        ALTER TABLE worktrees ADD COLUMN trash_error TEXT NOT NULL DEFAULT '';

        CREATE TABLE lanes (
            repo_root  TEXT NOT NULL REFERENCES repos(root) ON DELETE CASCADE,
            name       TEXT NOT NULL,
            position   INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (repo_root, name)
        );
        "#,
    )
}

/// v11: the identity a config-declared terminal pane hands to the tool it runs.
///
/// One row per pane that has actually launched something. Three things follow
/// from that, and each one is the reason a column is or is not here:
///
/// - **The row's existence is the "has this ever launched" bit.** It is written
///   by the daemon after a holder spawns, so there is no `ever_started` column to
///   get out of step with reality — and no way for a client to claim a launch
///   that never happened, which is what would make `--resume <unknown-id>` the
///   pane's permanent state.
/// - **The token never leaves the daemon.** It is interpolated into the command
///   here and never serialised to a client, so no browser storage, no IPC
///   sanitizer and no detach payload can drop or corrupt it.
/// - **`ON DELETE CASCADE` is the whole GC story.** Worktree ids are rowids and
///   SQLite reuses them, so a row keyed on one outlives the worktree it named and
///   would eventually be adopted by an unrelated checkout. The FK deletes these
///   rows with the worktree rather than leaving that to a sweep nobody runs.
fn migrate_v11_pane_sessions(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE pane_sessions (
            session_id  TEXT PRIMARY KEY,
            worktree_id INTEGER NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
            spec_id     TEXT NOT NULL,
            token       TEXT NOT NULL,
            created_at  TEXT NOT NULL
        );
        CREATE INDEX idx_pane_sessions_worktree ON pane_sessions(worktree_id);
        "#,
    )
}

/// v12: this machine's answers for vars the config declared overridable.
///
/// **Not the `settings` table, whose `scope` column was added for this.** That
/// column anticipated per-project *preferences*, and the settings store around it
/// is built for what a preference is: a known key, a clamped scalar or a `one_of`
/// enum, validated on read against a `defaults()` table. An override is none of
/// those — its key is a var name only the project's config knows, its legal
/// values are a `choices` list only that config knows, and its value may be a
/// *pointer* (`{"env": …}`, `{"shell": …}`) rather than a scalar. Storing it there
/// would mean every row is an "unknown key" that the validator waves through, so
/// the validation the settings store exists to provide would apply to none of it.
/// Same reasoning that kept lanes out in v10; `settings.scope` stays unused.
///
/// **`project_id` is not a project root.** It is where this config lives in the
/// repo's *main* checkout (see `veld_core::project_id`), so every worktree of one
/// repo shares a row — the whole point, since an override describes the laptop
/// and not the checkout. A project root would ask again in every worktree.
///
/// **`scope` + `scope_key` is a two-level lattice, and `scope_key` is what makes
/// it one table.** A `project` row carries `scope_key = ''`; a `worktree` row
/// carries the canonicalized checkout directory, so the narrower answer is a row
/// rather than a second table. Reads take both and let `worktree` win.
///
/// **`value` is a serialized `ConfigValue`, not a string.** An override may be a
/// pointer — `veld config set token --shell 'op read op://…'` — which is what
/// lets a `secret: true` var be overridable without veld taking custody of the
/// secret. A plain scalar round-trips as a bare JSON string, so the common case
/// stays readable in `sqlite3`.
/// One JSON column holding a node's routed HTTP ports, keyed by port name.
///
/// A node used to own exactly one hostname, so `nodes.url` was the whole story
/// and teardown removed one DNS host and one Caddy route. With every
/// `protocol: "http"` port getting its own hostname, a node can own several, and
/// the stop path has to be able to find all of them from state alone — the
/// config may have changed since the run started, which is why the URL was
/// persisted in the first place.
///
/// Backfill is deliberately absent: existing rows keep `url` and get `'{}'`
/// here, and `NodeState::hostnames()` folds `url` back in, so a run started
/// before this migration still tears its single route down. Writing the old
/// `url` into `urls` would have to invent a port name for it, and the only
/// honest one ("http") is a guess about a config we no longer have.
fn migrate_v13_node_urls(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        ALTER TABLE nodes ADD COLUMN urls TEXT NOT NULL DEFAULT '{}';
        "#,
    )
}

fn migrate_v12_var_overrides(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE var_overrides (
            project_id TEXT NOT NULL,
            scope      TEXT NOT NULL,
            scope_key  TEXT NOT NULL,
            name       TEXT NOT NULL,
            value      TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (project_id, scope, scope_key, name)
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
    fn a_test_binary_never_resolves_to_the_real_user_database() {
        // The guard's own regression test, and it can only pass by being true of
        // the binary running it: this test executable lives under cargo's target
        // directory, so `default_path()` must hand back the dev database.
        //
        // This exists because the alternative was found the hard way. A `Db::open()`
        // that reached the session-spawn path let twelve PTY tests migrate the
        // developer's production database, and an older veld then refused to open
        // it at all (`NewerSchema`). A `#[cfg(test)]` panic could not have caught
        // it — `veld-core` is compiled without `cfg(test)` when `veld-daemon`'s
        // tests link it.
        let real = dirs::data_dir().map(|d| d.join("veld").join("veld.db"));
        // Assert against the unoverridden resolution, so a `VELD_DB_PATH` in the
        // environment cannot make this pass for the wrong reason.
        let resolved = Db::cargo_target_db()
            .expect("a cargo-built test binary must sit under a CACHEDIR.TAG-marked target dir");
        if let Some(real) = real {
            assert_ne!(
                resolved, real,
                "a test must not resolve to the user's real database"
            );
        }
        // And it must be *the* dev database the justfile defines, not a second one
        // only cargo knows about: `just dev-db-from-real` snapshots the real
        // database into `dev_db` so a migration can be exercised against
        // real-shaped data, and a guard pointing elsewhere would have quietly
        // excluded `cargo test` from the one tool built for that.
        // Beside the justfile's dev DB, in the same gitignored directory, but a
        // separate file — `cargo test` must not write the database a running
        // `just dev` daemon owns. Asserted by shape rather than by exact path so
        // the supported `CARGO_TARGET_DIR`-outside-the-tree configuration, which
        // takes the `veld-dev.db` fallback, does not fail this.
        let name = resolved.file_name().unwrap().to_str().unwrap();
        assert!(
            name == "veld-cargo.db" || name == "veld-dev.db",
            "unexpected dev database name {name}"
        );
        assert_ne!(
            resolved.file_name().unwrap(),
            "veld.db",
            "must not be the dev *instance*'s database — `just dev` owns that one"
        );
    }

    /// v12 against a real v11 database, built from the shipped bodies so the
    /// migration runs over rows that already exist rather than an empty file.
    ///
    /// The property worth pinning is that v12 is purely additive: it creates one
    /// table and touches nothing, so every pre-existing row and every foreign key
    /// survives. A machine that upgrades mid-project keeps its worktrees, its
    /// runs, and its settings, and simply gains somewhere to put an answer.
    #[test]
    fn v11_v12_upgrade_adds_var_overrides_and_disturbs_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("veld.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            migrate_v1_initial(&conn).unwrap();
            migrate_v2_node_stats(&conn).unwrap();
            migrate_v3_environments_and_runs(&conn).unwrap();
            migrate_v4_graph_snapshot(&conn).unwrap();
            migrate_v5_desktop_worktrees(&conn).unwrap();
            migrate_v6_worktree_emoji(&conn).unwrap();
            migrate_v7_detailed_process_stats(&conn).unwrap();
            migrate_v8_settings(&conn).unwrap();
            migrate_v9_worktree_marker_color(&conn).unwrap();
            migrate_v10_rail_lanes_and_trash(&conn).unwrap();
            migrate_v11_pane_sessions(&conn).unwrap();
            conn.pragma_update(None, "user_version", 11).unwrap();
            conn.execute_batch(
                r#"
                INSERT INTO repos (root, name, created_at)
                  VALUES ('/tmp/r', 'r', '2026-01-01T00:00:00.000000Z');
                INSERT INTO worktrees (repo_root, path, branch, alias, emoji, is_main, created_at)
                  VALUES ('/tmp/r', '/tmp/r', 'main', 'main', '🦊', 1,
                          '2026-01-01T00:00:00.000000Z');
                INSERT INTO settings (scope, key, value, updated_at)
                  VALUES ('global', 'terminal.detachGraceMinutes', '30',
                          '2026-01-01T00:00:00.000000Z');
                "#,
            )
            .unwrap();
        }

        let db = Db::open_at(&path).unwrap();
        assert_eq!(
            db.schema_version().unwrap(),
            MIGRATIONS.last().unwrap().version
        );

        let conn = db.lock();
        // Pre-existing rows are untouched…
        let worktrees: i64 = conn
            .query_row("SELECT COUNT(*) FROM worktrees", [], |r| r.get(0))
            .unwrap();
        assert_eq!(worktrees, 1);
        let setting: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'terminal.detachGraceMinutes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(setting, "30");
        // …the new table is empty rather than backfilled: an override is an
        // answer someone gave, and there is nothing to infer one from.
        let overrides: i64 = conn
            .query_row("SELECT COUNT(*) FROM var_overrides", [], |r| r.get(0))
            .unwrap();
        assert_eq!(overrides, 0);
        // …and the upgrade left the database internally consistent.
        let fk: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fk, 0, "the upgrade must not orphan a row");
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
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

    #[test]
    fn v8_v9_upgrade_preserves_worktrees_and_adds_the_settings_table() {
        // The "does my new migration survive an old DB" test, built the same way
        // the v3 one is: hand-build a genuine v7 database from the shipped
        // migration bodies, then open it through the normal path so v8 and v9 run
        // against real rows.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("veld.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            migrate_v1_initial(&conn).unwrap();
            migrate_v2_node_stats(&conn).unwrap();
            migrate_v3_environments_and_runs(&conn).unwrap();
            migrate_v4_graph_snapshot(&conn).unwrap();
            migrate_v5_desktop_worktrees(&conn).unwrap();
            migrate_v6_worktree_emoji(&conn).unwrap();
            migrate_v7_detailed_process_stats(&conn).unwrap();
            conn.pragma_update(None, "user_version", 7).unwrap();
            conn.execute_batch(
                r#"
                INSERT INTO repos (root, name, created_at)
                  VALUES ('/tmp/r', 'r', '2026-01-01T00:00:00.000000Z');
                INSERT INTO worktrees (repo_root, path, branch, alias, emoji, is_main, created_at)
                  VALUES ('/tmp/r', '/tmp/r', 'main', 'main', '🦊', 1,
                          '2026-01-01T00:00:00.000000Z');
                "#,
            )
            .unwrap();
        }

        let db = Db::open_at(&path).unwrap();
        assert_eq!(
            db.schema_version().unwrap(),
            MIGRATIONS.last().unwrap().version
        );

        // The existing row survives with its hand-picked glyph intact, and its
        // colour arrives as the unassigned sentinel rather than as hue 0 — a
        // DEFAULT of 0 would have silently claimed a real colour for every
        // upgraded row, and the backfill could never tell it from a choice.
        let (emoji, color): (String, String) = db
            .lock()
            .query_row(
                "SELECT emoji, marker_color FROM worktrees WHERE path = '/tmp/r'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(emoji, "🦊");
        assert_eq!(
            color, "",
            "pre-v9 rows are unassigned, not silently the first palette entry"
        );

        // The settings table exists and reads as empty — an upgraded install has
        // every default and no stored overrides.
        assert_eq!(
            db.settings().unwrap()["terminal.fontSize"],
            serde_json::Value::from(12)
        );
        let stored: i64 = db
            .lock()
            .query_row("SELECT count(*) FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 0);
    }

    #[test]
    fn v3_migration_preserves_rows_and_normalizes_statuses() {
        use crate::state::{EndReason, NodeStatus, RunStatus};

        // Build a genuine v2 database by hand (the shipped v1+v2 migrations),
        // then open it through the normal path so v3 runs against real data.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("veld.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            migrate_v1_initial(&conn).unwrap();
            migrate_v2_node_stats(&conn).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
            conn.execute_batch(
                r#"
                INSERT INTO projects (root, name) VALUES ('/tmp/p', 'proj');
                INSERT INTO runs (project_root, name, run_id, status, execution_order, created_at, stopped_at) VALUES
                  ('/tmp/p', 'dev',   'aaaaaaaa-0000-0000-0000-000000000001', 'running',    '["web:local"]', '2026-01-01T00:00:00.000000Z', NULL),
                  ('/tmp/p', 'old',   'aaaaaaaa-0000-0000-0000-000000000002', 'stopped',    '[]', '2026-01-01T00:00:00.000000Z', '2026-01-02T00:00:00.000000Z'),
                  ('/tmp/p', 'weird', 'aaaaaaaa-0000-0000-0000-000000000003', 'recovering', '[]', '2026-01-01T00:00:00.000000Z', NULL);
                INSERT INTO nodes (run_row, node_key, node_name, variant, status, pid)
                  VALUES (1, 'web:local', 'web', 'local', 'healthy', 4242);
                INSERT INTO node_stats (run_row, node_key, cpu_percent, memory_bytes, process_count, sampled_at)
                  VALUES (1, 'web:local', 1.5, 100, 1, '2026-01-01T00:00:01.000000Z');
                "#,
            )
            .unwrap();
            // Fresh timestamp — the v3 migration age-prunes log_lines before
            // indexing, so a fixed old date would be (correctly) deleted.
            conn.execute(
                "INSERT INTO log_lines (project_root, run_name, node, variant, stream, ts, line)
                 VALUES ('/tmp/p', 'dev', 'web', 'local', 'server', ?1, 'hello')",
                [ts_to_str(chrono::Utc::now())],
            )
            .unwrap();
        }

        let db = Db::open_at(&path).unwrap();
        let root = Path::new("/tmp/p");

        // The table rebuild must NOT cascade-wipe nodes/node_stats.
        let run = db.get_run(root, "dev").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.end_reason, None);
        assert_eq!(run.nodes["web:local"].pid, Some(4242));
        let stats: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM node_stats", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stats, 1, "node_stats must survive the rebuild");

        // Terminal rows get the matching end_reason; stopped_at → ended_at.
        let old = db.get_run(root, "old").unwrap().unwrap();
        assert_eq!(old.status, RunStatus::Stopped);
        assert_eq!(old.end_reason, Some(EndReason::Stopped));
        assert!(old.ended_at.is_some());

        // Out-of-set legacy statuses are normalized to a terminal state so
        // they can't sit outside both the live set and every reaper's gate.
        let weird = db.get_run(root, "weird").unwrap().unwrap();
        assert_eq!(weird.status, RunStatus::Stopped);
        assert!(
            weird
                .end_detail
                .unwrap()
                .message
                .unwrap()
                .contains("normalized")
        );

        // Legacy log rows (run_id NULL) stay readable via the name scope.
        let rows = db
            .tail_logs(root, "dev", &crate::db::LogFilter::default(), 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, "hello");
        // ...but are invisible under an instance scope, by design.
        let scoped = db
            .tail_logs(
                root,
                "dev",
                &crate::db::LogFilter {
                    run_id: Some("aaaaaaaa-0000-0000-0000-000000000001".into()),
                    ..Default::default()
                },
                10,
            )
            .unwrap();
        assert!(scoped.is_empty());

        // Node status parses through the rebuild.
        assert_eq!(run.nodes["web:local"].status, NodeStatus::Healthy);
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
