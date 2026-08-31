//! The privileged helper's peer-uid gate: which uid its socket admits, and
//! where that uid came from.
//!
//! Lives here, not in `veld-helper`, for the reason [`crate::signing`] does —
//! two crates must agree. The helper *decides* the gate at startup; `veld
//! doctor` *reports* it from the helper's `status` response, and the wire values
//! it matches on are [`GateSource::as_str`]. One definition, parsed back with
//! [`GateSource::from_wire`], so the CLI renders by matching this enum rather
//! than a second list of strings to drift from — and every one of those matches
//! (`as_str` here, `gate_source_label` and `ungated_reason` in `veld doctor`) is
//! deliberately wildcard-free, so a new variant fails to build until each side
//! has said what to do with it. The field *names* are constants below for the
//! same reason; the values alone were not the whole contract.

use std::path::Path;

/// The `status` response field carrying the admitted uid (`null` when the socket
/// is ungated).
///
/// A constant, not a literal at each end, because the *names* are as much of the
/// contract as the values: rename one side and both crates still compile while
/// the `veld doctor` row silently reports a current helper as too old to check.
pub const ALLOW_UID_FIELD: &str = "allow_uid";

/// The `status` response field carrying [`GateSource::as_str`].
pub const ALLOW_UID_SOURCE_FIELD: &str = "allow_uid_source";

/// What the helper writes back before dropping a connection the gate refused.
///
/// Shared because `veld doctor` matches on it to tell "the gate refused *you*"
/// apart from "the helper is down" — which is the difference between naming the
/// one failure mode this gate can introduce and saying nothing at all. The peer
/// is untrusted, so this is for diagnosis only; nothing may depend on receiving
/// it (see `reject_connection`).
pub const REJECTED_PEER_ERROR: &str = "permission denied: untrusted peer uid";

/// Where the privileged helper's peer-uid gate got the uid it admits — or why
/// it has none.
///
/// Reported over `status` (and rendered by `veld doctor`) because a privileged
/// helper that is *not* gated is invisible from the outside: it serves every
/// command exactly as a gated one does, to anybody. See [`Gate::resolve`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateSource {
    /// `--allow-uid` from the service definition, written by
    /// `veld setup privileged`.
    Flag,
    /// Derived from the owner of the directory the helper binary lives in,
    /// because the service definition carries no `--allow-uid`.
    LibDirOwner,
    /// The install directory is root-owned, so there is no installing user to
    /// derive. Ignored — see [`Gate::resolve`].
    RefusedRootLibDir,
    /// The install directory could not be read at all.
    UnreadableLibDir,
    /// Not the privileged system daemon. Its `0o700` owner-only socket is the
    /// restriction; there is nothing for a peer gate to add.
    ///
    /// Benign where it belongs — on the user helper — but an **anomaly** coming
    /// back over the *system* socket, which is the only place `veld doctor`
    /// reads it: a helper answering there while believing itself unprivileged
    /// was given a `--socket-path` that does not match what setup writes.
    Unprivileged,
}

impl GateSource {
    /// The wire value carried in the helper's `status` response.
    pub fn as_str(self) -> &'static str {
        match self {
            GateSource::Flag => "flag",
            GateSource::LibDirOwner => "lib-dir-owner",
            GateSource::RefusedRootLibDir => "refused-root-lib-dir",
            GateSource::UnreadableLibDir => "unreadable-lib-dir",
            GateSource::Unprivileged => "unprivileged",
        }
    }

    /// Every variant, so [`Self::from_wire`] and the round-trip test share one
    /// list instead of two.
    ///
    /// The one thing here a compiler cannot check: adding a variant and
    /// forgetting this array. `as_str` and both of `veld doctor`'s rendering
    /// functions are wildcard-free and *will* fail to build, so the omission
    /// cannot escape a `cargo build` — but if it somehow did, the effect is the
    /// designed degrade path (an unrecognised source reads as unknown), never a
    /// wrong claim.
    pub const ALL: [GateSource; 5] = [
        GateSource::Flag,
        GateSource::LibDirOwner,
        GateSource::RefusedRootLibDir,
        GateSource::UnreadableLibDir,
        GateSource::Unprivileged,
    ];

    /// Parse a wire value back. `None` for a value this build does not know —
    /// a helper newer than the CLI reading it, which must degrade to a vaguer
    /// message rather than to a wrong one.
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }
}

/// The peer-uid gate a helper enforces on its socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    uid: Option<u32>,
    source: GateSource,
}

impl Gate {
    /// A helper that is not the privileged system daemon: no peer gate.
    pub fn unprivileged() -> Self {
        Self {
            uid: None,
            source: GateSource::Unprivileged,
        }
    }

