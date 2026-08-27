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

pub mod backup;
pub(crate) mod feedback;
mod import;
mod kv;
mod layouts;
mod logs;
mod panes;
mod settings;
mod settings_catalog;
pub(crate) mod state;
mod stats;
mod var_overrides;
mod worktrees;

pub use kv::PromotionState;
pub use layouts::{LayoutRejected, LayoutWrite, MAX_LAYOUT_BYTES, PaneLayout};
pub use logs::{LogFilter, LogRow, LogStream, stream_is_per_node};
pub use panes::{PaneSession, mint_pane_token};
pub use settings::{
    BackupPrefs, ConfigSource, DEFAULT_BACKUP_INTERVAL_MINUTES, DEFAULT_BACKUP_KEEP,
    DEFAULT_BACKUP_KEEP_DAILY, DEFAULT_DETACH_GRACE_MINUTES,
    DEFAULT_KEEP_AWAKE_SHARING_ON_BATTERY_MINUTES, DEFAULT_KEEP_AWAKE_SHARING_ON_POWER_MINUTES,
    DEFAULT_SHARING_PEER_TTL_MINUTES, DEFAULT_SHARING_WEB_TTL_MINUTES, GitCreateSource,
    KeepAwakePrefs, LogTimeZone, MAX_KEEP_AWAKE_MINUTES, MAX_RUN_HISTORY_DAYS,
    MAX_SHARE_TTL_MINUTES, MIN_KEEP_AWAKE_MINUTES, MIN_SHARE_TTL_MINUTES, SettingKey, defaults,
    parse_search_template,
};
pub use settings_catalog::{
    CatalogEntry, CatalogGroup, Choice, Choices, Requires, RuntimeSource, SettingGroup, Spec,
    ValueShape, catalog, catalog_groups,
};
pub use var_overrides::{OverrideScope, VarOverride};
pub use worktrees::{
    DiscoveredWorktree, LaneRecord, MAX_LANE_NAME_LEN, MAX_ORDER_LEN, RepoRecord, WORKTREE_COLORS,
    WORKTREE_EMOJI, WorktreePatch, WorktreeRecord, default_alias, is_worktree_color,
    is_worktree_emoji,
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

    /// A known setting was given a value its validator refuses.
    ///
    /// `reason` carries what the refusing validator already knew and used to
    /// throw away. `browser.searchUrl` is the case that made this worth a field:
    /// [`settings::parse_search_template`](crate::db::parse_search_template)
    /// returns sentences like *"must not put %s in the host — it belongs in the
    /// path or query"*, and collapsing that to "not a valid value" left the CLI
    /// and the dialog telling a user their template was wrong without telling
    /// them how. `None` is the honest answer for the validators that genuinely
    /// have nothing to add (a bool that got a string).
    #[error("{value} is not a valid value for setting {key:?}{}", reason.as_deref().map(|r| format!(" — {r}")).unwrap_or_default())]
    InvalidSetting {
        key: String,
        value: String,
        reason: Option<String>,
    },

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

impl DbError {
    /// This error as it should be *shown to a person*, without this enum's own
    /// wrapper in front of it.
    ///
    /// [`DbError::Sqlite`]'s `Display` is `"database error: {0}"`, which is right
    /// for a log line and wrong everywhere the surrounding UI has already said
    /// what kind of thing went wrong. It shipped to a screen as
    /// *"database error: database disk image is malformed"*, under a heading
    /// reading **What SQLite said** — this crate's prefix presented as SQLite's
    /// words, with "database" twice in six.
    ///
    /// Lives here rather than in the consumer that noticed: a caller stripping a
    /// prefix it has to *know* this enum uses is a caller that silently stops
    /// working when the `#[error(...)]` attribute above is reworded.
    #[must_use]
    pub fn reported_message(&self) -> String {
        match self {
            // Only this variant's own prefix. `Open` names the path it could not
            // open and `CreateDir` the directory it could not make — both worth
            // keeping.
            DbError::Sqlite(e) => e.to_string(),
            other => other.to_string(),
        }
    }

    /// Whether this is SQLite refusing a row rather than failing.
    ///
    /// The one distinction a caller usually needs, and the one that is easy to
    /// get wrong by collapsing every error into the friendliest status: a
    /// foreign-key violation means the row this references is gone (a client
    /// racing a deletion), while a locked database, a full disk or a poisoned
    /// lock are this process's problem and must not be reported as "not found".
    /// Kept here so callers do not have to link `rusqlite` to ask.
    #[must_use]
    pub fn is_constraint_violation(&self) -> bool {
        matches!(
            self,
            DbError::Sqlite(rusqlite::Error::SqliteFailure(inner, _))
                if inner.code == rusqlite::ErrorCode::ConstraintViolation
        )
    }

    /// Whether this error says the *file* is in trouble, rather than the
    /// statement being wrong about a row.
    ///
    /// **Why the distinction is worth a method.** Everything in `db/` funnels
    /// through `DbError::Sqlite` via `#[from]`, so "the database is damaged" and
    /// "that alias is taken" arrive at callers as the same variant and get the
    /// same `warn!` — which is how a corrupted page produced 440 identical log
    /// lines over 17 hours while every subsystem carried on as if nothing had
    /// happened. A caller that wants to *escalate* needs to tell those apart
    /// without linking `rusqlite` itself.
    ///
    /// Both the blanket `Sqlite` variant and the call-site-classified
    /// [`DbError::Open`] are inspected: corruption is as likely to surface on
    /// the `PRAGMA journal_mode=WAL` inside [`Db::open_at`] as on a later query.
    #[must_use]
    pub fn fault(&self) -> Option<DbFault> {
        let source = match self {
            DbError::Sqlite(e) => e,
            DbError::Open { source, .. } => source,
            _ => return None,
        };
        let rusqlite::Error::SqliteFailure(inner, _) = source else {
            return None;
        };
        match inner.code {
            // `NotADatabase` is corruption too, and the more alarming shape of
            // it: the header itself no longer reads as SQLite. Grouped rather
            // than split because the answer for both is the same — this file
            // cannot be trusted, restore a backup.
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                Some(DbFault::Corrupt)
            }
            // Kept separate from corruption because the right response differs:
            // I/O errors and a full disk are usually the *precursor* to damage
            // and are often transient (an unmounted volume, a disk that filled
            // for a minute), so they are worth reporting and worth not treating
            // as "your database is broken, restore a backup".
            //
            // `ReadOnly` belongs here for a specific reason: a
            // volume that APFS (or the kernel) has remounted read-only after I/O
            // errors is *the* shape the real incident's precursor took, and it is
            // a state in which nothing can be written and every reconcile fails
            // forever. Left unclassified they returned `None`, which after this
            // change means no banner, no marker and no log escalation — strictly
            // less visible than the wrong label they used to produce.
            //
            // `CannotOpen` is deliberately **not** here, having been tried: SQLite
            // returns it for any failed `open(2)`, so file-descriptor exhaustion
            // in a daemon that holds PTYs and sockets, or a mode a past
            // `sudo veld` left unreadable, would have been reported to the user
            // as "your database is damaged".
            rusqlite::ErrorCode::SystemIoFailure
            | rusqlite::ErrorCode::DiskFull
            | rusqlite::ErrorCode::ReadOnly => Some(DbFault::Io),
            _ => None,
        }
    }
}

