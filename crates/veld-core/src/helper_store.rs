//! The root-owned store the privileged `veld-helper` binary is served from
//! (#262), and the only way anything gets into it.
//!
//! # Why this exists
//!
//! A privileged install used to run its root LaunchDaemon out of
//! `$HOME/.local/lib/veld`. Overwriting that file gave the installing user root
//! at the next reboot — nothing verifies a binary at *process start*, and
//! nothing can, because the process doing the checking would be the attacker's.
//! [`crate::paths::privileged_helper_dir`] is where the binary lives instead;
//! this module is what puts it there.
//!
//! # The rule every function here obeys
//!
//! **Bytes are read once, verified, and written. Never re-read.**
//!
//! The candidate arrives at a path the caller chose and the attacker can write —
//! a download in `/tmp`, the old helper in the user's lib dir. Verifying a
//! *path* and then copying that *path* is a swap window: the attacker replaces
//! the file between the two, and root installs their payload having checked
//! somebody else's. [`Candidate`] holds the bytes and the signature it verified
//! them against, and [`Candidate::install`] writes those bytes — so what is
//! checked and what lands are the same object, not the same name.
//!
//! # And the version, which is not optional
//!
//! A signature attests provenance, not currency. The attacker *is* the
//! installing user, so the socket's uid gate admits them and they can drive an
//! install directly — handing it an older, genuinely signed helper with a known
//! vulnerability. It verifies. So an install also requires the candidate's
//! version, read out of the bytes the signature covers
//! ([`crate::signing::version_in_signed_bytes`]), to be no older than the
//! version already running.
//!
//! # What this deliberately does not do
//!
//! It never re-signs the binary on macOS. CI ad-hoc signs the helper *before*
//! `veld-sign` covers it, so the shipped bytes already carry the code signature
//! and the detached `.sig` matches them exactly. `install.sh`'s own
//! `codesign --force --sign -` is a byte-idempotent no-op for that reason; a
//! re-sign here would not be, and would invalidate the signature this store
//! exists to enforce.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::signing;

/// Largest candidate binary this will read. The release helper is ~6 MB, so
/// this leaves twenty times the headroom a real one needs — it exists to bound a
/// hostile path, not to describe a real artifact. The caller is unprivileged and
/// names the file, so without a bound "install this" is "make the root daemon
/// allocate a disk's worth of memory", and the process that gets OOM-killed is
/// the one holding every live URL up.
const MAX_CANDIDATE_BYTES: u64 = 128 * 1024 * 1024;

/// Mode for the store directory: root-owned, and readable by everyone because
/// `veld doctor` runs as the user and reports on the binary and its signature.
/// Readable is not writable; the whole point is the missing `w`.
const DIR_MODE: u32 = 0o755;
/// Mode for the installed binary — executable by launchd/systemd (root),
/// readable by doctor, writable by nobody but root.
const BIN_MODE: u32 = 0o755;
/// Mode for the detached signature beside it.
const SIG_MODE: u32 = 0o644;

/// A helper binary that has been read into memory together with the detached
/// signature it will be checked against.
///
/// Constructing one reads; [`Self::verified_version`] checks; [`Self::install`]
/// writes what was read. See the module doc for why those must be three views of
/// one set of bytes rather than three visits to one path.
pub struct Candidate {
    bytes: Vec<u8>,
    /// Read alongside the binary and kept, so the `.sig` installed beside it is
    /// the one that was actually verified rather than whatever the path holds by
    /// the time we get around to copying it.
    sig: [u8; 64],
    /// Only for error messages. Never re-opened.
    source: PathBuf,
}

/// Written out rather than derived, and that is a deliberate choice: a derived
/// `Debug` prints the whole candidate — several megabytes of binary and the raw
/// signature — into whatever formatted it, which for a `Result` in a test is a
/// panic message and in a handler could be a log line. Sizes and a path say
/// everything a reader of either actually wants.
impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("source", &self.source)
            .field("bytes", &format_args!("{} bytes", self.bytes.len()))
            .field("sig", &format_args!("64 bytes"))
            .finish()
    }
}

