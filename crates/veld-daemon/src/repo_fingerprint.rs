//! A cheap fingerprint of everything the repo rail's read-only git questions
//! depend on, so the poll can skip spawning git when none of it moved.
//!
//! The IDE polls `refresh_repos` every 5 s, and per repo that spawned
//! `git worktree list`, `git for-each-ref`, `git rev-list --count` and
//! `git log -1` — four process spawns (a few tens of milliseconds each, more on a
//! large repo) to re-derive answers that change only when somebody commits,
//! switches branch, fetches, or adds or removes a worktree. Each of those leaves
//! a mark on disk, and a `stat` is orders of magnitude cheaper than a spawn.
//!
//! **What is stamped, and why each one is there:**
//!
//! - `HEAD` of the checkout and of the common dir, `config` (branch upstreams
//!   live there — `git branch --set-upstream-to` touches nothing else) and
//!   `packed-refs`.
//! - Every directory and every loose ref file under `refs/`. A ref is written as
//!   `x.lock` and renamed over `x`, so the directory's mtime moves on create,
//!   delete and update, and the file's inode moves on update. Both are stamped
//!   because a loose ref that is rewritten may keep its length and, on a
//!   coarse-grained filesystem, its mtime.
//! - `reftable/`, for repos on the reftable backend.
//! - The `worktrees/` directory (moves when a linked worktree is added or
//!   pruned) and, for each linked worktree, its `HEAD`, `gitdir`, `locked` and
//!   `commondir` — plus **whether the checkout that `gitdir` points at still
//!   exists**, so `rm -rf` of a checkout reads as a change: git reports it as
//!   `prunable`, and nothing inside the git dir moved.
//!
//! **Not stamped, deliberately:** the index and anything that churns without
//! changing these answers (`FETCH_HEAD`, `ORIG_HEAD`, `logs/`, object files).
//! `git status` rewrites the index opportunistically, so stamping it would
//! invalidate the cache from the dirty sweep that runs beside it.
//!
//! **Fails toward git.** Any stat error other than "an optional file is absent"
//! yields `None`, and `None` means "do not trust a cache" — a vanished repo must
//! still reach git, whose answer is what the rail's `available` flag reports. So
//! does a fingerprint that is too large to be cheap ([`MAX_ENTRIES`]) and one
//! whose inputs changed within [`SETTLE`] of now, because a filesystem with
//! coarse timestamps could let a second change in the same tick go unseen.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// More stamped entries than this and the fingerprint gives up (returns `None`):
/// a repo with that many loose refs is one `git pack-refs` away from cheap, and
/// until then walking them costs about what the spawn it was meant to avoid does.
const MAX_ENTRIES: usize = 3000;

/// How recently an input may have changed and still be trusted — see the module
/// docs. A change this fresh is simply not cached; the next poll reads again.
const SETTLE: Duration = Duration::from_secs(2);

/// What one path looked like.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stamp {
    /// Absent, where absence is a normal, stable state (no `packed-refs` yet).
    Missing,
    Present {
        mtime_ns: i128,
        size: u64,
        ino: u64,
    },
}

/// Every stamped path and what it looked like. A map so equality does not depend
/// on directory iteration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RepoFingerprint(BTreeMap<PathBuf, Stamp>);

/// A fingerprint equal only to another of the same `seed`, for tests of what is
/// kept under one.
#[cfg(test)]
pub(super) fn for_test(seed: u64) -> RepoFingerprint {
    RepoFingerprint(BTreeMap::from([(
        PathBuf::from("seed"),
        Stamp::Present {
            mtime_ns: 0,
            size: seed,
            ino: 0,
        },
    )]))
}

/// The fingerprint of `repo_root`'s git state, or `None` when it cannot be taken
/// cheaply and trustworthily (see the module docs) — in which case the caller
/// must ask git.
pub(super) fn fingerprint(repo_root: &Path) -> Option<RepoFingerprint> {
    fingerprint_at(repo_root, SystemTime::now())
}

