//! Sets `VELD_VERSION`, the version every veld binary reports ([`crate::VERSION`]).
//!
//! A release is a tag only (#424): nothing commits a version bump back to
//! `main`, so `Cargo.toml`'s `version` there stops at whatever it last said. The
//! release build stamps the real version into `Cargo.toml` before compiling, so
//! there `CARGO_PKG_VERSION` is right. A build from a checkout is the other case:
//! its `Cargo.toml` is stale, and a dev build that called itself that stale
//! version would disagree with the installed release it runs beside — the
//! helper-version gate refuses `veld orchestrator start` over exactly that.
//!
//! So: the newer of `Cargo.toml`'s version and the newest `vX.Y.Z` tag reachable from
//! `HEAD`. A release build's stamped version is newer than every tag (it is the
//! next one); a checkout's newest tag is the release it was built on, which is
//! what `Cargo.toml` used to say when release commits still bumped it. No git, or
//! no tags (a source tarball, a shallow clone): `Cargo.toml`'s version.
//!
//! It trusts every local `vX.Y.Z` tag, so a throwaway `git tag v99.0.0` becomes every
//! dev binary's version until it is deleted. A fork that never fetched upstream's
//! tags reports an old one: `git fetch upstream --tags`.
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/release_version.rs"]
#[allow(dead_code)]
mod release_version;

fn main() {
    let pkg = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&manifest_dir)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    // A git path, absolute. Not `--path-format=absolute`, which needs git 2.31:
    // without it git answers relative to the directory it ran in.
    let git_path =
        |args: &[&str]| -> Option<PathBuf> { git(args).map(|p| Path::new(&manifest_dir).join(p)) };

    // Re-run when the answer can change: a tag lands (tags live in the common git
    // dir, shared by every worktree — loose, packed, or in a reftable), or HEAD is
    // switched to another branch or commit. Not on every commit: that would mean
    // watching the branch ref or the reflog, and a re-run rebuilds veld-core and
    // every crate above it even when the version comes out the same. The gap that
    // leaves — a merge that brings an already-fetched newer tag into reach — closes
    // at the next tag fetch or checkout.
    //
    // Only paths that exist: cargo treats a missing `rerun-if-changed` path as
    // changed on every build, and a fresh CI checkout has no `packed-refs`.
    let watch = |path: &Path| {
        if path.exists() {
            println!("cargo::rerun-if-changed={}", path.display());
        }
    };
    if let Some(common) = git_path(&["rev-parse", "--git-common-dir"]) {
        watch(&common.join("refs/tags"));
        watch(&common.join("packed-refs"));
        watch(&common.join("reftable"));
    }
    if let Some(head) = git_path(&["rev-parse", "--git-path", "HEAD"]) {
        watch(&head);
    }
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/release_version.rs");

    let tag = git(release_version::DESCRIBE_RELEASE_TAG);
    let version = release_version::reported_version(&pkg, tag.as_deref());
    println!("cargo::rustc-env=VELD_VERSION={version}");
}
