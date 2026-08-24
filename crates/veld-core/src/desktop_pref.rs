//! Does this machine's owner want Veld Desktop?
//!
//! Veld ships as two halves of one release — the CLI and the macOS app — and for
//! a long time an install brought both without asking. That is right for someone
//! who uses Veld as an IDE and wrong for someone who uses it purely as an
//! orchestrator: they paid a ~113 MB download on every single `veld update` for
//! an app they never opened. So the app half is now a recorded answer, and this
//! module is where that answer lives.
//!
//! **A file, not the database, and not a `veld settings` key.** Two reasons, both
//! structural rather than preference:
//!
//! - `install.sh` has to read it. That script is the fresh-install path, where
//!   there is no veld binary and no database yet, and it is also what a user
//!   re-running `curl … | bash` uses to update. A value only Rust can read cannot
//!   gate the app download on the one path that has no Rust.
//! - An update **migrates** the database, and a binary refuses a `user_version`
//!   newer than it supports (`DbError::NewerSchema`). The same argument that
//!   keeps the update *lock* out of SQLite (see `update_lock`) keeps this out:
//!   the answer is read by the process doing the migrating.
//!
//! The shape is deliberately trivial — `{"wanted":true}` on one line — because it
//! is parsed in two languages. Rust reads it with serde and writes it
//! canonically; `install.sh`'s `desktop_preference` strips whitespace and looks
//! for `"wanted":true`/`"wanted":false`, which is why pretty-printing here would
//! still work but nothing may rename the key or nest it.
//! `crates/veld-core/tests/install_script_contract.rs` runs the script's own
//! reader against a file this module wrote, because a rename on either side is
//! otherwise invisible: the app would simply stop being installed, or start being
//! installed again, with every suite green.
//!
//! **Only an explicit human act ever writes this.** Answering the prompt in
//! `veld update` or `install.sh`, or running `veld desktop install` /
//! `veld desktop uninstall`. Nothing infers it — not a handoff from the running
//! app, not an ambient `VELD_DESKTOP=0`, not a fresh database. An inferred answer
//! is one the user never gets asked for and cannot remember giving.

use std::path::{Path, PathBuf};

/// The file's name inside `~/.veld`. Named here so the Rust reader, the Rust
/// writer and the contract test cannot disagree about it.
pub const FILE_NAME: &str = "desktop.json";

/// What the user said about Veld Desktop.
///
/// Two variants, and *absence of the file* is the third state — "never asked" —
/// which is why the public reader returns an `Option` rather than defaulting.
/// Collapsing "never asked" into "no" would silently opt every existing user out;
/// collapsing it into "yes" is the behaviour this module exists to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopChoice {
    /// Install it, and keep it in step on every update.
    Wanted,
    /// Never download it, and skip the app half of every update.
    Unwanted,
}

impl DesktopChoice {
    pub fn wanted(self) -> bool {
        matches!(self, DesktopChoice::Wanted)
    }

    /// For `--json` consumers and `veld doctor`.
    pub fn as_str(self) -> &'static str {
        match self {
            DesktopChoice::Wanted => "wanted",
            DesktopChoice::Unwanted => "unwanted",
        }
    }
}

/// `~/.veld/desktop.json`, or `None` when there is no home directory to put it
/// in (which is also the only way the reader and writer below can be unavailable).
pub fn path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".veld").join(FILE_NAME))
}

/// The recorded answer, or `None` for "nobody has been asked yet".
///
/// Every failure — no home directory, no file, unreadable, unparseable, a torn
/// write from a machine that lost power mid-install — arrives here as `None`,
/// i.e. as "ask again". That is the only safe direction: a corrupt file must not
/// be able to decide the answer, and re-asking costs one prompt.
pub fn read() -> Option<DesktopChoice> {
    read_in(&home()?)
}

/// Record the answer. Best-effort by return type, not by intent: a caller that
/// cannot persist the choice still has to *act* on the answer it was just given,
/// and the worst case is being asked once more.
pub fn write(choice: DesktopChoice) -> Result<(), std::io::Error> {
    let home = home().ok_or_else(|| {
        std::io::Error::other("no home directory to record the Veld Desktop preference in")
    })?;
    write_in(&home, choice)
}

/// `dirs::home_dir()`, as its own function so the two public entry points above
/// read the same way as the `_in` variants the tests drive.
fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The reader, rooted at an arbitrary home directory.
///
/// Split out for the tests, and that split is not optional here: a test that read
/// the real `~/.veld/desktop.json` would report on the machine it runs on, and a
/// test that *wrote* it would answer the maintainer's own prompt for them.
pub fn read_in(home: &Path) -> Option<DesktopChoice> {
    let text = std::fs::read_to_string(home.join(".veld").join(FILE_NAME)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    match value.get("wanted")?.as_bool()? {
        true => Some(DesktopChoice::Wanted),
        false => Some(DesktopChoice::Unwanted),
    }
}

/// The writer, rooted at an arbitrary home directory.
///
/// One line, no spaces, trailing newline — the canonical form `install.sh` is
/// tested against. `~/.veld` is created if it is not there yet: on a fresh
/// install this can be the first thing that ever writes to it.
pub fn write_in(home: &Path, choice: DesktopChoice) -> Result<(), std::io::Error> {
    let dir = home.join(".veld");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(FILE_NAME),
        format!("{{\"wanted\":{}}}\n", choice.wanted()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_answer_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_in(dir.path()), None, "nothing written yet");

        write_in(dir.path(), DesktopChoice::Wanted).unwrap();
        assert_eq!(read_in(dir.path()), Some(DesktopChoice::Wanted));

        // Overwrites rather than appends — the second answer is the answer.
        write_in(dir.path(), DesktopChoice::Unwanted).unwrap();
        assert_eq!(read_in(dir.path()), Some(DesktopChoice::Unwanted));
    }

    #[test]
    fn the_written_form_is_the_one_the_install_script_parses() {
        let dir = tempfile::tempdir().unwrap();
        write_in(dir.path(), DesktopChoice::Wanted).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(".veld").join(FILE_NAME)).unwrap();
        // `install.sh` strips whitespace and greps for this literal. Pinned here
        // as well as in the cross-language contract test, because this is the
        // half a Rust-only edit can break.
        assert_eq!(raw, "{\"wanted\":true}\n");

        write_in(dir.path(), DesktopChoice::Unwanted).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(".veld").join(FILE_NAME)).unwrap();
        assert_eq!(raw, "{\"wanted\":false}\n");
    }

    #[test]
    fn anything_unreadable_means_ask_again_rather_than_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".veld").join(FILE_NAME);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();

        // A torn write, which is what a power loss during a fresh install leaves.
        std::fs::write(&file, "{\"wanted\":tr").unwrap();
        assert_eq!(read_in(dir.path()), None);

        // Valid JSON, wrong shape. Both of these used to be tempting to default
        // to "wanted" — that is the behaviour a corrupt file must not be able to
        // reinstate.
        std::fs::write(&file, "{}").unwrap();
        assert_eq!(read_in(dir.path()), None);
        std::fs::write(&file, "{\"wanted\":\"yes\"}").unwrap();
        assert_eq!(read_in(dir.path()), None);
        std::fs::write(&file, "").unwrap();
        assert_eq!(read_in(dir.path()), None);

        // Extra keys are fine: a newer veld may add one, and an older one must
        // still read the answer rather than fall back to asking.
        std::fs::write(&file, "{\"wanted\":false,\"asked_by\":\"install.sh\"}").unwrap();
        assert_eq!(read_in(dir.path()), Some(DesktopChoice::Unwanted));
    }
}
