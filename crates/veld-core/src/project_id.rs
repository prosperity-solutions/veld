//! What identifies "the same project" across several worktrees.
//!
//! A machine override describes the *laptop*, not the checkout — which container
//! runtime is installed, how much memory this machine can spare. Keying one by
//! the project root would ask for it again in every worktree of the same repo,
//! which is the complaint that produced the feature.
//!
//! So a project id is **where this config lives in the repo's main checkout**:
//! the canonicalized main-worktree root, plus the config directory's path
//! relative to the checkout it was found in. Every linked worktree of a repo
//! resolves to the same string, and two configs in one monorepo stay distinct.
//! Outside a git repo it degrades to the canonicalized config directory, which
//! is the status quo rather than something worse.
//!
//! ```text
//! ~/git/veld/veld.json                              -> ~/git/veld
//! ~/git/_worktrees/issue-223/veld.json              -> ~/git/veld
//! ~/git/_worktrees/issue-223/services/api/veld.json -> ~/git/veld/services/api
//! /tmp/scratch/veld.json                            -> /tmp/scratch   (no git)
//! ```
//!
//! **Why a path and not something that survives `mv`.** A root-commit SHA does
//! survive a move, and was the first answer — but a `--depth=1` clone reports
//! *HEAD* as its root commit, so a shallow checkout gets a plausible, wrong, and
//! fetch-dependent identity with no error. More importantly, every other
//! project-scoped thing in this database (runs, logs, stats, feedback) is keyed
//! by a raw path already. A cleverer identity here would mean overrides survive a
//! move while the run history that explains them does not, and two disagreeing
//! notions of "the same project" is a worse bug than the one it fixes. Moving a
//! checkout orphans its overrides; `veld config vars` prints the id so the next
//! `veld config set` is obvious.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The project a machine override is keyed by. Opaque to callers except for
/// display — it is a path, but nothing may assume it exists on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectId(String);

impl ProjectId {
    /// The stored key.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build one from an already-known key (a database row).
    pub fn from_stored(key: impl Into<String>) -> Self {
        Self(key.into())
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Resolve the project id for a config directory.
///
/// Never fails: git absence, a broken `git`, and an uncanonicalizable path all
/// fall back to the directory itself. An override that lands under a fallback key
/// is still correct for the checkout that set it — it just is not shared with
/// that project's other worktrees, which is exactly the behaviour of having no
/// feature at all.
pub fn project_id_for(config_dir: &Path) -> ProjectId {
    project_id_for_with_path(config_dir, None)
}

/// [`project_id_for`], with an explicit `PATH` for the `git` it shells out to.
///
/// **A daemon must pass one.** The daemon runs with a bare service `PATH`
/// (launchd/systemd), so a `git` installed by nix, asdf, or Homebrew-on-Linux is
/// not on it — and because this function never fails, a daemon that cannot run
/// `git` silently gets the *fallback* id (the config directory) while the CLI, on
/// the user's `PATH`, gets the git-derived one. The two then key the same project
/// differently: the UI writes an answer, reports success, and `veld start` never
/// reads it. AGENTS.md already requires the injection for the daemon's other git
/// plumbing (`desktop.rs`); this is the same rule, and the silent-divergence
/// failure mode is why it matters more here than a "command not found" would.
pub fn project_id_for_with_path(config_dir: &Path, path_env: Option<&str>) -> ProjectId {
    let here = canonicalize(config_dir);
    match main_worktree_root(&here, path_env) {
        Some(main_root) => {
            // The config's position *inside this checkout*, replayed against the
            // main checkout. `strip_prefix` is exact — both sides are
            // canonicalized, so this is not a textual prefix match.
            let checkout_root =
                canonicalize(&worktree_root(&here, path_env).unwrap_or(here.clone()));
            match here.strip_prefix(&checkout_root) {
                Ok(rel) if rel.as_os_str().is_empty() => ProjectId(path_key(&main_root)),
                Ok(rel) => ProjectId(path_key(&main_root.join(rel))),
                // The config dir is not under its own worktree root. That should
                // be impossible; treat it as "no git" rather than guessing.
                Err(_) => ProjectId(path_key(&here)),
            }
        }
        None => ProjectId(path_key(&here)),
    }
}

/// The canonicalized root of the repo's **main** worktree, or `None` outside git.
///
/// `git rev-parse --git-common-dir` is the primitive: it points at the one `.git`
/// every linked worktree shares. It needs `--path-format=absolute`, because
/// without it the answer is a bare relative `.git` in the main checkout and an
/// absolute path in a linked worktree — the same code reading two different kinds
/// of answer depending on where the user is standing.
fn main_worktree_root(dir: &Path, path_env: Option<&str>) -> Option<PathBuf> {
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        path_env,
    )?;
    let common = PathBuf::from(common);
    // `<main>/.git` for a normal clone; a bare repo has no working tree and no
    // project to key, so the parent is the answer in both readable cases.
    let root = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent()?.to_path_buf()
    } else {
        // A separate git dir (`git init --separate-git-dir`) or a bare repo:
        // there is no main checkout to point at, so fall back to no-git rather
        // than key every worktree under a directory that holds no config.
        return None;
    };
    Some(canonicalize(&root))
}