    /// Decide the gate from the service definition and, failing that, from the
    /// filesystem.
    ///
    /// **Why derive at all.** `--allow-uid` is a launchd/systemd *argument*, and
    /// only `veld setup privileged` writes the service definition — `install.sh`
    /// and `veld update` swap the binary and leave the plist alone. So every
    /// privileged install that predates the flag would stay ungated until its
    /// owner ran a `sudo` command, which is not a migration story (#337). The
    /// helper can work the uid out for itself instead, and does so the moment
    /// `veld update` bounces it, with no service-definition change at all.
    ///
    /// **Why the install directory's owner.** On the default privileged install
    /// the helper lives in `$HOME/.local/lib/veld`, created by `install.sh` as
    /// the installing user — the same user whose CLI drives this helper. The
    /// owner is also not attacker-controllable: giving a file away to another
    /// uid requires root on both macOS and Linux (`_POSIX_CHOWN_RESTRICTED`,
    /// `CAP_CHOWN`), so no local process can point the gate at itself. The one
    /// way an owner appears without a `chown` is a `uid=`-mapped mount
    /// (exFAT/msdos, SMB/NFS, an image attached with ownership disabled) — and
    /// there a non-root mount maps to the *mounting* user, so an attacker still
    /// cannot name somebody else's uid. (A directory an attacker can *write* is
    /// a strictly larger problem — they would replace the binary — and is what
    /// #262 closes.) The uid is never taken from the socket caller, which would
    /// be the caller nominating its own permissions.
    ///
    /// **Why a *derived* uid 0 is refused.** A system-paths install
    /// (`/usr/local/lib/veld`, created under `sudo`) is root-owned, and gating
    /// to 0 would admit only root — locking the user's own CLI out of its
    /// helper. That is the exact bug `resolve_real_uid()` was fixed for in #253,
    /// and it presents as a helper that is simply broken. So that class of
    /// install stays **ungated**, which is an acknowledged exception to #338's
    /// "no user runs anything" rule: nothing on disk says who its installing
    /// user is, so there is nothing to derive, and the remedy is the `veld setup
    /// privileged` that `veld doctor` names.
    ///
    /// **An explicit `--allow-uid 0` is honoured, not refused.** The rule above
    /// is about a uid this code *guessed*; a 0 in the service definition is one
    /// an operator typed, and it means "root only". No veld version writes it
    /// (`resolve_real_uid()` bails), so it only exists by hand-editing —
    /// and turning it into "no gate" would silently reopen a root socket that
    /// was deliberately closed. Availability is the user's to trade away here;
    /// it is not ours to trade away for them.
    ///
    /// **Why once, at startup, rather than per connection.** The gate is a
    /// property of the install, and the install changes by replacing this
    /// binary — which restarts the process (`veld update` bounces the helper,
    /// and `watch_own_binary` exits onto a swapped one), so the derivation
    /// re-runs exactly when its inputs can have changed.
    ///
    /// Two things do go stale in between, and neither is silent. A `chown` of
    /// the install directory under a running helper leaves the old uid until the
    /// next restart. And an `--allow-uid` in the service definition is never
    /// re-derived at all, so an account renumbered under it — Migration
    /// Assistant, MDM re-creating the user, a restored backup — keeps a gate
    /// pointing at a uid that no longer exists. In both cases `veld doctor`
    /// compares the reported uid against the invoking user's and, if the peer is
    /// refused outright, says the gate refused *you* rather than "helper down".
    ///
    /// **Why the error paths leave the socket ungated rather than shut.** Every
    /// "cannot tell" branch here resolves to no gate, which is fail-open — but
    /// the alternative is a root daemon that refuses to serve, and under
    /// launchd's `KeepAlive` that is a throttled restart loop with Caddy down
    /// and every URL dropped. Ungated is also exactly the pre-existing state, so
    /// it is a non-regression rather than a new hole, and `veld doctor` reports
    /// it as a failing row with a remedy — loud, not silent.
    pub fn resolve(flag: Option<u32>, privileged: bool, exe: Option<&Path>) -> Self {
        Self::from_owner(flag, privileged, exe.and_then(lib_dir_owner))
    }