/// A fault that is about the database file, not about a row. See
/// [`DbError::fault`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DbFault {
    /// SQLite says the file is malformed — a damaged page, or a header that no
    /// longer reads as a database at all.
    Corrupt,
    /// The storage underneath refused: an I/O error or a full disk.
    Io,
}

impl DbFault {
    /// A short, stable word for logs, `--json` output and the wire.
    ///
    /// `const` so a caller can build a compile-time list from it rather than
    /// spelling these words a second time — see `dbhealth::NOTIFY_IDS`, where a
    /// list that drifted from this one would silently stop notifying.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DbFault::Corrupt => "corrupt",
            DbFault::Io => "io",
        }
    }
}

/// What [`Db::integrity`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    /// SQLite answered `ok`.
    Ok,
    /// SQLite reported damage, or could not complete the check because of it.
    /// Carries what it said, for a log line and for the IDE's detail panel.
    Damaged(String),
}

/// Where a failure to open the database gets reported, if anybody is listening.
///
/// **This exists because the alternative is a discipline, and the discipline kept
/// failing.** The daemon wants every fault recorded so it can tell the user, and
/// it opens this database from 27 different places — schedulers, request
/// handlers, startup warm-ups — several of which map the error straight to a
/// status and discard it. Wiring each call site was tried across three review
/// rounds and missed sites every time, including the one that logged 247 of the
/// 440 errors in the incident this reporting was built for.
///
/// A `fn` pointer rather than a boxed closure so this needs no allocation and no
/// lock on the read path, and `OnceLock` so it can only be installed once — a
/// second installer would silently replace the first.
static OPEN_OBSERVER: std::sync::OnceLock<fn(&DbError)> = std::sync::OnceLock::new();

/// Install the process's open-failure observer. Idempotent; later calls are
/// ignored rather than overwriting.
///
/// Called once by the daemon at startup. Deliberately not called by the CLI:
/// a one-shot command reports its own failure to the person who typed it.
pub fn observe_open_failures(hook: fn(&DbError)) {
    let _ = OPEN_OBSERVER.set(hook);
}

/// The bundled SQLite library's version, e.g. `"3.53.2"`.
///
/// Reported at daemon startup. veld links SQLite statically, so which one it is
/// cannot be discovered from the machine — and it is the first question any
/// corruption diagnosis asks, since sqlite.org's `howtocorrupt.html` is
/// organised by the versions each documented bug is present in.
#[must_use]
pub fn sqlite_version() -> &'static str {
    rusqlite::version()
}

/// Whether this process has already described a damaged file. See
/// [`log_corrupt_file_shape`].
static CORRUPT_SHAPE_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Legal SQLite page sizes (`PRAGMA page_size` accepts these and nothing else).
const SQLITE_PAGE_SIZES: [u64; 8] = [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536];

/// Describe the shape of a file that would not open, once per process.
///
/// The error itself already carries the path and SQLite's message, and that was
/// never the gap — `DbError::Open` has carried the path since the database was
/// created. What was missing is the three facts that separate the *recoverable*
/// shape of this failure from the unrecoverable ones, and they can only be read
/// from the bytes:
///
/// - **size**, and whether it is a whole multiple of a legal page size. A whole
///   multiple means nothing was truncated mid-page — the pages are all still
///   there and one of them is wrong. A size that divides by nothing legal means
///   the file was cut short or something else was written over it.
/// - **the first 16 bytes.** `SQLite format 3\0` means the header survived and
///   the damage is deeper in. A b-tree page type byte (`0x02`/`0x05`/`0x0a`/
///   `0x0d`) at offset 0 means another page's image landed on page 1 — the shape
///   that is fully reconstructible. ASCII means a text file was copied over it.
///
/// One incident (#332) sat undiagnosed for a day and then took a page-by-page
/// hand decode to establish exactly these three facts, all of which were a
/// `stat` and a 16-byte read away.
///
/// **Once per process, not once per open**: the daemon opens the database per
/// HTTP request and per scheduler pass, and a per-open version of this is a
/// flood in the log of the machine that can least afford one.
fn log_corrupt_file_shape(path: &Path) {
    use std::sync::atomic::Ordering;
    if CORRUPT_SHAPE_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    let shape = corrupt_file_shape(path);
    tracing::error!(
        path = %path.display(),
        size_bytes = shape.size_bytes,
        whole_pages_of = shape.whole_pages_of,
        first_16_bytes = %shape.first_16_bytes,
        "database will not open and reads as damaged — recording its shape once for diagnosis"
    );
}

/// The three facts [`log_corrupt_file_shape`] reports.
#[derive(Debug, PartialEq, Eq)]
struct CorruptFileShape {
    size_bytes: u64,
    /// The largest legal page size the file divides by, or `0` for none.
    whole_pages_of: u64,
    /// Space-separated lowercase hex of the first (up to) 16 bytes.
    first_16_bytes: String,
}