/// The canonicalized root of the worktree `dir` sits in.
fn worktree_root(dir: &Path, path_env: Option<&str>) -> Option<PathBuf> {
    git(
        dir,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        path_env,
    )
    .map(PathBuf::from)
}

fn git(dir: &Path, args: &[&str], path_env: Option<&str>) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null());
    if let Some(path) = path_env {
        cmd.env("PATH", path);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim_end().to_owned();
    (!s.is_empty()).then_some(s)
}

/// Canonicalize, keeping the input when the path does not exist yet. Symlink
/// resolution is what makes two spellings of one directory agree; a missing path
/// has nothing to resolve and is used verbatim.
fn canonicalize(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The stored string for a path. Matches `db::state::root_key`'s lossy
/// conversion so a project id and a project root are comparable by eye in
/// `veld config vars` output.
fn path_key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .expect("command runs");
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(root: &Path) {
        run(root, &["git", "init", "-q", "-b", "main"]);
        run(root, &["git", "config", "user.email", "t@example.com"]);
        run(root, &["git", "config", "user.name", "t"]);
        // Any file will do — git needs something to commit, and `project_id_for`
        // never reads the config. Deliberately not a `veld.json`, so the
        // root-config gate in `tests/validate-schema.sh` keeps its meaning
        // instead of gaining an exemption it does not need.
        std::fs::write(root.join("README.md"), "fixture").expect("write");
        run(root, &["git", "add", "-A"]);
        run(root, &["git", "commit", "-qm", "init"]);
    }

    #[test]
    fn a_linked_worktree_shares_the_main_checkouts_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).expect("mkdir");
        init_repo(&main);

        let linked = tmp.path().join("wt");
        run(
            &main,
            &[
                "git",
                "worktree",
                "add",
                "-q",
                linked.to_str().expect("utf8"),
                "-b",
                "feature",
            ],
        );

        assert_eq!(
            project_id_for(&main),
            project_id_for(&linked),
            "an override describes the machine, so every worktree of one repo must \
             resolve to one id"
        );
        assert_eq!(
            project_id_for(&main).as_str(),
            canonicalize(&main).to_string_lossy()
        );
    }

    #[test]
    fn a_subdirectory_config_keeps_its_own_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).expect("mkdir");
        init_repo(&main);
        let sub = main.join("services").join("api");
        std::fs::create_dir_all(&sub).expect("mkdir");

        let linked = tmp.path().join("wt");
        run(
            &main,
            &[
                "git",
                "worktree",
                "add",
                "-q",
                linked.to_str().expect("utf8"),
                "-b",
                "feature",
            ],
        );
        let linked_sub = linked.join("services").join("api");
        std::fs::create_dir_all(&linked_sub).expect("mkdir");

        assert_eq!(
            project_id_for(&sub),
            project_id_for(&linked_sub),
            "the same sub-project in two worktrees is one project"
        );
        assert_ne!(
            project_id_for(&sub),
            project_id_for(&main),
            "two configs in one monorepo are two projects"
        );
    }

    #[test]
    fn a_directory_outside_git_keys_by_itself() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("scratch");
        std::fs::create_dir_all(&dir).expect("mkdir");
        assert_eq!(
            project_id_for(&dir).as_str(),
            canonicalize(&dir).to_string_lossy(),
            "no git means no shared identity, which is the status quo, not a failure"
        );
    }
}