impl Candidate {
    /// Read `binary` and its sibling `<binary>.sig` into memory.
    ///
    /// Bounded by [`MAX_CANDIDATE_BYTES`], and the bound is applied to the read
    /// rather than to a prior `metadata()` call: a stat answers about the file
    /// that was there a moment ago, and this path's whole hazard is a file that
    /// changes underneath us.
    pub fn read(binary: &Path) -> Result<Self> {
        let file = std::fs::File::open(binary)
            .with_context(|| format!("cannot read the helper at {}", binary.display()))?;
        let mut bytes = Vec::new();
        file.take(MAX_CANDIDATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("cannot read the helper at {}", binary.display()))?;
        if bytes.len() as u64 > MAX_CANDIDATE_BYTES {
            bail!(
                "the file at {} is larger than {MAX_CANDIDATE_BYTES} bytes; refusing to install it",
                binary.display()
            );
        }
        let sig = signing::read_detached_sig_bytes(binary).with_context(|| {
            format!(
                "no readable 64-byte signature at {}",
                signing::sig_path_for(binary).display()
            )
        })?;
        Ok(Self {
            bytes,
            sig,
            source: binary.to_path_buf(),
        })
    }

    /// The candidate's own version, once its bytes verify against the org key —
    /// or an error saying which of the two checks failed.
    ///
    /// The order matters and is not interchangeable: the version is only
    /// meaningful *because* the signature covers the bytes it was read from, so
    /// verification comes first and a failure there short-circuits.
    pub fn verified_version(&self) -> Result<String> {
        self.verified_version_with(&signing::ORG_SIGNING_PUBKEY)
    }

    /// [`Self::verified_version`] against an explicit key.
    ///
    /// Private, and the whole seam this type has: the public entry points name
    /// [`signing::ORG_SIGNING_PUBKEY`] and cannot be pointed at another key. It
    /// exists so the tests below can build a *genuinely signed* candidate —
    /// which is the only way to exercise the check that matters, since a
    /// correctly signed older release is exactly what the version rule exists to
    /// refuse and an unsigned fixture would be rejected one step earlier and
    /// prove nothing.
    fn verified_version_with(&self, pubkey: &[u8; 32]) -> Result<String> {
        if !signing::verify_data(pubkey, &self.bytes, &self.sig) {
            bail!(
                "the helper at {} is not signed with the org's key; refusing to install it",
                self.source.display()
            );
        }
        signing::version_in_signed_bytes(&self.bytes).with_context(|| {
            format!(
                "the helper at {} is signed but carries no version record, so it cannot be \
                 checked against the running version; refusing to install it",
                self.source.display()
            )
        })
    }

    /// Verify and version-check, returning the version that may be installed.
    ///
    /// Split from the writing half so the decision — which is the whole security
    /// property — can be tested without a root-owned directory to write into.
    fn approve(&self, pubkey: &[u8; 32], running_version: &str) -> Result<String> {
        let version = self.verified_version_with(pubkey)?;
        if !signing::version_is_not_older(&version, running_version) {
            bail!(
                "refusing to install veld-helper {version} over the running {running_version}: a \
                 signature says who built a binary, not how old it is, so an install may only \
                 move forward"
            );
        }
        Ok(version)
    }

    /// Verify, version-check against `running_version`, and install into the
    /// store. Returns the version installed.
    ///
    /// Equal versions are accepted — see
    /// [`crate::signing::version_is_not_older`]. Older ones are the whole reason
    /// this function takes `running_version` at all.
    pub fn install(&self, running_version: &str) -> Result<String> {
        let version = self.approve(&signing::ORG_SIGNING_PUBKEY, running_version)?;

        let dir = ensure_dir()?;
        let bin = dir.join("veld-helper");
        let sig = signing::sig_path_for(&bin);

        // Signature first, binary second. Both land by rename, so each is
        // atomic on its own — but the order decides what a crash in between
        // leaves behind, and a new binary beside a stale signature is a helper
        // that will refuse to relaunch onto itself. A stale binary beside a new
        // signature is the same refusal, one release earlier, and the next
        // install repairs it.
        write_atomically(&sig, &self.sig, SIG_MODE)?;
        write_atomically(&bin, &self.bytes, BIN_MODE)?;
        Ok(version)
    }