/// Read a damaged file's shape. Pure apart from the two reads, so the
/// classification is testable against fixtures rather than against an incident.
fn corrupt_file_shape(path: &Path) -> CorruptFileShape {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // Read this as "whole pages are plausible", never as a measurement: a file
    // with 4096-byte pages also divides by 512, and the header the real page
    // size lives in is exactly what is gone. `0` — divides by no legal page size
    // at all — is the *un*recoverable shape: something was truncated or written
    // over wholesale rather than one page landing at the wrong address.
    let whole_pages_of = SQLITE_PAGE_SIZES
        .iter()
        .rev()
        .copied()
        .find(|p| size != 0 && size % p == 0)
        .unwrap_or(0);

    let mut head = [0u8; 16];
    let read = {
        use std::io::Read;
        std::fs::File::open(path)
            .and_then(|mut f| f.read(&mut head))
            .unwrap_or(0)
    };
    let first_16_bytes = head[..read]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");

    CorruptFileShape {
        size_bytes: size,
        whole_pages_of,
        first_16_bytes,
    }
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
    /// no `[env]`, nothing in CI. (There is a `.cargo/config.toml` now — but it
    /// guards the *other* hole, an inherited `VELD_DB_PATH`, and it is not what
    /// makes this function necessary. Read the paragraph as history.) A test that built an axum router and drove one
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

    /// Whether this process resolves to the **installed user's** database —
    /// `<data_dir>/veld/veld.db` — rather than to a `VELD_DB_PATH` override or a
    /// cargo build's own [`cargo_target_db`].
    ///
    /// Extracted from [`Self::open`], which had this condition inline to decide
    /// whether the one-time legacy import should run. It earned a name when a
    /// second caller needed the same fact for a different reason: a `veld` CLI
    /// deciding whether the daemon on the default port is *its* daemon. Both
    /// questions are the same question — "is this the real one?" — and answering
    /// it in two places is how they would come to disagree.
    pub fn uses_installed_database() -> bool {
        Self::path_override().is_none() && Self::cargo_target_db().is_none()
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
        // Reported once, here, rather than at each of the caller's error arms —
        // see [`OPEN_OBSERVER`]. `open_at` is deliberately *not* hooked: it is
        // the tests' entry point, and a test that deliberately opens a damaged
        // fixture must not publish a fault into the process it shares with every
        // other test.
        let opened = (|| {
            let path = Self::default_path()?;
            Self::open_at(&path)
        })();
        let db = match opened {
            Ok(db) => db,
            Err(e) => {
                if let Some(observer) = OPEN_OBSERVER.get() {
                    observer(&e);
                }
                return Err(e);
            }
        };
        // Mirrors `default_path`'s precedence: only the genuine `<data_dir>`
        // database gets the import (an empty VELD_DB_PATH counts as unset there
        // too).
        if Self::uses_installed_database() {
            db.import_legacy_files_once();
        }
        Ok(db)
    }

    /// Open (and migrate) a database at an explicit path. Used by tests.
    pub fn open_at(path: &Path) -> Result<Self, DbError> {
        let opened = Self::open_at_inner(path);
        if let Err(ref e) = opened {
            // A file that will not open is the one moment its own bytes are the
            // only evidence there is, and the moment veld has historically said
            // least. See `log_corrupt_file_shape`.
            if e.fault() == Some(DbFault::Corrupt) {
                log_corrupt_file_shape(path);
            }
        }
        opened
    }

    fn open_at_inner(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DbError::CreateDir {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        // The file holds secrets (sensitive node outputs, relay tokens), so it
        // must be 0600. SQLite creates -wal/-shm with the same mode.
        //
        // **Never with an `open()` that is then closed.** sqlite.org's
        // `howtocorrupt.html` §2.2: "the close() system call will cancel all
        // POSIX advisory locks on the same file for all threads and all file
        // descriptors in the process … developers should be careful to never
        // use close() on an SQLite database file while one or more database
        // connections are open, even in other threads." The daemon opens this
        // database per HTTP request and per scheduler pass, so a `close(2)`
        // here silently drops the advisory locks every *other* live connection
        // in this process believes it still holds. Reproduced on APFS: with a
        // read transaction open, `fcntl(F_GETLK)` on SQLite's SHARED byte range
        // reports the lock held, and `F_UNLCK` immediately after an
        // `open()`+`close()` from the same process.
        //
        // The two paths are deliberately different syscalls and neither closes
        // a descriptor on a live inode:
        //
        // - **create**: `create_new` + `mode`, i.e. a brand-new inode nobody can
        //   hold a lock on yet. `AlreadyExists` means somebody else won the race
        //   and is handled by the existing-file path.
        // - **existing**: `chmod(2)`, no descriptor at all. Note this is a
        //   deliberate behaviour *change*: `OpenOptionsExt::mode` applies only
        //   when the file is created, so the old code corrected nothing on an
        //   existing database — it destroyed the process's advisory locks in
        //   exchange for no effect whatsoever. A pre-existing database with
        //   wrong permissions now actually gets tightened to 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
            {
                // Dropping *this* descriptor is safe: it is the only one that
                // has ever existed for this inode.
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
                }
                // Anything else (a missing parent, a read-only volume) is
                // reported by `Connection::open` below with better context.
                Err(_) => {}
            }
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

    /// The highest schema version this binary can open — the last entry in
    /// [`MIGRATIONS`], and the number [`DbError::NewerSchema`] reports as
    /// `supported`.
    ///
    /// Public because a backup artifact has to be judged *before* it is opened:
    /// `backup::restore` reads the candidate file's `user_version` straight out of
    /// its header and compares it against this, so a restore that would produce an
    /// unopenable database is refused rather than performed. Doing that after the
    /// move would mean discovering it with the live database already replaced.
    pub fn supported_schema_version() -> i64 {
        MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
    }

    /// Whether this database holds anything a person would miss.
    ///
    /// **The question a backup has to ask before it runs**, because `Db::open()`
    /// creates and migrates: on a machine whose `veld.db` was deleted, an empty one
    /// is minted within seconds by whichever daemon task opens it next, and backing
    /// *that* up produces an artifact that passes every check, wins the restore
    /// pick, and turns the `veld doctor` row green — the exact second failure of the
    /// incident the backup feature was built for.
    ///
    /// **Asked of every table rather than of a chosen few**, which is the part that
    /// was got wrong first. Naming `projects` and `repos` looked precise and refused
    /// real users: `projects` is derived from run rows and the GC deletes ended runs
    /// after seven days, while `repos` is only ever written by the Desktop worktree
    /// registry — so a CLI-only user who had not started a run for a week had
    /// neither, while their settings, relay tokens, lanes and pane layouts were all
    /// still there. The incident's own loss was settings rows.
    ///
    /// `kv` and the bulk tables are excluded: `kv` is where veld keeps its own
    /// bookkeeping (a first-use stamp, a GC timestamp) and is non-empty on a
    /// database nobody has touched, and the bulk tables are excluded from backups
    /// anyway, so a machine holding nothing but log lines has nothing to restore.
    ///
    /// Short-circuits on the first non-empty table, so the common answer costs one
    /// `EXISTS` query.
    pub fn holds_user_state(&self) -> Result<bool, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for name in names {
            if name == "kv" || backup::EXCLUDED_TABLES.contains(&name.as_str()) {
                continue;
            }
            let any: bool = conn.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM \"{}\")",
                    name.replace('"', "\"\"")
                ),
                [],
                |r| r.get(0),
            )?;
            if any {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The current schema version (`PRAGMA user_version`). For diagnostics.
    pub fn schema_version(&self) -> Result<i64, DbError> {
        let conn = self.lock();
        let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok(v)
    }

    /// Ask SQLite whether the live database is intact, as a value rather than
    /// as an error.
    ///
    /// **`quick_check`, not `integrity_check`, and the difference is the whole
    /// reason this is affordable.** Measured on a real 8.7 MB `veld.db`, the
    /// quick check costs ~15 ms — cheap enough for a timer — and it is
    /// sufficient for the fault class that matters here: a damaged page header
    /// is exactly what it reports. `integrity_check` additionally cross-checks
    /// every index against its table, which is the right tool for judging a
    /// *backup artifact* before restoring it ([`backup::inspect_deep`]) and the
    /// wrong one to put on a schedule.
    ///
    /// **Both failure shapes are damage.** A database that is intact answers
    /// with the single row `ok`; one that is not either answers with a
    /// description of what is wrong *or* fails the statement outright with
    /// `SQLITE_CORRUPT` (which is what the real incident did — the pragma could
    /// not read the page it needed to report on). Returning `Ok(Damaged)` for
    /// the first and `Err` for the second would make every caller handle the
    /// same condition twice, so the corrupt-shaped error is folded into
    /// `Damaged` here and only genuinely unrelated errors are returned as `Err`.
    pub fn integrity(&self) -> Result<Integrity, DbError> {
        let conn = self.lock();
        // **Every row, not the first.** The pragma emits one row *per finding*,
        // and `query_row` takes the first and discards the rest — which threw
        // away the trailing `database disk image is malformed` and any second
        // damaged page, in the one string the dialog labels "What SQLite said"
        // and `veld doctor` prints.
        let rows: Result<Vec<String>, _> = (|| {
            let mut stmt = conn.prepare("PRAGMA quick_check")?;
            let found = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, rusqlite::Error>(found)
        })();
        match rows {
            // A single `ok` row is SQLite's way of saying there were no findings.
            Ok(found) if found.len() == 1 && found[0].trim() == "ok" => Ok(Integrity::Ok),
            // No rows at all is not a clean bill of health — it is a pragma that
            // answered nothing, which no version does for an intact file.
            Ok(found) if found.is_empty() => Ok(Integrity::Damaged(
                "quick_check returned no answer".to_string(),
            )),
            Ok(found) => Ok(Integrity::Damaged(found.join("\n"))),
            Err(e) => {
                let e = DbError::Sqlite(e);
                match e.fault() {
                    Some(DbFault::Corrupt) => Ok(Integrity::Damaged(e.to_string())),
                    _ => Err(e),
                }
            }
        }
    }

    /// Pages `Db::vacuum` reclaims per call.
    ///
    /// A bare `PRAGMA incremental_vacuum` reclaims the **whole** freelist in one
    /// write transaction. On a pass that just deleted seven days of log rows
    /// that is tens of thousands of page relocations under a single lock, while
    /// every other veld process waits on its 10-second `busy_timeout` and
    /// `veld _log` drops lines when that expires.
    ///
    /// 2,000 pages is ~8 MB at the 4 KiB page size this database uses, so at the
    /// GC's 600-second interval a backlog drains at ~49 MB/hour — comfortably
    /// faster than veld frees pages in steady state, slower after a one-off mass
    /// delete. `incremental_vacuum` has always been resumable by design, so the
    /// remainder simply goes on the next pass.
    ///
    /// This is a lock-hold and churn bound. It is **not** a corruption fix: the
    /// documented `auto_vacuum` algorithm is excluded as a cause of #332 (it
    /// refuses to move a root page outright, and page 1 is never the source or
    /// the destination of a relocation).
    const VACUUM_PAGES_PER_PASS: u32 = 2_000;

    /// Reclaim disk space after large deletes: move a bounded number of freed
    /// pages out of the file (incremental vacuum) and truncate the WAL. Called
    /// by GC after pruning; best-effort.
    ///
    /// Returns the number of pages still on the freelist afterwards, so a caller
    /// can tell "nothing to do" from "more next pass".
    ///
    /// The reclaim is skipped entirely when the freelist is empty — the common
    /// case on a healthy pass, and previously an unconditional write transaction
    /// on every one. The gate is `freelist_count` and deliberately not the
    /// prune counts: `node_process_stats` prunes on a 2-hour horizon every pass,
    /// so "0 log rows pruned" is never "nothing was freed".
    pub fn vacuum(&self) -> Result<u32, DbError> {
        self.vacuum_pages(Self::VACUUM_PAGES_PER_PASS)
    }

    /// [`Db::vacuum`] with an explicit page budget, so the *bound* is testable
    /// without writing 8 MB of fixture to exceed the production one.
    pub(crate) fn vacuum_pages(&self, max_pages: u32) -> Result<u32, DbError> {
        let conn = self.lock();
        let freelist: u32 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        if freelist > 0 {
            conn.execute_batch(&format!("PRAGMA incremental_vacuum({max_pages});"))?;
        }
        // wal_checkpoint returns a result row — use query_row. Run even with an
        // empty freelist: the WAL grows from ordinary writes, and truncating it
        // is the cheaper half of this call.
        let _: (i64, i64, i64) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        let remaining: u32 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        Ok(remaining)
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
/// A new table added here is **backed up automatically**: [`backup`] enumerates
/// tables from `sqlite_master` minus a denylist, so nothing has to be remembered
/// for the common case. The one case that does need a thought is a *high-volume*
/// table — logs, samples, blobs — which belongs in
/// [`backup::EXCLUDED_TABLES`](backup::EXCLUDED_TABLES) or every backup starts
/// carrying it. Written here because this is where somebody adding one is
/// looking.
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
        name: "worktree-display-name",
        apply: migrate_v13_worktree_display_name,
    },
    Migration {
        version: 14,
        name: "node-endpoints",
        apply: migrate_v14_node_endpoints,
    },
    Migration {
        version: 15,
        name: "pane-layouts",
        apply: migrate_v15_pane_layouts,
    },
    Migration {
        version: 16,
        name: "repo-sort-position",
        apply: migrate_v16_repo_sort_position,
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

/// One JSON column holding a node's named ports, keyed by port name.
///
/// A node used to own exactly one hostname, so `nodes.url` was the whole story
/// and teardown removed one DNS host and one Caddy route. With every
/// `protocol: "http"` port getting its own hostname, a node can own several, and
/// the stop path has to be able to find all of them from state alone — the
/// config may have changed since the run started, which is why the URL was
/// persisted in the first place.
///
/// **Backfill is deliberately absent**, and the reason is that it could not be
/// honest: writing the old `url` into `endpoints` has to invent a port name, and
/// the only candidate ("http") is a guess about a config that is no longer on
/// disk. Existing rows keep `url` and get `'{}'` here.
///
/// That makes an empty map ambiguous — "no ports" and "ports not recorded" look
/// alike — so **every consumer that decides what a node can do must read
/// [`crate::state::NodeState::endpoints_or_legacy`]**, which folds `url`/`port`
/// back in as the single primary entry such a row always had. Reading the raw
/// map is correct only where the answer is "what did this run record", never
/// "what does this node have". `veld update` does not stop running environments,
/// so rows like these outlive the upgrade by days.
fn migrate_v14_node_endpoints(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        ALTER TABLE nodes ADD COLUMN endpoints TEXT NOT NULL DEFAULT '{}';
        "#,
    )
}

/// v13: `worktrees.display_name` — the free-text name the rail renders.
///
/// The alias is an *identifier*: it defaults the run name, which feeds the
/// hostname `{service}.{run}.{project}.localhost`, so it is bounded to
/// `[A-Za-z0-9._-]` and can hold neither a space nor a capital the user meant.
/// Before this column the create dialog slugged what you typed and then showed
/// you the slug forever — "Hello test" went in and `hello-test` was the only
/// name that existed anywhere.
///
/// Two columns rather than relaxing the alias, because the two have genuinely
/// different rules: one has to survive DNS, the other only has to be readable in
/// a 236px column. And rather than deriving the label back out of the slug,
/// because that derivation is not invertible — capitals, punctuation and
/// non-ASCII are gone by then.
///
/// `''` means "no separate name, render the alias", which is what every
/// pre-existing row gets and what clearing the field returns a row to. It is a
/// sentinel and not a NULL for the same reason `emoji` and `marker_color` are:
/// `wt_from_row` reads a `String`, and one nullable column in that struct would
/// make every reader handle an absence that means the same thing as `''`.
fn migrate_v13_worktree_display_name(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("ALTER TABLE worktrees ADD COLUMN display_name TEXT NOT NULL DEFAULT '';")
}

/// v15: the panes a worktree is showing — one row per worktree, not per window.
///
/// This is where a layout stopped being browser state. It lived in
/// `sessionStorage` plus two `localStorage` keys, which made "the panes of
/// worktree 7" a different set in every client: a browser tab opening the same
/// worktree as the desktop app saw a fresh, empty layout, and because the
/// tab could not know the terminal ids the app was using, it spawned a *second*
/// set of shells rather than re-attaching to the running ones. There is one set
/// of panes per worktree, so there is one row.
///
/// **`layout` is opaque to the daemon and that is the design, not laziness.**
/// The inner shape is the UI's `PaneLayout` — docks, tabs, per-kind payloads
/// (`url`, `profile`, `emulation`, `media`, `zoom`) — and it grows a field
/// every time a pane kind does. Nothing in Rust reads inside it, so a new pane
/// kind is a UI-only change instead of a migration, and an *older* daemon
/// round-trips a newer client's layout instead of erasing the parts it does not
/// understand. The client validates on the way in (`parseLayouts`), which is
/// where the knowledge is. The one property the daemon does enforce is that it
/// is syntactically JSON — see `Db::put_pane_layout`.
///
/// **`version` is the whole concurrency story.** A write states the version it
/// read; a mismatch is refused rather than merged. Contention is rare by
/// construction — one client shows a worktree at a time (the claim registry in
/// `veld-daemon`'s `ide` module) — so this is a hand-off guard, not a merge
/// engine: the window that has just been granted a worktree must not lose panes
/// to a debounced write still in flight from the window that let it go.
///
/// **`ON DELETE CASCADE`, for the reason v11 spells out**: `worktrees.id` is a
/// rowid and SQLite reuses the highest free one, so a layout row that outlived
/// its worktree would be adopted by the next checkout created — handing it a
/// set of panes, and terminal session ids, from a worktree that no longer
/// exists.
fn migrate_v15_pane_layouts(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE pane_layouts (
            worktree_id INTEGER PRIMARY KEY REFERENCES worktrees(id) ON DELETE CASCADE,
            version     INTEGER NOT NULL,
            layout      TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );
        "#,
    )
}

/// v16: manual project order, for the IDE's project column.
///
/// The same shape v10 gave worktrees and lanes, one level up: **`NULL` means the
/// user has not placed this project**, and an unplaced row sorts to a
/// name-ordered tail rather than to position 0. A `NOT NULL DEFAULT 0` would have
/// declared every existing project deliberately placed at the front, in an order
/// nobody chose, and there would be no way to tell that from a real choice
/// afterwards.
///
/// Nullable is also what makes the migration a no-op for existing users: every
/// row keeps sorting by name until somebody drags something, which is exactly
/// what they saw before this column existed.
fn migrate_v16_repo_sort_position(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("ALTER TABLE repos ADD COLUMN sort_position INTEGER;")
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

/// Overwrite one table's root page with rubbish, reproducing the fault class of
/// the incident this module's health reporting was built for: a single damaged
/// page, with the rest of the database perfectly readable.
///
/// The page number comes from `sqlite_schema.rootpage` for `table_name`, and
/// pages are 1-indexed, hence the `- 1`. The connection must be closed before
/// calling this (SQLite caches pages, so a live handle would keep answering from
/// memory and the test would assert nothing).
#[cfg(test)]
pub(crate) fn corrupt_table_page(path: &Path, table_name: &str) {
    use std::io::{Seek, SeekFrom, Write};

    let (page_size, rootpage): (i64, i64) = {
        let conn = Connection::open(path).unwrap();
        let page_size = conn
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .unwrap();
        let rootpage = conn
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE name = ?1",
                [table_name],
                |r| r.get(0),
            )
            .unwrap();
        (page_size, rootpage)
    };

    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.seek(SeekFrom::Start(((rootpage - 1) * page_size) as u64))
        .unwrap();
    f.write_all(&[0xff; 200]).unwrap();
    f.sync_all().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file whose size is a whole number of pages is the *recoverable* shape —
    /// nothing was truncated, one page is simply wrong — and one that divides by
    /// no legal page size is not. Getting that classification the wrong way
    /// round is the only way this reporting can mislead, so it is asserted
    /// rather than reasoned about.
    #[test]
    fn a_damaged_files_shape_separates_whole_pages_from_a_truncated_file() {
        let dir = tempfile::tempdir().unwrap();

        // 59,759 × 4096 was the real incident's size; three pages is the same
        // fact at a size a test can write.
        let whole = dir.path().join("whole.db");
        std::fs::write(&whole, vec![0x0du8; 3 * 4096]).unwrap();
        let shape = corrupt_file_shape(&whole);
        assert_eq!(shape.size_bytes, 12_288);
        assert_eq!(
            shape.whole_pages_of, 4096,
            "12288 divides by 512/1024/2048/4096 — the largest is what gets reported, \
             because a smaller divisor is true of every file the larger one is true of"
        );

        // Cut one byte off and no legal page size divides it any more.
        let cut = dir.path().join("cut.db");
        std::fs::write(&cut, vec![0u8; 3 * 4096 - 1]).unwrap();
        assert_eq!(
            corrupt_file_shape(&cut).whole_pages_of,
            0,
            "a size that divides by no legal page size must report 0, not a fallback"
        );

        // An empty file divides by everything arithmetically and by nothing
        // usefully; 0 is the honest answer.
        let empty = dir.path().join("empty.db");
        std::fs::write(&empty, b"").unwrap();
        let shape = corrupt_file_shape(&empty);
        assert_eq!((shape.size_bytes, shape.whole_pages_of), (0, 0));
        assert_eq!(shape.first_16_bytes, "");
    }

    /// The 16 bytes are the fact that separates the three failure shapes, so
    /// they have to be readable as bytes — not summarised, not interpreted.
    #[test]
    fn a_damaged_files_first_bytes_are_reported_verbatim() {
        let dir = tempfile::tempdir().unwrap();

        // An intact header: the damage is deeper in, and the page-1 story does
        // not apply.
        let headered = dir.path().join("headered.db");
        std::fs::write(&headered, b"SQLite format 3\0rest").unwrap();
        assert_eq!(
            corrupt_file_shape(&headered).first_16_bytes,
            "53 51 4c 69 74 65 20 66 6f 72 6d 61 74 20 33 00"
        );

        // The incident's own first bytes: a `0x0d` table-b-tree leaf sitting
        // where the file header belongs — another page's image at page 1's
        // address, which is fully reconstructible.
        let leaf = dir.path().join("leaf.db");
        std::fs::write(
            &leaf,
            [
                0x0d, 0x00, 0x00, 0x00, 0x02, 0x0f, 0xd5, 0x00, 0x0f, 0xef, 0x0f, 0xd5, 0, 0, 0, 0,
            ],
        )
        .unwrap();
        assert!(
            corrupt_file_shape(&leaf)
                .first_16_bytes
                .starts_with("0d 00"),
            "a b-tree page type byte at offset 0 is the recoverable shape and must be visible"
        );

        // Fewer than 16 bytes must not panic or pad.
        let tiny = dir.path().join("tiny.db");
        std::fs::write(&tiny, [0xffu8, 0x01]).unwrap();
        assert_eq!(corrupt_file_shape(&tiny).first_16_bytes, "ff 01");
    }

    /// Opening this database must not `close(2)` a descriptor on the live file:
    /// sqlite.org's `howtocorrupt.html` §2.2 is explicit that doing so cancels
    /// *every* POSIX advisory lock the process holds on it, across all threads
    /// and all descriptors, while the SQLite connections carry on believing they
    /// still hold theirs. The daemon opens this database per HTTP request and per
    /// scheduler pass, so the second open is the normal case, not a corner one.
    ///
    /// Asserted through SQLite's own SHARED byte range rather than through
    /// behaviour, because there is no behaviour to observe: the locks are gone
    /// and nothing notices until something else does.
    #[cfg(unix)]
    #[test]
    fn a_second_open_leaves_the_first_connections_locks_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.db");

        let first = Db::open_at(&path).unwrap();
        // A read transaction is what makes SQLite take the lock at all — with no
        // statement in flight there is nothing for a stray `close` to destroy,
        // which is how this defect stayed invisible.
        let guard = first.lock();
        let mut stmt = guard.prepare("SELECT count(*) FROM sqlite_schema").unwrap();
        let mut rows = stmt.query([]).unwrap();
        rows.next().unwrap();

        assert!(
            posix_lock_held(&path),
            "precondition: an open read transaction must hold the SHARED range — if this \
             fails the test proves nothing about the second open"
        );

        // The whole point: this runs the create/chmod path again on a file that
        // already exists, in the same process.
        let _second = Db::open_at(&path).unwrap();

        assert!(
            posix_lock_held(&path),
            "a second Db::open_at in the same process destroyed the first connection's \
             advisory lock — see howtocorrupt.html §2.2"
        );

        drop(rows);
        drop(stmt);
        drop(guard);
    }

    /// The deliberate behaviour change that comes with dropping the stray
    /// `open()`: `OpenOptionsExt::mode` applies **only when the file is
    /// created**, so the old code corrected nothing on an existing database — it
    /// destroyed the process's advisory locks in exchange for no effect at all.
    /// `chmod(2)` actually tightens one, which is what the file holding relay
    /// tokens and sensitive node outputs needed all along.
    #[cfg(unix)]
    #[test]
    fn opening_an_existing_database_tightens_its_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.db");

        // Create it, then loosen it the way a bad umask or a restored copy would.
        drop(Db::open_at(&path).unwrap());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        drop(Db::open_at(&path).unwrap());

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "an existing database with group/world-readable permissions must be tightened \
             on open — it stores relay tokens and sensitive node outputs"
        );
    }

    /// A database veld creates is never briefly world-readable.
    #[cfg(unix)]
    #[test]
    fn a_new_database_is_created_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.db");
        drop(Db::open_at(&path).unwrap());

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a freshly created database must be 0600");
    }

    /// Whether this process holds a lock on SQLite's SHARED byte range.
    ///
    /// **Probed from a forked child, and it has to be.** POSIX record locks are
    /// per (process, inode) and a process never conflicts with itself, so an
    /// `F_GETLK` issued from *this* process reports `F_UNLCK` whether the lock is
    /// there or not — the test would pass identically against the defect it
    /// exists to catch. Locks are not inherited across `fork`, so the child sees
    /// the parent's lock as a genuine conflict.
    #[cfg(unix)]
    fn posix_lock_held(path: &Path) -> bool {
        use nix::libc;
        use nix::sys::wait::{WaitStatus, waitpid};
        use nix::unistd::{ForkResult, fork};

        // SQLite's SHARED range: `SHARED_FIRST`/`SHARED_SIZE` in `os_unix.c`,
        // i.e. `PENDING_BYTE + 2` for 510 bytes.
        const SHARED_FIRST: i64 = 0x4000_0002;
        const SHARED_SIZE: i64 = 510;

        // Built in the parent: `CString::new` allocates, which the child must not.
        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();

        // SAFETY: the child calls only `open`, `fcntl` and `_exit` — all
        // async-signal-safe — before exiting, which is what makes `fork` legal
        // from a multi-threaded test binary. It allocates nothing and unwinds
        // nowhere.
        match unsafe { fork() }.expect("fork failed") {
            ForkResult::Child => {
                let code = unsafe {
                    let fd = libc::open(c_path.as_ptr(), libc::O_RDWR);
                    if fd < 0 {
                        2
                    } else {
                        let mut fl: libc::flock = std::mem::zeroed();
                        fl.l_type = libc::F_WRLCK as libc::c_short;
                        fl.l_whence = libc::SEEK_SET as libc::c_short;
                        fl.l_start = SHARED_FIRST as libc::off_t;
                        fl.l_len = SHARED_SIZE as libc::off_t;
                        if libc::fcntl(fd, libc::F_GETLK, &mut fl) != 0 {
                            2
                        } else if fl.l_type == libc::F_UNLCK as libc::c_short {
                            1
                        } else {
                            0
                        }
                    }
                };
                unsafe { libc::_exit(code) }
            }
            ForkResult::Parent { child } => match waitpid(child, None).expect("waitpid failed") {
                WaitStatus::Exited(_, 0) => true,
                WaitStatus::Exited(_, 1) => false,
                other => panic!("lock probe child failed: {other:?}"),
            },
        }
    }

    /// The classifier earns its keep only if it separates "this file is in
    /// trouble" from the constraint violations that arrive as the same variant.
    #[test]
    fn a_refused_row_is_not_a_fault() {
        let (_dir, db) = test_db();
        db.upsert_repo(Path::new("/tmp/r"), "r").unwrap();
        // A foreign key that names nothing: SQLite refuses the row, which is
        // emphatically not a damaged database.
        let refused = {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO worktrees (repo_root, path, branch, alias, is_main, created_at)
                 VALUES ('/tmp/nope', '/tmp/w', 'main', 'main', 0, '2026-01-01T00:00:00.000000Z')",
                [],
            )
            .unwrap_err()
        };
        let err = DbError::Sqlite(refused);
        assert!(err.is_constraint_violation());
        assert_eq!(err.fault(), None, "a refused row is not a file fault");
    }

    /// A message shown under a heading reading "What SQLite said" must be
    /// SQLite's words, not this enum's `Display` wrapper. It shipped to a screen
    /// as `database error: database disk image is malformed`.
    #[test]
    fn a_reported_message_drops_this_enums_own_prefix() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("veld.db");
        {
            let db = Db::open_at(&path).unwrap();
            db.upsert_repo(Path::new("/tmp/r"), "r").unwrap();
        }
        corrupt_table_page(&path, "pane_layouts");
        let db = Db::open_at(&path).unwrap();
        let err = {
            let conn = db.lock();
            DbError::Sqlite(
                conn.query_row("SELECT COUNT(*) FROM pane_layouts", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_err(),
            )
        };

        assert!(
            err.to_string().starts_with("database error: "),
            "if this stops being true the method below has nothing to do — and the \
             point is that its caller no longer has to know either way: {err}"
        );
        let shown = err.reported_message();
        assert!(
            !shown.starts_with("database error: "),
            "the wrapper must be gone: {shown:?}"
        );
        assert!(
            shown.contains("malformed"),
            "…and SQLite's own words must survive: {shown:?}"
        );
    }

    /// Every other variant is shown exactly as it reads: `Open` names the path it
    /// could not open, and losing that would cost the reader the useful half.
    #[test]
    fn a_reported_message_leaves_every_other_variant_alone() {
        for e in [
            DbError::AliasTaken("main".into()),
            DbError::RefusingMainWorktree,
            DbError::NoDataDir,
        ] {
            assert_eq!(e.reported_message(), e.to_string());
        }
    }

    /// The whole point: a damaged page must come back as [`DbFault::Corrupt`]
    /// through the same `DbError::Sqlite` variant every ordinary failure uses.
    #[test]
    fn a_damaged_page_is_classified_as_corruption() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("veld.db");
        {
            let db = Db::open_at(&path).unwrap();
            db.upsert_repo(Path::new("/tmp/r"), "r").unwrap();
        }
        corrupt_table_page(&path, "pane_layouts");

        let db = Db::open_at(&path).unwrap();
        let err = {
            let conn = db.lock();
            conn.query_row("SELECT COUNT(*) FROM pane_layouts", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_err()
        };
        assert_eq!(DbError::Sqlite(err).fault(), Some(DbFault::Corrupt));
    }

    /// `Db::open()` succeeding is not evidence of a healthy database — the
    /// property that made `veld doctor` print "Database OK" throughout a
    /// 17-hour incident. `integrity()` is what actually asks.
    #[test]
    fn integrity_sees_damage_that_opening_the_database_does_not() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("veld.db");
        {
            let db = Db::open_at(&path).unwrap();
            db.upsert_repo(Path::new("/tmp/r"), "r").unwrap();
            assert_eq!(db.integrity().unwrap(), Integrity::Ok);
        }
        corrupt_table_page(&path, "pane_layouts");

        // Opening still works, and so does every read that does not traverse
        // the damaged page. This is the incident in two assertions.
        let db = Db::open_at(&path).expect("a damaged database still opens");
        assert_eq!(
            db.list_repos().unwrap().len(),
            1,
            "an untouched table still reads"
        );

        match db.integrity().unwrap() {
            Integrity::Damaged(detail) => assert!(
                !detail.trim().is_empty(),
                "damage must come with something to show the user"
            ),
            Integrity::Ok => panic!("quick_check must not call a damaged database intact"),
        }
    }

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

    /// A cargo build must never claim the installed user's database — which is
    /// what tells a CLI that the daemon on the default port is *not* its own.
    ///
    /// The sibling of the test below, at the level the mistake actually happened.
    /// `default_path()` already keeps a cargo-built binary's *reads* off the real
    /// database; it says nothing about a **write sent over HTTP**, and
    /// `veld settings set` sends one. During #306's own smoke test a cargo-built
    /// `veld` wrote the developer's real settings through the installed daemon
    /// while reading `.veld-dev/veld-cargo.db`, so the set reported success and
    /// the matching get reported the old value. `Db::uses_installed_database` is
    /// the predicate `veld settings` consults before it will talk to a daemon at
    /// all, and this asserts it is false for exactly the binary that must not.
    #[test]
    fn a_cargo_built_binary_never_claims_the_installed_database() {
        // Both predicates resolve through `HOME`, which `console.rs`'s tests
        // repoint at a tempdir — under this same guard, so taking it here is
        // what makes that exclusion real rather than one-sided.
        let _guard = crate::test_support::process_state_guard();
        assert!(
            Db::cargo_target_db().is_some(),
            "a cargo-built test binary must sit under a CACHEDIR.TAG-marked target dir"
        );
        assert!(
            !Db::uses_installed_database(),
            "a cargo build claimed the installed database — a CLI would then write \
             the real one through the installed daemon while reading the dev one"
        );
    }

    #[test]
    fn a_test_binary_never_resolves_to_the_real_user_database() {
        // `dirs::data_dir()` below reads `HOME` — see the sibling test above for
        // why that means taking the crate-wide process-state guard.
        let _guard = crate::test_support::process_state_guard();
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

    /// v14 against a real v13 database holding a node row from before per-port
    /// endpoints — the row every machine that upgrades with an environment
    /// running will have, since `veld update` deliberately does not stop them.
    ///
    /// Two properties, and the second is the one that matters: the ALTER leaves
    /// the row alone (no backfill, `endpoints = '{}'`), **and** the node still
    /// reads back as a node with one endpoint, because `endpoints_or_legacy`
    /// folds `url`/`port` in. Reading the raw map instead is what made `veld
    /// share` refuse a whole run as having nothing to share.
    #[test]
    fn v13_v14_upgrade_leaves_a_legacy_node_shareable() {
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
            migrate_v12_var_overrides(&conn).unwrap();
            migrate_v13_worktree_display_name(&conn).unwrap();
            conn.pragma_update(None, "user_version", 13).unwrap();
            conn.execute_batch(
                r#"
                INSERT INTO projects (root, name) VALUES ('/tmp/p', 'p');
                INSERT INTO environments (project_root, name, created_at)
                  VALUES ('/tmp/p', 'dev', '2026-01-01T00:00:00.000000Z');
                INSERT INTO runs (id, environment_id, run_id, status, created_at)
                  VALUES (1, 1, '11111111-1111-4111-8111-111111111111', 'running',
                          '2026-01-01T00:00:00.000000Z');
                INSERT INTO nodes (run_row, node_key, node_name, variant, status, pid, port, url)
                  VALUES (1, 'web:local', 'web', 'local', 'healthy', 4242, 3000,
                          'https://web.dev.p.localhost');
                "#,
            )
            .unwrap();
        }

        let db = Db::open_at(&path).unwrap();
        assert_eq!(
            db.schema_version().unwrap(),
            MIGRATIONS.last().unwrap().version
        );

        // The column exists and the row was not backfilled.
        {
            let conn = db.lock();
            let raw: String = conn
                .query_row(
                    "SELECT endpoints FROM nodes WHERE node_key = 'web:local'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(raw, "{}", "v14 must not invent a port name");
            let integrity: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
        }

        // And the node is still a node with an endpoint, which is what every
        // consumer that decides what it can do must see.
        let state = db.load_project_state(Path::new("/tmp/p")).unwrap();
        let node = &state.get_run("dev").expect("run survives").nodes["web:local"];
        assert!(node.endpoints.is_empty(), "nothing was backfilled");
        let folded = node.endpoints_or_legacy();
        assert_eq!(folded.len(), 1, "the legacy row still has its one port");
        let ep = &folded["http"];
        assert_eq!(ep.hostname, "web.dev.p.localhost");
        assert_eq!(ep.url.as_deref(), Some("https://web.dev.p.localhost"));
        assert_eq!(ep.port, 3000);
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
