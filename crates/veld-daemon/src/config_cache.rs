//! Parsed project configs, reused while nothing they were read from has changed.
//!
//! The repo listing needs every worktree's config on every poll, and a parse is
//! not cheap: it reads and JSONC-parses each file, then walks the filesystem to
//! expand the `include` globs. With a few hundred worktrees that was seconds of
//! work per poll, for files that had not changed in hours — and with the default
//! `extensions.source = main`, main's config was parsed once more for every
//! other worktree in the repo.
//!
//! An entry is trusted only while every path the load looked at still has the
//! stamp it had then: the files it read, the directories the glob walk visited,
//! and the root file's directory (see [`LoadedConfig::watched`]). A new file a
//! glob would match appears as a change to its directory, so re-stamping those
//! paths is enough — and it is a `stat` each, where the load was a read, a parse
//! and a directory walk.
//!
//! **Only the listing reads through here.** Starting a run, running an extension
//! action and resolving a pane all parse the config fresh, so a stale entry could
//! at worst show an out-of-date rail — never act on a config that has changed.
//!
//! [`LoadedConfig::watched`]: veld_core::include::LoadedConfig::watched

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use tracing::debug;
use veld_core::config::VeldConfig;

/// More entries than this and the cache starts over. Bounds the memory held for
/// worktrees that have since been removed, which nothing else evicts; well past
/// any real worktree count, so a working set is never thrown away by it.
const MAX_ENTRIES: usize = 2048;

/// A load that depends on more paths than this is not cached.
///
/// Each one costs a stamp held for the life of the daemon and a `stat` on every
/// lookup, and the count comes from the repo's own `include` globs — a `**`
/// segment visits every directory under it, `node_modules` included. Past this,
/// re-stamping stops being cheaper than the walk it replaces, so the config is
/// simply parsed each time, as it was before there was a cache. (A listing still
/// parses a main checkout's config once, not once per worktree — see
/// `desktop::repo_view_blocking`.)
const MAX_WATCHED: usize = 4096;

/// How recently a path may have changed and still have its load cached.
///
/// Every path the load reads is stamped only after the parse has read it, so an
/// edit landing between the two would be recorded as the state the cached config
/// reflects — and then served until that path next changed. A load whose inputs
/// changed this close to it is therefore returned but not kept, and the next
/// lookup parses again. Generous against filesystems that store times in whole
/// seconds.
///
/// Judged on the later of mtime and ctime. mtime alone is whatever the writer
/// says: `cp -p`, `rsync -a` and `tar x` all backdate it, and such a write in the
/// window would pass as long settled. ctime is set by the kernel on any change.
/// The flip side is that a config whose inputs never sit still — a `**` glob over
/// a directory a build keeps writing into — is never cached and is parsed on
/// every lookup, which is what happened before there was a cache.
const SETTLE: Duration = Duration::from_secs(2);

/// What a path looked like, or `None` if it did not exist.
///
/// The inode as well as mtime and length: an editor that saves by writing a new
/// file and renaming it over the old one can keep both of those identical. The
/// ctime because a `chmod` changes nothing else, and it decides whether the file
/// can be read at all. The device because an inode number is only unique on one.
/// All three are Unix-only and read as 0 elsewhere, where mtime and length are
/// the whole stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    ino: u64,
    dev: u64,
    ctime: (i64, i64),
}

impl Stamp {
    /// When this path last changed by the kernel's account as well as the
    /// writer's: the later of mtime and (on Unix) ctime.
    fn changed(&self) -> Option<SystemTime> {
        let ctime = u64::try_from(self.ctime.0)
            .ok()
            .filter(|secs| *secs > 0)
            .map(|secs| {
                SystemTime::UNIX_EPOCH
                    + Duration::from_secs(secs)
                    + Duration::from_nanos(u64::try_from(self.ctime.1).unwrap_or(0))
            });
        match (self.modified, ctime) {
            (Some(m), Some(c)) => Some(m.max(c)),
            (m, c) => m.or(c),
        }
    }
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let (ino, dev, ctime) = {
        use std::os::unix::fs::MetadataExt;
        (meta.ino(), meta.dev(), (meta.ctime(), meta.ctime_nsec()))
    };
    #[cfg(not(unix))]
    let (ino, dev, ctime) = (0, 0, (0, 0));
    Some(Stamp {
        modified: meta.modified().ok(),
        len: meta.len(),
        ino,
        dev,
        ctime,
    })
}

