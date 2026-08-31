//! The privileged helper's peer-uid gate: which uid its socket admits, and
//! where that uid came from.
//!
//! Lives here, not in `veld-helper`, for the reason [`crate::signing`] does —
//! two crates must agree. The helper *decides* the gate at startup; `veld
//! doctor` *reports* it from the helper's `status` response, and the wire values
//! it matches on are [`GateSource::as_str`]. One definition, parsed back with
//! [`GateSource::from_wire`], so the CLI's rendering is a compiler-checked match
//! over the same enum rather than a second list of strings to drift from.

use std::path::Path;

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
    /// The service definition says `--allow-uid 0`. Ignored — see
    /// [`Gate::resolve`].
    RefusedRootFlag,
    /// The install directory is root-owned, so there is no installing user to
    /// derive. Ignored — see [`Gate::resolve`].
    RefusedRootLibDir,
    /// The install directory could not be read at all.
    UnreadableLibDir,
    /// Not the privileged system daemon. Its `0o700` owner-only socket is the
    /// restriction; there is nothing for a peer gate to add.
    Unprivileged,
}

impl GateSource {
    /// The wire value carried in the helper's `status` response.
    pub fn as_str(self) -> &'static str {
        match self {
            GateSource::Flag => "flag",
            GateSource::LibDirOwner => "lib-dir-owner",
            GateSource::RefusedRootFlag => "refused-root-flag",
            GateSource::RefusedRootLibDir => "refused-root-lib-dir",
            GateSource::UnreadableLibDir => "unreadable-lib-dir",
            GateSource::Unprivileged => "unprivileged",
        }
    }

    /// Parse a wire value back. `None` for a value this build does not know —
    /// a helper newer than the CLI reading it, which must degrade to a vaguer
    /// message rather than to a wrong one.
    pub fn from_wire(value: &str) -> Option<Self> {
        [
            GateSource::Flag,
            GateSource::LibDirOwner,
            GateSource::RefusedRootFlag,
            GateSource::RefusedRootLibDir,
            GateSource::UnreadableLibDir,
            GateSource::Unprivileged,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
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
    /// uid requires root on both macOS and Linux, so no local process can point
    /// the gate at itself. (A directory an attacker can *write* is a strictly
    /// larger problem — they would replace the binary — and is what #262
    /// closes.) The uid is never taken from the socket caller, which would be
    /// the caller nominating its own permissions.
    ///
    /// **Why uid 0 is never a gate target, whatever its source.** A system-paths
    /// install (`/usr/local/lib/veld`, created under `sudo`) is root-owned, and
    /// gating to 0 would admit only root — locking the user's own CLI out of its
    /// helper. That is the exact bug `resolve_real_uid()` was fixed for in #253,
    /// and it presents as a helper that is simply broken.
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
        if !privileged {
            return Self::unprivileged();
        }
        match flag {
            Some(0) => Self {
                uid: None,
                source: GateSource::RefusedRootFlag,
            },
            Some(uid) => Self {
                uid: Some(uid),
                source: GateSource::Flag,
            },
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
fn lib_dir_owner(exe: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let resolved = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
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

        // An unprivileged helper is never gated, whatever the filesystem says:
        // its 0o700 owner-only socket is the restriction.
        let g = Gate::from_owner(Some(INSTALLING), false, Some(INSTALLING));
        assert_eq!(g.uid(), None);
        assert_eq!(g.source(), GateSource::Unprivileged);

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

        // Same rule for a hand-written `--allow-uid 0`, whatever its source.
        let g = Gate::from_owner(Some(0), true, Some(INSTALLING));
        assert_eq!(g.uid(), None);
        assert_eq!(g.source(), GateSource::RefusedRootFlag);

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
        for (source, wire) in [
            (GateSource::Flag, "flag"),
            (GateSource::LibDirOwner, "lib-dir-owner"),
            (GateSource::RefusedRootFlag, "refused-root-flag"),
            (GateSource::RefusedRootLibDir, "refused-root-lib-dir"),
            (GateSource::UnreadableLibDir, "unreadable-lib-dir"),
            (GateSource::Unprivileged, "unprivileged"),
        ] {
            assert_eq!(source.as_str(), wire);
            assert_eq!(GateSource::from_wire(wire), Some(source));
        }
        // A helper newer than this build is "unknown", never a wrong match.
        assert_eq!(GateSource::from_wire("invented-later"), None);
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