/// [`fingerprint`] against an explicit clock, so a test can take one of a
/// repository it has only just written to.
fn fingerprint_at(repo_root: &Path, now: SystemTime) -> Option<RepoFingerprint> {
    let (gitdir, common) = locate(repo_root)?;
    let mut out = Walk {
        stamps: BTreeMap::new(),
        newest: None,
    };

    // Required: a repo without these is not one we can vouch for.
    out.file(&gitdir.join("HEAD"), true)?;
    out.file(&common.join("HEAD"), true)?;
    out.file(&common.join("config"), true)?;
    // Optional: absent is a normal state.
    out.file(&gitdir.join("config.worktree"), false)?;
    out.file(&common.join("packed-refs"), false)?;

    out.tree(&common.join("refs"), true)?;
    out.tree(&common.join("reftable"), false)?;

    // Linked worktrees.
    let worktrees = common.join("worktrees");
    out.file(&worktrees, false)?;
    match std::fs::read_dir(&worktrees) {
        Ok(entries) => {
            for entry in entries {
                let dir = entry.ok()?.path();
                for name in ["HEAD", "gitdir", "locked", "commondir"] {
                    out.file(&dir.join(name), false)?;
                }
                // Whether the checkout is still there is the one fact about a
                // linked worktree that lives outside the git dir.
                // A relative target (`worktree.useRelativePaths`) is relative to
                // this admin dir, not to the daemon's cwd.
                if let Ok(target) = std::fs::read_to_string(dir.join("gitdir")) {
                    let target = dir.join(target.trim());
                    out.exists(&target)?;
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }

    if out.stamps.len() > MAX_ENTRIES {
        return None;
    }
    // Inputs that changed inside the settle window (or claim to have changed in
    // the future) are not trusted yet.
    if out
        .newest
        .is_some_and(|newest| newest + SETTLE > now || newest > now)
    {
        return None;
    }
    Some(RepoFingerprint(out.stamps))
}

/// Where this repo's git state lives: `(gitdir, common dir)`. Equal for an
/// ordinary or bare repo; for a linked worktree used as the root, the
/// per-worktree dir and the shared one. `None` when it cannot be worked out.
fn locate(repo_root: &Path) -> Option<(PathBuf, PathBuf)> {
    let dotgit = repo_root.join(".git");
    match std::fs::metadata(&dotgit) {
        Ok(md) if md.is_dir() => Some((dotgit.clone(), dotgit)),
        Ok(md) if md.is_file() => {
            // `gitdir: <path>` — a linked worktree or a submodule.
            let text = std::fs::read_to_string(&dotgit).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = repo_root.join(target); // absolute `target` replaces the base
            let common = match std::fs::read_to_string(gitdir.join("commondir")) {
                Ok(c) => gitdir.join(c.trim()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => gitdir.clone(),
                Err(_) => return None,
            };
            Some((gitdir, common))
        }
        Ok(_) => None,
        // A bare repo has no `.git`; its root is the git dir.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let looks_bare = repo_root.join("HEAD").is_file() && repo_root.join("objects").is_dir();
            looks_bare.then(|| (repo_root.to_path_buf(), repo_root.to_path_buf()))
        }
        Err(_) => None,
    }
}

struct Walk {
    stamps: BTreeMap<PathBuf, Stamp>,
    /// The latest mtime seen, for the settle rule.
    newest: Option<SystemTime>,
}

impl Walk {
    /// Stamp one path. `required` makes absence a failure; otherwise absence is a
    /// stamp. `None` on any failure, so `?` aborts the whole fingerprint.
    fn file(&mut self, path: &Path, required: bool) -> Option<()> {
        match std::fs::metadata(path) {
            Ok(md) => self.record(path, &md),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => {
                self.stamps.insert(path.to_path_buf(), Stamp::Missing);
                Some(())
            }
            Err(_) => None,
        }
    }

    /// Record only whether `path` exists — for something outside the git dir
    /// whose own mtime churns (a checkout's `.git` file).
    fn exists(&mut self, path: &Path) -> Option<()> {
        let stamp = match std::fs::metadata(path) {
            Ok(_) => Stamp::Present {
                mtime_ns: 0,
                size: 0,
                ino: 0,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stamp::Missing,
            Err(_) => return None,
        };
        self.stamps.insert(path.to_path_buf(), stamp);
        Some(())
    }

    /// Stamp a directory and everything beneath it (directories and files).
    /// Aborts early once the entry budget is spent rather than walk a huge tree.
    fn tree(&mut self, root: &Path, required: bool) -> Option<()> {
        let mut stack = vec![root.to_path_buf()];
        let mut first = true;
        while let Some(dir) = stack.pop() {
            let md = match std::fs::metadata(&dir) {
                Ok(md) => md,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && first && !required => {
                    self.stamps.insert(dir, Stamp::Missing);
                    return Some(());
                }
                Err(_) => return None,
            };
            first = false;
            self.record(&dir, &md)?;
            for entry in std::fs::read_dir(&dir).ok()? {
                let path = entry.ok()?.path();
                let md = std::fs::metadata(&path).ok()?;
                if md.is_dir() {
                    stack.push(path);
                } else {
                    self.record(&path, &md)?;
                }
                if self.stamps.len() > MAX_ENTRIES {
                    return None;
                }
            }
        }
        Some(())
    }

    fn record(&mut self, path: &Path, md: &std::fs::Metadata) -> Option<()> {
        let modified = md.modified().ok()?;
        let mtime_ns = match modified.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        self.newest = Some(self.newest.map_or(modified, |n| n.max(modified)));
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(md);
        #[cfg(not(unix))]
        let ino = 0;
        self.stamps.insert(
            path.to_path_buf(),
            Stamp::Present {
                mtime_ns,
                size: md.len(),
                ino,
            },
        );
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// An hour on, so nothing the test just wrote is inside [`SETTLE`].
    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(3600)
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn fp(root: &Path) -> RepoFingerprint {
        fingerprint_at(root, later()).expect("fingerprintable")
    }

    /// A repo with one commit, a bare `origin` it has pushed `main` to, and the
    /// canonical path of each (macOS `$TMPDIR` is a symlink).
    struct Fixture {
        _tmp: tempfile::TempDir,
        work: PathBuf,
        origin: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let origin = base.join("origin.git");
        let work = base.join("work");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&origin, &["init", "--bare", "-q"]);
        git(&work, &["init", "-q"]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "one"]);
        git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&work, &["push", "-q", "origin", "main"]);
        git(&work, &["fetch", "-q", "origin"]);
        Fixture {
            _tmp: tmp,
            work,
            origin,
        }
    }

    #[test]
    fn unchanged_repo_has_a_stable_fingerprint() {
        let f = fixture();
        assert_eq!(fp(&f.work), fp(&f.work));
        // Things the answers do not depend on must not move it: a `git status`
        // refreshes the index.
        let before = fp(&f.work);
        git(&f.work, &["status", "--porcelain"]);
        assert_eq!(before, fp(&f.work));

        // A no-op fetch is allowed to move it — git takes and releases a lock
        // file in `refs/remotes/origin/`, which bumps that directory's mtime —
        // but only the once: the poll re-stamps after a fetch, and a second
        // identical fetch must leave the re-stamped value alone.
        git(&f.work, &["fetch", "-q", "origin"]);
        let after_fetch = fp(&f.work);
        git(&f.work, &["status", "--porcelain"]);
        assert_eq!(after_fetch, fp(&f.work));
    }

    #[test]
    fn a_fresh_change_is_not_trusted_until_it_settles() {
        let f = fixture();
        // Real clock: everything above was written a moment ago.
        assert_eq!(fingerprint(&f.work), None);
    }

    #[test]
    fn commit_moves_it() {
        let f = fixture();
        let before = fp(&f.work);
        std::fs::write(f.work.join("b.txt"), "b").unwrap();
        git(&f.work, &["add", "."]);
        git(&f.work, &["commit", "-q", "-m", "two"]);
        assert_ne!(before, fp(&f.work));
    }

    #[test]
    fn creating_and_switching_branches_moves_it() {
        let f = fixture();
        let before = fp(&f.work);
        git(&f.work, &["branch", "feature/x"]);
        let created = fp(&f.work);
        assert_ne!(before, created, "a new loose ref in a new directory");

        git(&f.work, &["branch", "feature/y"]);
        let second = fp(&f.work);
        assert_ne!(created, second, "a new loose ref in an existing directory");

        git(&f.work, &["checkout", "-q", "feature/x"]);
        assert_ne!(second, fp(&f.work), "HEAD");

        git(&f.work, &["branch", "-q", "-D", "feature/y"]);
        assert_ne!(
            fp(&f.work),
            second,
            "a deleted ref must not look like the one before it"
        );
    }

    #[test]
    fn rewriting_a_ref_in_place_moves_it() {
        let f = fixture();
        git(&f.work, &["branch", "topic"]);
        let before = fp(&f.work);
        // Same length of content (a 40-hex sha) in the same file: only the
        // mtime/inode can tell.
        std::fs::write(f.work.join("c.txt"), "c").unwrap();
        git(&f.work, &["add", "."]);
        git(&f.work, &["commit", "-q", "-m", "three"]);
        git(&f.work, &["branch", "-f", "topic", "HEAD"]);
        assert_ne!(before, fp(&f.work));
    }

    #[test]
    fn packing_refs_moves_it() {
        let f = fixture();
        let before = fp(&f.work);
        git(&f.work, &["pack-refs", "--all"]);
        assert_ne!(before, fp(&f.work));
    }

    #[test]
    fn set_upstream_to_moves_it() {
        let f = fixture();
        git(&f.work, &["branch", "topic"]);
        let before = fp(&f.work);
        git(
            &f.work,
            &["branch", "--set-upstream-to=origin/main", "topic"],
        );
        assert_ne!(before, fp(&f.work), "upstream lives in `config` only");
    }

    #[test]
    fn a_fetch_that_brings_new_refs_moves_it() {
        let f = fixture();
        let other = f.work.parent().unwrap().join("other");
        std::fs::create_dir_all(&other).unwrap();
        git(&other, &["clone", "-q", f.origin.to_str().unwrap(), "."]);
        std::fs::write(other.join("n.txt"), "n").unwrap();
        git(&other, &["add", "."]);
        git(&other, &["commit", "-q", "-m", "elsewhere"]);
        git(&other, &["push", "-q", "origin", "main"]);

        let before = fp(&f.work);
        git(&f.work, &["fetch", "-q", "origin"]);
        assert_ne!(before, fp(&f.work), "origin/main moved");
    }

    #[test]
    fn linked_worktree_add_and_removal_move_it() {
        let f = fixture();
        let before = fp(&f.work);
        let wt = f.work.parent().unwrap().join("wt");
        git(
            &f.work,
            &["worktree", "add", "-q", "-b", "wtb", wt.to_str().unwrap()],
        );
        let added = fp(&f.work);
        assert_ne!(before, added);

        // `rm -rf` of the checkout touches nothing inside the git dir — git
        // would now list it `prunable` — so the checkout's existence is stamped.
        std::fs::remove_dir_all(&wt).unwrap();
        let vanished = fp(&f.work);
        assert_ne!(added, vanished);

        git(&f.work, &["worktree", "prune"]);
        assert_ne!(vanished, fp(&f.work));
    }

    #[test]
    fn removing_a_worktree_recorded_with_a_relative_path_moves_it() {
        let f = fixture();
        let wt = f.work.parent().unwrap().join("wt");
        git(
            &f.work,
            &["worktree", "add", "-q", "-b", "wtb", wt.to_str().unwrap()],
        );
        // What `git worktree add --relative-paths` writes; set by hand so the
        // test does not depend on a git new enough to have the flag.
        std::fs::write(
            f.work.join(".git/worktrees/wt/gitdir"),
            "../../../../wt/.git\n",
        )
        .unwrap();
        let present = fp(&f.work);
        std::fs::remove_dir_all(&wt).unwrap();
        assert_ne!(present, fp(&f.work));
    }

    #[test]
    fn a_commit_in_a_linked_worktree_moves_it() {
        let f = fixture();
        let wt = f.work.parent().unwrap().join("wt");
        git(
            &f.work,
            &["worktree", "add", "-q", "-b", "wtb", wt.to_str().unwrap()],
        );
        let before = fp(&f.work);
        std::fs::write(wt.join("w.txt"), "w").unwrap();
        git(&wt, &["add", "."]);
        git(&wt, &["commit", "-q", "-m", "in worktree"]);
        assert_ne!(before, fp(&f.work), "refs/heads/wtb moved");
    }

    #[test]
    fn a_linked_worktree_used_as_the_root_is_followed_to_the_common_dir() {
        let f = fixture();
        let wt = f.work.parent().unwrap().join("wt");
        git(
            &f.work,
            &["worktree", "add", "-q", "-b", "wtb", wt.to_str().unwrap()],
        );
        let before = fp(&wt);
        git(&f.work, &["branch", "elsewhere"]);
        assert_ne!(before, fp(&wt), "a ref created from the main checkout");
    }

    #[test]
    fn a_vanished_or_foreign_directory_is_not_cacheable() {
        let f = fixture();
        std::fs::remove_dir_all(&f.work).unwrap();
        assert_eq!(fingerprint_at(&f.work, later()), None);

        let plain = tempfile::tempdir().unwrap();
        assert_eq!(fingerprint_at(plain.path(), later()), None);
    }

    #[test]
    fn a_broken_git_dir_is_not_cacheable() {
        let f = fixture();
        std::fs::remove_file(f.work.join(".git/HEAD")).unwrap();
        assert_eq!(fingerprint_at(&f.work, later()), None);
    }
}