struct Entry {
    stamps: Vec<(PathBuf, Option<Stamp>)>,
    /// `None` for a config that did not parse — cached like a success, so a broken
    /// file is not re-parsed on every poll until somebody fixes it.
    config: Option<VeldConfig>,
}

impl Entry {
    fn is_current(&self) -> bool {
        self.stamps.iter().all(|(path, was)| stamp(path) == *was)
    }

    /// Whether every input last changed more than [`SETTLE`] before `started`.
    ///
    /// Compares a filesystem's times with this machine's clock. An input dated in
    /// the future — a file server whose clock runs ahead, a tree unpacked from a
    /// host whose did — is never settled, so that config is parsed on every lookup
    /// as it was before this cache; logged, because that is otherwise invisible.
    fn settled(&self, started: SystemTime) -> bool {
        let cutoff = started
            .checked_sub(SETTLE)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.stamps.iter().all(|(path, s)| {
            let Some(changed) = s.as_ref().and_then(Stamp::changed) else {
                return true;
            };
            if changed > started {
                debug!(
                    path = %path.display(),
                    "config input is dated in the future; not caching its load"
                );
            }
            changed < cutoff
        })
    }
}

/// Entries are shared rather than owned so a lookup can re-stamp one **outside**
/// the lock: a `stat` is not always fast — a worktree on a network share that has
/// gone away can hang one for seconds — and held under this one lock it would
/// stall every other repo's listing with it.
static CACHE: LazyLock<Mutex<HashMap<PathBuf, Arc<Entry>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// [`veld_core::config::parse_config`], answered from the cache when every path
/// the last parse looked at is unchanged. `None` when the config does not parse.
///
/// Blocking — it stats, and on a miss reads and walks, the filesystem — so it
/// belongs on the blocking pool, as `desktop::repo_view` runs it.
pub fn parse_config(path: &Path) -> Option<VeldConfig> {
    lookup(path, SystemTime::now)
}

/// [`parse_config`] with the clock [`SETTLE`] is judged against passed in, so a
/// test can cache a fixture it has only just written.
fn lookup(path: &Path, clock: impl Fn() -> SystemTime) -> Option<VeldConfig> {
    let known = CACHE
        .lock()
        .expect("config cache poisoned")
        .get(path)
        .cloned();
    // An entry replaced while this one is being checked is fine: this one is
    // judged on its own stamps, and whichever is current answers correctly.
    if let Some(entry) = known.as_ref().filter(|e| e.is_current()) {
        return entry.config.clone();
    }
    // The parse itself runs unlocked, so one slow load does not serialise every
    // other worktree's lookup behind it. Two callers missing on the same path at
    // once both parse it, and each answers from its own load; of their entries,
    // the later insert wins, and whichever is kept is judged on its own stamps.
    let started = clock();
    let entry = load(path);
    let config = entry.config.clone();
    let too_many = entry.stamps.len() > MAX_WATCHED;
    if too_many || !entry.settled(started) {
        if too_many {
            debug!(
                path = %path.display(),
                watched = entry.stamps.len(),
                "config load depends on too many paths to cache; parsing it each time"
            );
        }
        let mut cache = CACHE.lock().expect("config cache poisoned");
        // Not kept. The entry this lookup found stale goes too — but only that one:
        // a concurrent caller may have put a current entry there since.
        if let Some(stale) = &known {
            if cache.get(path).is_some_and(|e| Arc::ptr_eq(e, stale)) {
                cache.remove(path);
            }
        }
        return config;
    }
    let mut cache = CACHE.lock().expect("config cache poisoned");
    if cache.len() >= MAX_ENTRIES && !cache.contains_key(path) {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), Arc::new(entry));
    config
}

