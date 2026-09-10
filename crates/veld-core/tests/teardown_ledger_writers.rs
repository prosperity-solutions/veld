//! **Repo-wide guard: only teardown may record a teardown.**
//!
//! `NodeStatus::Stopped` is the per-node teardown ledger
//! (`veld_core::orchestrator::teardown_pending`): it means a node's `on_stop`
//! hook has been attempted, and every path that ends a run reads it to decide
//! which nodes still owe one. Writing it without running the hook tells every
//! later reaper that a teardown which never happened already had — and whatever
//! the hook existed to remove, for a container node its container, is stranded
//! permanently with no veld command able to collect it.
//!
//! That has already happened three times in one change. `Db::clear_node_pid`
//! used to write `pid = NULL, status = 'stopped'` while its name promised one
//! column, so the daemon's stale-`stopping` reaper and both arms of its
//! terminal-run straggler sweep — three callers that only ever meant "this PID
//! is gone" — were silently marking runs as torn down. The daemon's and the
//! CLI's orphan sweeps write `NodeStatus::Failed` for the same reason.
//!
//! **This is an allowlist, deliberately, rather than a scan of the files that
//! already got it right.** An earlier version checked only `veld-daemon`'s
//! `gc.rs` and `monitor.rs`: a seventh ending path added in a new module would
//! have written `Stopped` and left every test green. Here a new file has to be
//! named before it can write the ledger, which is the point at which somebody
//! reads why.
//!
//! It is still a source scan, so it can be defeated — a write behind a helper,
//! or a `NodeStatus` value computed rather than named, is invisible to it.
//! Nothing in the type system distinguishes the eight statuses, so treat the
//! rule as the guarantee and this as the tripwire.

use std::path::{Path, PathBuf};

/// Files permitted to record a node as torn down. Each one runs the node's
/// `on_stop` hook first.
const ALLOWED: &[&str] = &["crates/veld-core/src/orchestrator.rs"];

/// This file, skipped because `is_ledger_write_spots_every_spelling` below
/// necessarily contains every spelling it is looking for.
const SELF: &str = "crates/veld-core/tests/teardown_ledger_writers.rs";

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/crates/veld-core.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root above crates/veld-core")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // `target/` and `node_modules/` hold no first-party Rust.
            let name = entry.file_name();
            if name == "target" || name == "node_modules" {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Does this line assign the ledger value to a `status` field?
///
/// **Path-qualification insensitive, which is the whole reason this is a
/// function.** The first version matched the literal `status = NodeStatus::…`
/// and the daemon spells it `veld_core::state::NodeStatus::…`, so the guard
/// passed with a deliberate violation in the tree — verified by planting one.
/// `is_ledger_write_spots_every_spelling` is that check, kept.
fn is_ledger_write(line: &str, variant: &str) -> bool {
    let Some(at) = line.find(variant) else {
        return false;
    };
    // Strip whatever path qualifies the enum, then the assignment operator.
    let before = line[..at]
        .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == ':')
        .trim_end();
    let Some(lhs) = before
        .strip_suffix('=')
        .or_else(|| before.strip_suffix(':'))
        .map(str::trim_end)
    else {
        return false;
    };
    lhs.ends_with("status")
}

#[test]
fn is_ledger_write_spots_every_spelling() {
    let variant = format!("NodeStatus::{}", "Stopped");
    for line in [
        "node.status = NodeStatus::Stopped;",
        "node.status = veld_core::state::NodeStatus::Stopped;",
        "node_state.status = crate::state::NodeStatus::Stopped;",
        "status: NodeStatus::Stopped,",
        "status:  veld_core::state::NodeStatus::Stopped,",
    ] {
        assert!(is_ledger_write(line, &variant), "missed: {line}");
    }
    for line in [
        // A read, a match arm, a serializer, a test assertion — all fine.
        "NodeStatus::Stopped => \"stopped\",",
        "_ => NodeStatus::Stopped,",
        "assert_eq!(node.status, NodeStatus::Stopped);",
        "!matches!(node_state.status, NodeStatus::Pending | NodeStatus::Stopped)",
        "node.status = NodeStatus::Failed;",
    ] {
        assert!(!is_ledger_write(line, &variant), "false positive: {line}");
    }
}

#[test]
fn only_teardown_records_a_node_as_torn_down() {
    let root = workspace_root();
    let crates = root.join("crates");
    assert!(crates.is_dir(), "no crates/ under {}", root.display());

    let mut sources = Vec::new();
    rust_sources(&crates, &mut sources);
    assert!(
        sources.len() > 20,
        "the walk found only {} Rust files, so it is not finding the tree",
        sources.len()
    );

    // Assembled at runtime so this file's own source does not match itself.
    let variant = format!("NodeStatus::{}", "Stopped");

    let mut offenders: Vec<String> = Vec::new();
    for path in sources {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.contains(&rel.as_str()) || rel == SELF {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (n, line) in src.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            if is_ledger_write(trimmed, &variant) {
                offenders.push(format!("{rel}:{}", n + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these sites record a node as torn down without running its `on_stop` \
         hook: {offenders:?}\n\
         A crash detector or sweep must write `NodeStatus::Failed` and leave the \
         ledger to whoever runs the hook. If this site really does run the hook, \
         add its path to ALLOWED in this test and say why."
    );
}
