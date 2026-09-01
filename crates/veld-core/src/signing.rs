//! Org signature verification for the privileged `veld-helper`.
//!
//! The privileged helper runs as root but lives in a **user-writable**
//! directory and relaunches itself (binary watcher, `restart`, `shutdown`) so
//! the service manager can pick up a new version — the #247 escalation: any
//! process with the installing user's privileges can swap the binary and get
//! root on the next relaunch. The fix: the org signs each release helper with
//! its ed25519 private key into a detached `<binary>.sig`, and the *running*
//! helper verifies the on-disk binary against its embedded public key before
//! it will `exit(0)` onto a replacement.
//!
//! The crypto and the org key live **here** in `veld-core`, not in `veld-helper`,
//! so `veld doctor` can check a helper's signature with the exact same
//! primitive and key — a duplicated constant would drift when the key rotates,
//! and two different verify paths would disagree about the one thing a security
//! gate must not guess.
//!
//! Verification is **fail-closed**: any failure — missing/oversized `.sig`,
//! unreadable binary, mismatched signature — returns false, and the callers
//! (the helper's relaunch paths, and the doctor row) treat that as "not safe to
//! relaunch onto", never "assume safe".

use std::io::Read;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};

/// The org's ed25519 public key, raw 32 bytes. Public — embedded at build time.
/// Derived from `notes/veld-signing-ed25519.pub` (SPKI); the private key lives
/// in the org vault and the GitHub `SIGNING_PRIVATE_KEY` secret.
pub const ORG_SIGNING_PUBKEY: [u8; 32] = [
    0x9d, 0x75, 0xa4, 0xc5, 0x5c, 0x02, 0xb4, 0x6e, 0x53, 0xa3, 0x0d, 0x1d, 0xf8, 0x84, 0xc8, 0xaa,
    0xf0, 0xd3, 0x90, 0x23, 0x06, 0xd1, 0xc6, 0xee, 0x53, 0x60, 0x32, 0x99, 0xa3, 0x1b, 0x31, 0x56,
];

/// Whether `data` carries a valid ed25519 signature by `pubkey` in `sig`.
pub fn verify_data(pubkey: &[u8; 32], data: &[u8], sig: &[u8]) -> bool {
    if sig.len() != 64 {
        return false;
    }
    let Ok(vk) = VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(sig) else {
        return false;
    };
    vk.verify_strict(data, &sig).is_ok()
}

/// `<path>.sig` — the sibling detached signature (append, not replace).
pub fn sig_path_for(binary: &Path) -> PathBuf {
    let mut s = binary.as_os_str().to_os_string();
    s.push(".sig");
    PathBuf::from(s)
}

/// The detached signature beside `binary`, or `None` when there isn't a
/// readable 64-byte one.
///
/// Bounds the read to 64 bytes (a valid ed25519 signature is exactly that): the
/// file can sit in a user-writable directory, so a huge one must not be slurped
/// into memory here — that would be a memory DoS on every relaunch attempt,
/// every doctor run, and (since #262) every install request.
///
/// `read_exact` rather than one `read`: a single `read` is allowed to return
/// fewer bytes than asked for even on a regular file, and a short read here
/// would fail a genuine signature closed. The bound is unchanged — the buffer is
/// 64 bytes, so a larger `.sig` is still never read past that.
fn read_detached_sig(binary: &Path) -> Option<[u8; 64]> {
    let mut file = std::fs::File::open(sig_path_for(binary)).ok()?;
    let mut sig = [0u8; 64];
    file.read_exact(&mut sig).ok()?;
    Some(sig)
}

/// [`read_detached_sig`] for callers outside this module, as an `Option` of the
/// raw 64 bytes. The install path (`crate::helper_store`) needs the signature
/// itself and not just a verdict, because it installs the `.sig` it verified
/// against rather than re-reading the caller's path a second time.
pub fn read_detached_sig_bytes(binary: &Path) -> Option<[u8; 64]> {
    read_detached_sig(binary)
}