/// Parse `path` and stamp everything the parse depended on.
fn load(path: &Path) -> Entry {
    // A failed load reports nothing it looked at. Its whole input is the root file
    // (included files never fail a load, they only add findings), and the
    // directory, where a second root spelling changes which file wins.
    let mut watched = vec![path.to_path_buf()];
    if let Some(dir) = path.parent() {
        watched.push(dir.to_path_buf());
    }
    let (watched, config) = match veld_core::config::parse_config_with_files(path) {
        Ok(loaded) => {
            watched.extend(loaded.watched);
            (watched, Some(loaded.config))
        }
        Err(_) => (watched, None),
    };
    let mut watched = watched;
    watched.sort();
    watched.dedup();
    Entry {
        stamps: watched
            .into_iter()
            .map(|p| {
                let s = stamp(&p);
                (p, s)
            })
            .collect(),
        config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str =
        r#"{"schemaVersion":"3","name":"demo","include":["apps/*/veld.node.json"],"nodes":{}}"#;

    fn node(name: &str) -> String {
        format!(
            r#"{{"nodes":{{"{name}":{{"default_variant":"local","variants":{{"local":{{"type":"command","argv":["true"]}}}}}}}}}}"#
        )
    }

    fn names(cfg: &VeldConfig) -> Vec<String> {
        let mut v: Vec<String> = cfg.nodes.keys().cloned().collect();
        v.sort();
        v
    }

    /// Look `root` up with [`SETTLE`] judged an hour from now, so a fixture written
    /// a moment ago is cached. Without this every test would exercise a miss, and a
    /// stamp that failed to notice a change would still pass.
    fn later(root: &Path) -> Option<VeldConfig> {
        lookup(root, || SystemTime::now() + Duration::from_secs(3600))
    }

    fn cached(root: &Path) -> bool {
        CACHE
            .lock()
            .unwrap()
            .get(root)
            .is_some_and(|e| e.is_current())
    }

    /// Each test uses its own directory, so the process-wide cache never hands
    /// one test another's entry.
    fn project(root_text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("veld.json");
        std::fs::write(&root, root_text).unwrap();
        std::fs::create_dir_all(dir.path().join("apps/web")).unwrap();
        std::fs::write(dir.path().join("apps/web/veld.node.json"), node("web")).unwrap();
        (dir, root)
    }

    #[test]
    fn an_unchanged_config_is_served_from_the_cache() {
        let (_dir, root) = project(ROOT);
        assert_eq!(names(&later(&root).unwrap()), ["web"]);
        assert!(cached(&root));
        assert_eq!(names(&later(&root).unwrap()), ["web"]);
    }

    #[test]
    fn a_load_whose_inputs_just_changed_is_not_kept() {
        let (dir, root) = project(ROOT);
        // Written now: a change this close to the parse may have landed after the
        // loader read the file, so the result must not be trusted next time.
        std::fs::write(dir.path().join("apps/web/veld.node.json"), node("site")).unwrap();
        assert_eq!(names(&parse_config(&root).unwrap()), ["site"]);
        assert!(!CACHE.lock().unwrap().contains_key(&root));
    }

    #[cfg(unix)]
    #[test]
    fn a_write_that_backdates_its_mtime_is_not_taken_as_settled() {
        let (dir, root) = project(ROOT);
        std::fs::write(dir.path().join("apps/web/veld.node.json"), node("site")).unwrap();
        // What `cp -p` or `rsync -a` does, applied to every input so that no mtime
        // gives the write away: each one now claims an hour of rest. Only ctime,
        // which the kernel sets and nothing here can move, still says otherwise.
        let past = SystemTime::now() - Duration::from_secs(3600);
        let mut stack = vec![dir.path().to_path_buf()];
        while let Some(p) = stack.pop() {
            if p.is_dir() {
                stack.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
            }
            std::fs::File::open(&p).unwrap().set_modified(past).unwrap();
        }
        assert_eq!(names(&parse_config(&root).unwrap()), ["site"]);
        assert!(!CACHE.lock().unwrap().contains_key(&root));
    }

    #[test]
    fn editing_an_included_file_is_seen() {
        let (dir, root) = project(ROOT);
        later(&root).unwrap();
        assert!(cached(&root));
        std::fs::write(dir.path().join("apps/web/veld.node.json"), node("site")).unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["site"]);
    }

    #[test]
    fn a_new_file_matching_a_glob_is_seen() {
        let (dir, root) = project(ROOT);
        later(&root).unwrap();
        assert!(cached(&root));
        // In a directory the glob has not seen before …
        std::fs::create_dir_all(dir.path().join("apps/api")).unwrap();
        std::fs::write(dir.path().join("apps/api/veld.node.json"), node("api")).unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["api", "web"]);
        // … and in one it has already probed for the name and found nothing.
        std::fs::create_dir_all(dir.path().join("apps/db")).unwrap();
        later(&root).unwrap();
        assert!(cached(&root));
        std::fs::write(dir.path().join("apps/db/veld.node.json"), node("db")).unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["api", "db", "web"]);
    }

    #[test]
    fn a_literal_include_whose_directory_does_not_exist_yet_is_seen() {
        let (dir, root) = project(
            r#"{"schemaVersion":"3","name":"demo","include":["services/worker/veld.node.json"],"nodes":{}}"#,
        );
        assert!(names(&later(&root).unwrap()).is_empty());
        assert!(cached(&root));
        std::fs::create_dir_all(dir.path().join("services/worker")).unwrap();
        std::fs::write(
            dir.path().join("services/worker/veld.node.json"),
            node("worker"),
        )
        .unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["worker"]);
    }

    #[test]
    fn a_new_file_deep_under_a_double_star_glob_is_seen() {
        let (dir, root) = project(
            r#"{"schemaVersion":"3","name":"demo","include":["apps/**/veld.node.json"],"nodes":{}}"#,
        );
        std::fs::create_dir_all(dir.path().join("apps/web/nested/deeper")).unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["web"]);
        assert!(cached(&root));
        std::fs::write(
            dir.path().join("apps/web/nested/deeper/veld.node.json"),
            node("deep"),
        )
        .unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["deep", "web"]);
    }

    #[test]
    fn a_second_root_spelling_appearing_is_seen() {
        let (dir, root) = project(ROOT);
        let before = later(&root).unwrap();
        assert!(cached(&root));
        let ambiguous = |c: &VeldConfig| {
            c.deferred_findings
                .iter()
                .any(|f| f.rule == "ambiguous-root-config")
        };
        assert!(!ambiguous(&before));
        std::fs::write(dir.path().join("veld.jsonc"), ROOT).unwrap();
        assert!(ambiguous(&later(&root).unwrap()));
    }

    #[test]
    fn removing_a_matched_file_is_seen() {
        let (dir, root) = project(ROOT);
        later(&root).unwrap();
        assert!(cached(&root));
        std::fs::remove_file(dir.path().join("apps/web/veld.node.json")).unwrap();
        assert!(names(&later(&root).unwrap()).is_empty());
    }

    #[test]
    fn a_broken_root_is_cached_as_none_and_a_fix_is_seen() {
        let (_dir, root) = project(ROOT);
        std::fs::write(&root, "{ not json").unwrap();
        assert!(later(&root).is_none());
        assert!(cached(&root));
        std::fs::write(&root, ROOT).unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["web"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_permission_change_alone_is_seen() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, root) = project(ROOT);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root can read a mode-000 file, so this only means something unprivileged.
        if std::fs::read(&root).is_ok() {
            return;
        }
        assert!(later(&root).is_none());
        assert!(cached(&root));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["web"]);
    }

    #[test]
    fn a_file_replaced_by_rename_is_seen() {
        let (dir, root) = project(ROOT);
        later(&root).unwrap();
        assert!(cached(&root));
        let fresh = dir.path().join("apps/web/.veld.node.json.tmp");
        std::fs::write(&fresh, node("renamed")).unwrap();
        std::fs::rename(&fresh, dir.path().join("apps/web/veld.node.json")).unwrap();
        assert_eq!(names(&later(&root).unwrap()), ["renamed"]);
    }

    #[test]
    fn a_load_watching_too_many_paths_is_not_kept() {
        let (dir, root) = project(
            r#"{"schemaVersion":"3","name":"demo","include":["apps/**/veld.node.json"],"nodes":{}}"#,
        );
        for i in 0..=MAX_WATCHED {
            std::fs::create_dir_all(dir.path().join(format!("apps/web/d{i}"))).unwrap();
        }
        assert_eq!(names(&later(&root).unwrap()), ["web"]);
        assert!(!CACHE.lock().unwrap().contains_key(&root));
    }
}