    /// [`Gate::resolve`] with the filesystem lookup already done, so the policy
    /// is testable without a chown.
    pub fn from_owner(flag: Option<u32>, privileged: bool, lib_dir_owner: Option<u32>) -> Self {
        match flag {
            // Honoured on any socket, not just the system one. `privileged`
            // gates the *derivation* — the guess — not an instruction somebody
            // wrote down; dropping the flag on an unprivileged helper would
            // silently discard it (`log_gate` says nothing in that case) and
            // contradict the rule two paragraphs up. It also matches what
            // origin/main did, which applied the flag whatever the socket path.
            // No shipped path passes it to a user helper — both setup writers
            // use the system socket — so this is consistency, not a feature.
            Some(uid) => Self {
                uid: Some(uid),
                source: GateSource::Flag,
            },
            None if !privileged => Self::unprivileged(),
            None => match lib_dir_owner {
                Some(0) => Self {
                    uid: None,
                    source: GateSource::RefusedRootLibDir,
                },
                Some(uid) => Self {
                    uid: Some(uid),
                    source: GateSource::LibDirOwner,
                },
                None => Self {
                    uid: None,
                    source: GateSource::UnreadableLibDir,
                },
            },
        }
    }

    /// The uid admitted alongside root, or `None` for an ungated socket.
    pub fn uid(&self) -> Option<u32> {
        self.uid
    }

    pub fn source(&self) -> GateSource {
        self.source
    }
}

/// The owning uid of the directory `exe` lives in.
///
/// `None` when the path has no parent or the directory cannot be stat'd — the
/// caller must treat that as "no gate", never as uid 0, which would gate to
/// root.
///
/// The path is canonicalised first so the answer describes the directory the
/// binary is actually served from. Without it the two platforms disagree for a
/// helper exec'd through a symlink: Linux's `/proc/self/exe` is already
/// resolved, while macOS's `_NSGetExecutablePath` hands back the path used to
/// exec.
///
/// A canonicalise that fails is `None`, never a fallback to the unresolved
/// path. Guessing there would stat whatever the unresolved parent happens to
/// be — the one branch where a symlinked path component would decide the
/// admitted uid, and the only place the gap between resolving and stat'ing is
/// worth anything to anyone. "Cannot tell" is a state this code already handles.
///
/// **The exe is resolved first, then its parent taken — not the other way
/// round**, and the order is the whole point. `exe.parent()` on a helper exec'd
/// through a symlink gives the directory the *symlink* sits in, which can be
/// root-owned `/tmp` while the real install belongs to the user; resolving
/// first lands on the directory the binary is actually served from.
/// `a_helper_reached_through_a_symlink_derives_from_the_real_install_dir` pins
/// it, because "just take the parent, it is all you need" is the natural
/// simplification and it is wrong.
fn lib_dir_owner(exe: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let resolved = std::fs::canonicalize(exe).ok()?;
    let dir = resolved.parent()?;
    std::fs::metadata(dir).ok().map(|m| m.uid())
}

#[cfg(test)]
mod tests {
    use super::{Gate, GateSource};

    /// The gate's whole policy, as pure logic — the filesystem lookup is the one
    /// thing held out (see `derives_the_gate_from_a_real_directorys_owner`).
    #[test]
    fn gate_policy_covers_every_way_a_uid_can_be_absent() {
        const INSTALLING: u32 = 501;

        // An unprivileged helper never *derives* a gate, whatever the filesystem
        // says: its 0o700 owner-only socket is the restriction.
        let g = Gate::from_owner(None, false, Some(INSTALLING));
        assert_eq!(g.uid(), None);
        assert_eq!(g.source(), GateSource::Unprivileged);

        // But an explicit flag is honoured wherever it appears — `privileged`
        // gates the guess, not a written-down instruction. Silently dropping it
        // is what origin/main did not do, and it would leave no trace.
        let g = Gate::from_owner(Some(INSTALLING), false, Some(999));
        assert_eq!(g.uid(), Some(INSTALLING));
        assert_eq!(g.source(), GateSource::Flag);

        // The service definition wins when it carries a usable uid.
        let g = Gate::from_owner(Some(INSTALLING), true, Some(999));
        assert_eq!(g.uid(), Some(INSTALLING));
        assert_eq!(g.source(), GateSource::Flag);

        // No flag — the pre-#337 install every existing privileged machine has —
        // derives from the install directory's owner. This is the whole point.
        let g = Gate::from_owner(None, true, Some(INSTALLING));
        assert_eq!(g.uid(), Some(INSTALLING));
        assert_eq!(g.source(), GateSource::LibDirOwner);

        // A root-owned install directory (system paths) must NOT gate to 0: that
        // admits only root and locks the user's own CLI out.
        let g = Gate::from_owner(None, true, Some(0));
        assert_eq!(g.uid(), None);
        assert_eq!(g.source(), GateSource::RefusedRootLibDir);

        // A hand-written `--allow-uid 0` is HONOURED, not refused: it is an
        // operator's explicit "root only", no veld version writes it, and
        // turning it into `None` would silently reopen a root socket somebody
        // deliberately closed. The refusal above is for a uid this code guessed.
        let g = Gate::from_owner(Some(0), true, Some(INSTALLING));
        assert_eq!(g.uid(), Some(0));
        assert_eq!(g.source(), GateSource::Flag);

        // An unreadable directory is "no gate", never uid 0.
        let g = Gate::from_owner(None, true, None);
        assert_eq!(g.uid(), None);
        assert_eq!(g.source(), GateSource::UnreadableLibDir);
    }