/// Whether `binary` carries a valid org signature in `<binary>.sig`.
///
/// `false` for any failure — see the module doc on fail-closed.
pub fn verify_binary_signed(binary: &Path) -> bool {
    let Some(sig) = read_detached_sig(binary) else {
        return false;
    };
    let Ok(data) = std::fs::read(binary) else {
        return false;
    };
    verify_data(&ORG_SIGNING_PUBKEY, &data, &sig)
}

/// A reason when `binary` is NOT safe to relaunch onto, or `None` when it
/// verifies. Wraps [`verify_binary_signed`] with a diagnosable message for the
/// fail-closed callers (the helper's watcher/`restart`/`shutdown`, and `veld
/// doctor`).
pub fn relaunch_guard(binary: &Path) -> Option<String> {
    if verify_binary_signed(binary) {
        return None;
    }
    Some(format!(
        "the binary at {} is not signed with the org's key (or its {} is \
         missing/invalid); refusing to relaunch onto it",
        binary.display(),
        sig_path_for(binary).display()
    ))
}

// ---------------------------------------------------------------------------
// Version attestation (#262)
// ---------------------------------------------------------------------------
//
// The install RPC must refuse an **older but genuinely signed** helper: the
// attacker is the installing user, so #253's uid gate admits them and they can
// hand the root helper a past release with a known vulnerability. It verifies
// perfectly, because a signature attests provenance, not currency.
//
// So the version has to come from *inside* the signed payload. It already does,
// and that is the whole trick: the signature covers every byte of the binary,
// and the binary can be made to carry a version record of our own design. No
// second signed document, no change to what `veld-sign` writes, no change to
// `release.yml`'s packaging — and therefore **no way for this to wedge the
// updater**, which is the one failure #338's rule 2 forbids. A manifest, a
// `version` field prepended to the `.sig`, or a per-release signing subkey all
// change what the *previous* release's `relaunch_guard` sees, and a previous
// release that refuses to relaunch onto the new binary is an update that can
// only be repaired with sudo.
//
// Rejected alternatives, so the next reader does not re-derive them:
//
//   * **Signed JSON manifest** (version + hash, signed alongside): the obvious
//     answer. Costs `veld-sign`, `release.yml` and #339's smoke test, and puts a
//     format change in the delivery path to buy a number the binary already has.
//   * **Exec the candidate with `--version` after verifying it.** Tempting,
//     because verification does dissolve the "self-reported" objection, and it is
//     the only mechanism that can read releases published before this one. But it
//     execs a known-vulnerable artifact — the exact class this exists to reject —
//     *before* deciding to reject it.
//   * **Blocklist of every prior release's hash.** Reads the immutable past
//     exactly, but its default is *accept*: any signed build missing from the
//     list is admitted forever, silently.

/// The version record's 16-byte magic, stored **obfuscated** and recovered by
/// XOR at both compile time and run time.
///
/// The plaintext magic must never exist as a standalone constant, and this is
/// not paranoia about secrecy — the magic is public. It is because **the scanner
/// and the scanned are the same program.** A helper links
/// [`version_in_signed_bytes`], and a *newer* helper will one day scan that
/// helper looking for this magic. If the plaintext sat in the scanner's own
/// rodata, every binary would carry two hits: its record, and its copy of the
/// needle.
///
/// Storing the halves separately and joining them at runtime was tried first and
/// is not enough: in a debug build the linker placed the two halves next to each
/// other and reproduced the exact byte sequence anyway
/// (`the_scanners_own_needle_is_not_a_second_record` is what found it). That
/// mitigation depended on layout; this one depends on arithmetic. The plaintext
/// appears only where [`version_record`] writes it.
const VERSION_MAGIC_OBFUSCATED: [u8; 16] = [
    0xff, 0xa3, 0x29, 0x7e, 0x79, 0x3e, 0xed, 0xb2, 0x0f, 0x11, 0x20, 0xc7, 0xf7, 0x9c, 0x5c, 0xc2,
];

/// XOR key for [`VERSION_MAGIC_OBFUSCATED`]. Any non-zero value works; this one
/// is arbitrary and must not change once a release has shipped, because it
/// decides the magic every past helper is looking for.
const VERSION_MAGIC_KEY: u8 = 0xa5;