    /// The bytes, for a caller that needs to look at them without installing.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The store directory, created root-owned if it is not already there.
///
/// The parent (`/var/db`, `/var/lib`) is root-owned on every machine, so an
/// unprivileged process cannot create this path ahead of us — which is the
/// reason [`crate::paths::privileged_helper_dir`] is under it rather than under
/// `/usr/local`. The ownership check below is therefore not expected to fire;
/// it is here because "cannot happen" is a poor foundation for the one directory
/// whose contents run as root, and because a machine may have been through an
/// older veld, a restore, or an administrator.
pub fn ensure_dir() -> Result<PathBuf> {
    let dir = crate::paths::privileged_helper_dir();
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    set_mode(&dir, DIR_MODE)?;
    assert_root_owned(&dir)?;
    Ok(dir)
}

/// Whether `path` is a directory only root can write — the question "is this
/// install already safe", asked of the directory a helper binary sits in.
///
/// The privileged helper's own migration (#262) uses this to decide whether it
/// has anything to do: a legacy system-paths install already keeps its helper in
/// a root-owned `/usr/local/lib/veld` created under `sudo`, and moving it would
/// be churn and risk on a machine that never had the bug. `false` when the path
/// cannot be stat'd, which keeps "cannot tell" out of the safe answer.
pub fn is_root_owned_and_locked(path: &Path) -> bool {
    assert_root_owned(path).is_ok()
}

/// Fail unless `path` is owned by root and writable by nobody else.
///
/// Both halves are needed. Root ownership alone still admits a `0777` directory
/// somebody left behind, and permissions alone say nothing about who can
/// `chmod` them back.
fn assert_root_owned(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    let meta =
        std::fs::metadata(path).with_context(|| format!("cannot stat {}", path.display()))?;
    if meta.uid() != 0 {
        bail!(
            "{} is owned by uid {} rather than root; refusing to serve the privileged helper from \
             a directory somebody else controls",
            path.display(),
            meta.uid()
        );
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o022 != 0 {
        bail!(
            "{} is group- or world-writable (mode {mode:o}); refusing to serve the privileged \
             helper from it",
            path.display()
        );
    }
    Ok(())
}

/// Write `bytes` to `path` via a temporary file in the same directory, then
/// rename over it.
///
/// The temporary sits *inside* the store, which is the point: it is root-owned
/// and on the same filesystem, so nobody can touch the file between write and
/// rename, and the rename is atomic rather than a copy that can be interrupted
/// half-way. Replacing a running binary this way is safe on both platforms — the
/// live process keeps the old inode and only the directory entry moves.
fn write_atomically(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    // Appended, never `with_extension`, and this is not style. `with_extension`
    // *replaces* an existing one, so `veld-helper` and `veld-helper.sig` would
    // both stage through `veld-helper.incoming` — the binary and its signature
    // writing over each other, and whichever renamed second winning. Appending
    // gives `veld-helper.incoming` and `veld-helper.sig.incoming`, which is the
    // same rule `signing::sig_path_for` follows and for the same reason.
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".incoming");
    let tmp = PathBuf::from(tmp);
    // A leftover from an interrupted install would otherwise be opened and
    // truncated, which is fine, but removing it first also clears a stale mode.
    let _ = std::fs::remove_file(&tmp);
    {
        let mut file = std::fs::File::create(&tmp)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        // Durable before the rename: a crash that left the directory entry
        // pointing at unwritten blocks would be a root service that cannot
        // exec, with `KeepAlive` retrying it forever.
        file.sync_all()
            .with_context(|| format!("cannot flush {}", tmp.display()))?;
    }
    set_mode(&tmp, mode)?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("cannot move {} into place", tmp.display()))?;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("cannot set mode {mode:o} on {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

    /// A scratch directory that cleans itself up, so a failing assertion does
    /// not leave fixtures behind for the next run to trip over.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "veld-store-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// A stand-in for the org's key. The production entry points cannot be
    /// pointed at it — see [`Candidate::verified_version_with`].
    fn test_key() -> (SigningKey, [u8; 32]) {
        let signing = SigningKey::from_bytes(&[3u8; 32]);
        (signing.clone(), VerifyingKey::from(&signing).to_bytes())
    }

    /// A helper binary as the release pipeline would produce one: bytes carrying
    /// an embedded version record, with a detached signature over exactly those
    /// bytes.
    fn signed_helper(dir: &Path, version: &str, key: &SigningKey) -> PathBuf {
        let mut bytes = b"\x7fELF ... a plausible helper ...".to_vec();
        bytes.extend_from_slice(&signing::version_record(version));
        bytes.extend_from_slice(b"... and the rest of the program ...");
        let bin = dir.join("veld-helper");
        std::fs::write(&bin, &bytes).unwrap();
        std::fs::write(signing::sig_path_for(&bin), key.sign(&bytes).to_bytes()).unwrap();
        bin
    }

    /// **The acceptance criterion this whole module exists for.**
    ///
    /// The candidate is *genuinely signed* — it verifies, because the org really
    /// did build it — and it is still refused, because it is older than what is
    /// running. That is the downgrade a root-owned directory alone does not
    /// close: the attacker is the installing user, the socket's uid gate admits
    /// them by design, and they can hand this straight to the install RPC.
    #[test]
    fn an_older_but_genuinely_signed_helper_is_refused() {
        let scratch = Scratch::new("older");
        let (key, pubkey) = test_key();
        let bin = signed_helper(scratch.path(), "16.57.0", &key);
        let candidate = Candidate::read(&bin).unwrap();

        // It really does verify — this is not a signature failure in disguise.
        assert_eq!(candidate.verified_version_with(&pubkey).unwrap(), "16.57.0");

        let err = candidate
            .approve(&pubkey, "16.58.3")
            .unwrap_err()
            .to_string();
        assert!(err.contains("may only move forward"), "{err}");
    }

    /// The same helper, newer, is accepted — otherwise the rule above would be
    /// indistinguishable from "refuse everything", and updates would be wedged.
    #[test]
    fn a_newer_genuinely_signed_helper_is_accepted() {
        let scratch = Scratch::new("newer");
        let (key, pubkey) = test_key();
        let bin = signed_helper(scratch.path(), "16.59.0", &key);
        let candidate = Candidate::read(&bin).unwrap();
        assert_eq!(candidate.approve(&pubkey, "16.58.3").unwrap(), "16.59.0");
    }

    /// Re-installing the running release is allowed: it is what a repair, a
    /// pinned `--target-version`, and a re-run of `install.sh` all do.
    #[test]
    fn the_running_version_may_be_reinstalled() {
        let scratch = Scratch::new("same");
        let (key, pubkey) = test_key();
        let bin = signed_helper(scratch.path(), "16.58.3", &key);
        let candidate = Candidate::read(&bin).unwrap();
        assert_eq!(candidate.approve(&pubkey, "16.58.3").unwrap(), "16.58.3");
    }

    /// A newer helper signed by the *wrong* key is refused on the signature,
    /// before its version is ever believed. Version and signature are not
    /// alternatives; the version is only meaningful because the signature covers
    /// the bytes it was read from.
    #[test]
    fn a_wrongly_signed_helper_is_refused_however_new_it_claims_to_be() {
        let scratch = Scratch::new("wrongkey");
        let attacker = SigningKey::from_bytes(&[4u8; 32]);
        let (_, org_pubkey) = test_key();
        let bin = signed_helper(scratch.path(), "99.0.0", &attacker);
        let candidate = Candidate::read(&bin).unwrap();
        let err = candidate
            .approve(&org_pubkey, "16.58.3")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not signed with the org's key"), "{err}");
    }

    /// Tampering after signing is caught: the signature covers the bytes, and
    /// the bytes are what was read.
    #[test]
    fn a_helper_modified_after_signing_is_refused() {
        let scratch = Scratch::new("tampered");
        let (key, pubkey) = test_key();
        let bin = signed_helper(scratch.path(), "16.59.0", &key);
        let mut bytes = std::fs::read(&bin).unwrap();
        bytes.extend_from_slice(b"payload");
        std::fs::write(&bin, &bytes).unwrap();

        let candidate = Candidate::read(&bin).unwrap();
        let err = candidate
            .approve(&pubkey, "16.58.3")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not signed with the org's key"), "{err}");
    }

    /// A genuinely signed helper from before this mechanism existed carries no
    /// record, and is refused rather than assumed current.
    ///
    /// That is the correct reading: no record means older than the first release
    /// that had one, so "unversioned" and "too old" are the same fact.
    #[test]
    fn a_signed_helper_with_no_version_record_is_refused() {
        let scratch = Scratch::new("norecord");
        let (key, pubkey) = test_key();
        let bytes = b"a genuinely signed release from before the version record".to_vec();
        let bin = scratch.path().join("veld-helper");
        std::fs::write(&bin, &bytes).unwrap();
        std::fs::write(signing::sig_path_for(&bin), key.sign(&bytes).to_bytes()).unwrap();

        let candidate = Candidate::read(&bin).unwrap();
        let err = candidate
            .approve(&pubkey, "16.58.3")
            .unwrap_err()
            .to_string();
        assert!(err.contains("carries no version record"), "{err}");
    }

    /// The bytes that were verified are the bytes that would be installed.
    ///
    /// A swap at the source path after reading changes nothing, because nothing
    /// re-reads it — the check and the write are two views of one buffer. This is
    /// the property that makes it safe for root to accept a path chosen by an
    /// unprivileged caller.
    #[test]
    fn a_swap_after_reading_cannot_change_what_would_be_installed() {
        let scratch = Scratch::new("swap");
        let (key, pubkey) = test_key();
        let bin = signed_helper(scratch.path(), "16.59.0", &key);
        let candidate = Candidate::read(&bin).unwrap();

        std::fs::write(&bin, b"the attacker's payload").unwrap();
        std::fs::write(signing::sig_path_for(&bin), [0u8; 64]).unwrap();

        assert_eq!(candidate.approve(&pubkey, "16.58.3").unwrap(), "16.59.0");
        assert!(
            candidate
                .bytes()
                .ends_with(b"... and the rest of the program ...")
        );
    }

    /// The ownership gate, exercised on a directory the test owns. A test runs
    /// as the user, so the *passing* case cannot be built here — this pins the
    /// refusal, which is the direction that matters.
    #[test]
    fn a_directory_owned_by_somebody_other_than_root_is_refused() {
        let dir = std::env::temp_dir().join(format!("veld-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = assert_root_owned(&dir).unwrap_err().to_string();
        assert!(err.contains("owned by uid"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_atomic_write_lands_with_the_mode_it_was_given() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("veld-store-w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("veld-helper");
        write_atomically(&path, b"bytes", 0o755).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"bytes");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // And the temporary is gone rather than left beside it.
        assert!(!path.with_extension("incoming").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The binary and its signature must not stage through the same temporary.
    ///
    /// `with_extension("incoming")` *replaces* `.sig`, so both files would have
    /// used `veld-helper.incoming` and the second rename would have moved
    /// whatever the first had just written. The symptom would be a store holding
    /// a 64-byte "binary" or a 6 MB "signature", i.e. a privileged helper that
    /// cannot start — with nothing in the install path reporting a failure.
    #[test]
    fn the_binary_and_its_signature_do_not_share_a_temporary() {
        let scratch = Scratch::new("tmpname");
        let bin = scratch.path().join("veld-helper");
        let sig = signing::sig_path_for(&bin);

        write_atomically(&sig, &[9u8; 64], SIG_MODE).unwrap();
        write_atomically(&bin, b"the binary", BIN_MODE).unwrap();

        assert_eq!(std::fs::read(&bin).unwrap(), b"the binary");
        assert_eq!(std::fs::read(&sig).unwrap(), vec![9u8; 64]);
    }

    /// An unsigned candidate is refused before its version is ever consulted —
    /// the ordering the module doc calls non-interchangeable.
    #[test]
    fn an_unsigned_candidate_is_refused_for_its_signature_not_its_version() {
        let dir = std::env::temp_dir().join(format!("veld-store-u-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("veld-helper");
        std::fs::write(&bin, b"not the org's build").unwrap();
        std::fs::write(signing::sig_path_for(&bin), [7u8; 64]).unwrap();
        let candidate = Candidate::read(&bin).unwrap();
        let err = candidate.verified_version().unwrap_err().to_string();
        assert!(err.contains("not signed with the org's key"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A candidate with no `.sig` at all fails at read time, before anything
    /// has been allocated for it.
    #[test]
    fn a_candidate_with_no_signature_is_refused_at_read() {
        let dir = std::env::temp_dir().join(format!("veld-store-n-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("veld-helper");
        std::fs::write(&bin, b"unsigned").unwrap();
        let err = Candidate::read(&bin).unwrap_err().to_string();
        assert!(err.contains("no readable 64-byte signature"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