    /// The wire values are the contract between the helper that writes them and
    /// the `veld doctor` row that reads them, and nothing else pins them: a
    /// renamed variant would still compile on both sides and quietly stop
    /// matching. Round-tripping every variant is what catches that.
    #[test]
    fn every_gate_source_round_trips_through_the_wire() {
        // Driven off `ALL`, so a variant missing from it fails here rather than
        // in a caller — and the literal pairs still pin the exact bytes, which
        // is what `veld doctor` actually matches on.
        for (source, wire) in [
            (GateSource::Flag, "flag"),
            (GateSource::LibDirOwner, "lib-dir-owner"),
            (GateSource::RefusedRootLibDir, "refused-root-lib-dir"),
            (GateSource::UnreadableLibDir, "unreadable-lib-dir"),
            (GateSource::Unprivileged, "unprivileged"),
        ] {
            assert_eq!(source.as_str(), wire);
            assert_eq!(GateSource::from_wire(wire), Some(source));
            assert!(GateSource::ALL.contains(&source));
        }
        for source in GateSource::ALL {
            assert_eq!(
                GateSource::from_wire(source.as_str()),
                Some(source),
                "{source:?} is in ALL but does not round-trip"
            );
        }
        // A helper newer than this build is "unknown", never a wrong match.
        assert_eq!(GateSource::from_wire("invented-later"), None);
    }

    /// Exec'd through a symlink, the gate must derive from the **real** install
    /// directory's owner — not from the directory the symlink happens to sit in.
    ///
    /// This is the test that stops `lib_dir_owner` being "simplified" to
    /// `exe.parent()` then canonicalise. That reads as equivalent and is not: a
    /// helper reached through a link in a root-owned directory would derive root
    /// and refuse to gate, turning a protected install into an unprotected one.
    /// Proven on real hardware before it was written — a root helper exec'd via
    /// `/tmp/veld-helper-symlink` still gated to the lib dir's owner.
    #[test]
    fn a_helper_reached_through_a_symlink_derives_from_the_real_install_dir() {
        let real = tempfile::tempdir().unwrap();
        let exe = real.path().join("veld-helper");
        std::fs::write(&exe, b"not really a binary").unwrap();

        // The link lives somewhere else entirely; only the target's directory
        // may decide the uid.
        let elsewhere = tempfile::tempdir().unwrap();
        let link = elsewhere.path().join("veld-helper-symlink");
        std::os::unix::fs::symlink(&exe, &link).unwrap();

        assert_eq!(
            super::lib_dir_owner(&link),
            super::lib_dir_owner(&exe),
            "a symlinked path must resolve to the real install directory's owner"
        );

        // A dangling link is "cannot tell", never a guess at the link's own
        // parent — which is the branch a root-owned link directory would abuse.
        std::fs::remove_file(&exe).unwrap();
        assert_eq!(super::lib_dir_owner(&link), None);
    }

    /// The derivation reads a real directory's owner, not a constant — and
    /// `resolve` reaches it from a binary path the way the helper does.
    ///
    /// Written to hold whoever the suite runs as: on a machine where the test
    /// user *is* root the correct answer is "refuse", which is the same property
    /// stated from the other side.
    #[test]
    fn derives_the_gate_from_a_real_directorys_owner() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("veld-helper");
        std::fs::write(&exe, b"not really a binary").unwrap();
        let me = nix::unistd::Uid::effective().as_raw();

        let gate = Gate::resolve(None, true, Some(&exe));
        if me == 0 {
            assert_eq!(gate.uid(), None);
            assert_eq!(gate.source(), GateSource::RefusedRootLibDir);
        } else {
            assert_eq!(gate.uid(), Some(me));
            assert_eq!(gate.source(), GateSource::LibDirOwner);
        }

        // A path with no parent directory to stat is "no gate", not uid 0.
        let gate = Gate::resolve(None, true, Some(std::path::Path::new("/")));
        assert_eq!(gate.uid(), None);
        assert_eq!(gate.source(), GateSource::UnreadableLibDir);
    }
}