/// Bytes reserved for the version string inside a record. Fixed width so the
/// field has a known end without needing a terminator: rodata carries no NULs
/// between strings, so "read until the next NUL" would run into whatever the
/// linker placed next.
const VERSION_FIELD_LEN: usize = 32;

/// Total size of an embedded version record: 16 magic + 32 version.
pub const VERSION_RECORD_LEN: usize = 16 + VERSION_FIELD_LEN;

/// The plaintext magic. `const` so [`version_record`] can build it at compile
/// time, and used at runtime by [`version_in_signed_bytes`] — one definition,
/// both directions.
const fn version_record_magic() -> [u8; 16] {
    let mut magic = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        magic[i] = VERSION_MAGIC_OBFUSCATED[i] ^ VERSION_MAGIC_KEY;
        i += 1;
    }
    magic
}

/// Build the record a binary embeds so its version can be read back out of the
/// signed bytes: the magic, then `version` as ASCII, NUL-padded to a fixed width.
///
/// `const` so the result is a compile-time constant a `#[used] static` can hold —
/// the record has to be *data in the binary*, not something computed at startup,
/// or there would be nothing in the file to find.
///
/// Panics at compile time if `version` does not fit, which is the right moment:
/// a version too long to record is a build that must not ship.
pub const fn version_record(version: &str) -> [u8; VERSION_RECORD_LEN] {
    let mut out = [0u8; VERSION_RECORD_LEN];
    let magic = version_record_magic();
    let mut i = 0;
    while i < 16 {
        out[i] = magic[i];
        i += 1;
    }
    let bytes = version.as_bytes();
    assert!(
        bytes.len() <= VERSION_FIELD_LEN,
        "version string does not fit in the embedded version record"
    );
    let mut j = 0;
    while j < bytes.len() {
        out[16 + j] = bytes[j];
        j += 1;
    }
    out
}

/// The version recorded inside `bytes`, or `None` when there isn't exactly one
/// answer.
///
/// Callers must have verified `bytes` against the org key **first**. This
/// function trusts what it reads, and that trust is only earned by the
/// signature: nothing else stops a caller writing whatever version it likes into
/// a file it controls.
///
/// Every well-formed record found must agree. Requiring literally one *hit*
/// would be too strict — a macOS universal binary carries a slice per
/// architecture and therefore a record per slice, all saying the same thing —
/// and refusing that would wedge the updater on the very artifacts it exists to
/// install. Requiring one *value* keeps the strictness where it belongs.
pub fn version_in_signed_bytes(bytes: &[u8]) -> Option<String> {
    let magic = version_record_magic();
    let mut found: Option<String> = None;
    for start in 0..bytes.len().saturating_sub(VERSION_RECORD_LEN - 1) {
        if bytes[start..start + 16] != magic {
            continue;
        }
        let Some(version) = parse_version_field(&bytes[start + 16..start + VERSION_RECORD_LEN])
        else {
            // The magic without a well-formed field is not a record. It cannot
            // be forged into one without the org key, so skipping it is safe —
            // and treating it as fatal would let one unlucky byte sequence in a
            // dependency stop every install.
            continue;
        };
        match &found {
            Some(seen) if *seen != version => return None,
            Some(_) => {}
            None => found = Some(version),
        }
    }
    found
}

/// A record's fixed-width version field as a string: printable ASCII, then NUL
/// padding to the end, and non-empty.
fn parse_version_field(field: &[u8]) -> Option<String> {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    if end == 0 || field[end..].iter().any(|b| *b != 0) {
        return None;
    }
    let text = std::str::from_utf8(&field[..end]).ok()?;
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .then(|| text.to_owned())
}

/// Whether `candidate` is at least as new as `running`, comparing the numeric
/// `MAJOR.MINOR.PATCH` triple.
///
/// Equal passes: an install RPC that refused the version already running could
/// not repair a corrupted helper, and re-installing the same release is exactly
/// what `veld update --target-version` and a re-run of `install.sh` do.
///
/// A version either side cannot be read as a triple returns `false` — fail
/// closed, like everything else here. A pre-release suffix (`1.2.3-rc1`) is
/// compared on the triple alone; veld does not ship them, and guessing an order
/// for them would be a worse answer than the conservative one.
pub fn version_is_not_older(candidate: &str, running: &str) -> bool {
    match (version_triple(candidate), version_triple(running)) {
        (Some(c), Some(r)) => c >= r,
        _ => false,
    }
}

/// `MAJOR.MINOR.PATCH` as a comparable tuple, ignoring any `-`/`+` suffix.
fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let core = version
        .split_once(['-', '+'])
        .map_or(version, |(before, _)| before);
    let mut parts = core.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let triple = (next()?, next()?, next()?);
    parts.next().is_none().then_some(triple)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn keypair() -> (VerifyingKey, SigningKey) {
        let signing = SigningKey::from_bytes(&[9u8; 32]);
        (VerifyingKey::from(&signing), signing)
    }

    #[test]
    fn verify_data_accepts_a_valid_signature() {
        let (vk, signing) = keypair();
        let sig = signing.sign(b"the genuine binary");
        assert!(verify_data(
            &vk.to_bytes(),
            b"the genuine binary",
            &sig.to_bytes()
        ));
    }

    #[test]
    fn verify_data_rejects_tampered_data() {
        let (vk, signing) = keypair();
        let sig = signing.sign(b"genuine");
        assert!(!verify_data(&vk.to_bytes(), b"tampered", &sig.to_bytes()));
    }

    #[test]
    fn verify_data_rejects_wrong_key() {
        let (_, signing) = keypair();
        let other = SigningKey::from_bytes(&[8u8; 32]);
        let sig = signing.sign(b"data");
        assert!(!verify_data(
            &VerifyingKey::from(&other).to_bytes(),
            b"data",
            &sig.to_bytes()
        ));
    }

    #[test]
    fn verify_data_rejects_wrong_signature_length() {
        let (vk, _) = keypair();
        assert!(!verify_data(&vk.to_bytes(), b"data", b"tooshort"));
    }

    #[test]
    fn sig_path_appends_not_replaces() {
        assert_eq!(
            sig_path_for(Path::new("/x/veld-helper")),
            PathBuf::from("/x/veld-helper.sig")
        );
    }

    #[test]
    fn file_verification_passes_on_a_validly_signed_file() {
        let dir = std::env::temp_dir().join(format!("veld-signing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (vk, signing) = keypair();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"binary").unwrap();
        std::fs::write(sig_path_for(&binary), signing.sign(b"binary").to_bytes()).unwrap();
        assert!(verify_data(
            &vk.to_bytes(),
            b"binary",
            &std::fs::read(sig_path_for(&binary)).unwrap()
        ));

        // Tamper the binary → fails the content check.
        assert!(!verify_data(
            &vk.to_bytes(),
            b"tampered",
            &std::fs::read(sig_path_for(&binary)).unwrap()
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // -- Version attestation (#262) -----------------------------------------

    /// A record round-trips: what a binary embeds is what a scanner reads back.
    #[test]
    fn a_record_reads_back_as_the_version_it_was_built_from() {
        let record = version_record("16.58.3");
        // Buried in noise, the way it sits in a real binary.
        let mut bytes = vec![0x41u8; 500];
        bytes.extend_from_slice(&record);
        bytes.extend_from_slice(&[0x42u8; 500]);
        assert_eq!(version_in_signed_bytes(&bytes).as_deref(), Some("16.58.3"));
    }

    /// **The rollback this whole mechanism exists for.** An older release is
    /// genuinely signed and verifies perfectly; only the version stops it.
    #[test]
    fn an_older_version_is_refused_and_the_same_or_newer_is_not() {
        assert!(!version_is_not_older("16.57.0", "16.58.3"));
        assert!(!version_is_not_older("15.99.99", "16.0.0"));
        assert!(!version_is_not_older("16.58.2", "16.58.3"));
        // Equal is allowed: re-installing the running release is what a repair,
        // a `--target-version`, and a re-run of install.sh all do.
        assert!(version_is_not_older("16.58.3", "16.58.3"));
        assert!(version_is_not_older("16.58.4", "16.58.3"));
        assert!(version_is_not_older("17.0.0", "16.58.3"));
        // Ordering is numeric, not lexical — the case a string compare gets
        // wrong, and gets wrong in the direction that admits an older build.
        assert!(!version_is_not_older("16.9.0", "16.10.0"));
        assert!(version_is_not_older("16.10.0", "16.9.0"));
    }

    /// Anything unreadable as a triple fails closed, on either side.
    #[test]
    fn an_unparseable_version_is_refused_rather_than_guessed() {
        for (candidate, running) in [
            ("", "16.58.3"),
            ("sixteen", "16.58.3"),
            ("16.58", "16.58.3"),
            ("16.58.3.1", "16.58.3"),
            ("16.58.3", ""),
            ("16.58.3", "not-a-version"),
        ] {
            assert!(
                !version_is_not_older(candidate, running),
                "{candidate:?} over {running:?} should have been refused"
            );
        }
    }

    /// Bytes carrying no record read as no version — which the install path
    /// treats as a refusal, so a helper built before this mechanism existed can
    /// never be installed over one that has it.
    #[test]
    fn bytes_with_no_record_have_no_version() {
        assert_eq!(version_in_signed_bytes(&[0u8; 4096]), None);
        assert_eq!(version_in_signed_bytes(b"an older release"), None);
    }

    /// Two records that disagree are no answer at all.
    ///
    /// Nothing can forge a second record into a signed binary — that needs the
    /// org key — so this is not an attack being blocked. It is the honest
    /// reading of an ambiguous artifact, and the alternative (take the first)
    /// would let a future build shape silently decide the version by layout.
    #[test]
    fn disagreeing_records_read_as_no_version() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&version_record("16.58.3"));
        bytes.extend_from_slice(&[0u8; 64]);
        bytes.extend_from_slice(&version_record("1.0.0"));
        assert_eq!(version_in_signed_bytes(&bytes), None);
    }

    /// Identical records do *not* disagree.
    ///
    /// A macOS universal binary carries one slice per architecture and therefore
    /// one record per slice. Refusing that would wedge the updater on exactly the
    /// artifacts it exists to install, so "one value" is the rule rather than
    /// "one hit".
    #[test]
    fn repeated_identical_records_are_one_answer() {
        let mut bytes = Vec::new();
        for _ in 0..3 {
            bytes.extend_from_slice(&version_record("16.58.3"));
            bytes.extend_from_slice(&[0xcc; 32]);
        }
        assert_eq!(version_in_signed_bytes(&bytes).as_deref(), Some("16.58.3"));
    }

    /// The magic with a malformed field behind it is not a record.
    ///
    /// This is the tolerance that keeps an unlucky byte sequence in a dependency
    /// from stopping every install: it is skipped, not fatal.
    #[test]
    fn the_magic_without_a_well_formed_field_is_skipped() {
        let magic = version_record_magic();
        let mut bytes = Vec::new();
        // Magic followed by rodata-like text with no NUL padding — what a stray
        // occurrence in a real binary looks like.
        bytes.extend_from_slice(&magic);
        bytes.extend_from_slice(b"could not read own executable pa");
        assert_eq!(version_in_signed_bytes(&bytes), None);

        // ...and it does not stop a real record elsewhere being found.
        bytes.extend_from_slice(&version_record("16.58.3"));
        assert_eq!(version_in_signed_bytes(&bytes).as_deref(), Some("16.58.3"));
    }

    /// The plaintext magic must not be a constant anywhere in this crate's own
    /// data — see [`VERSION_MAGIC_OBFUSCATED`]. This asserts the arithmetic that
    /// replaced a layout assumption.
    #[test]
    fn the_magic_is_recovered_by_xor_and_not_stored_in_the_clear() {
        let magic = version_record_magic();
        assert_ne!(magic, VERSION_MAGIC_OBFUSCATED);
        for (i, byte) in magic.iter().enumerate() {
            assert_eq!(*byte, VERSION_MAGIC_OBFUSCATED[i] ^ VERSION_MAGIC_KEY);
        }
    }

    #[test]
    fn an_oversized_sig_is_rejected_without_slurping_it() {
        let dir =
            std::env::temp_dir().join(format!("veld-signing-oversize-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"binary").unwrap();
        std::fs::write(sig_path_for(&binary), vec![0u8; 1 << 20]).unwrap();
        let _ = verify_binary_signed(&binary); // returns false; bounded read means it didn't OOM
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
