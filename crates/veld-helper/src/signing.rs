//! Verify the on-disk helper binary against the org's ed25519 public key.
//!
//! The privileged helper runs as root but lives in a **user-writable**
//! directory, and it relaunches itself (binary watcher, `restart`, `shutdown`)
//! so the service manager can pick up a new version. That is the escalation
//! from #247: any process with the installing user's privileges can swap the
//! binary and get root on the next relaunch. The peer-credential gate
//! (`main.rs`) closes "any local process can drive the socket"; this closes
//! "a swapped binary gets executed as root."
//!
//! The org's public key is embedded here at build time. The matching private
//! key lives only in the org vault and the GitHub `SIGNING_PRIVATE_KEY` secret;
//! CI signs each release binary with it and ships a detached `<binary>.sig`.
//! The *running* helper — the genuine, org-signed binary already in memory —
//! verifies the on-disk binary against its embedded key before it will ever
//! `exit(0)` onto a replacement. An attacker cannot re-pin that key: it lives
//! in the running process, not in any file they can write, and they cannot
//! forge a signature without the private key.
//!
//! Verification is **fail-closed**: any failure to read, a missing/wrong-length
//! `.sig`, or a mismatched signature returns false, and every caller refuses to
//! relaunch rather than assume safety. A delete-only attack (strip the `.sig`)
//! is a denial-of-service, not an escalation — the helper just stays on the
//! genuine in-memory binary until repaired.

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};

/// The org's ed25519 public key, raw 32 bytes. Public — embedded at build time.
/// Derived from `notes/veld-signing-ed25519.pub` (SPKI); the private key lives
/// in the org vault and the GitHub `SIGNING_PRIVATE_KEY` secret.
const ORG_SIGNING_PUBKEY: [u8; 32] = [
    0x9d, 0x75, 0xa4, 0xc5, 0x5c, 0x02, 0xb4, 0x6e, 0x53, 0xa3, 0x0d, 0x1d, 0xf8, 0x84, 0xc8, 0xaa,
    0xf0, 0xd3, 0x90, 0x23, 0x06, 0xd1, 0xc6, 0xee, 0x53, 0x60, 0x32, 0x99, 0xa3, 0x1b, 0x31, 0x56,
];

/// Whether `data` carries a valid ed25519 signature by `pubkey` in `sig`.
fn verify_data(pubkey: &[u8; 32], data: &[u8], sig: &[u8]) -> bool {
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
pub(crate) fn sig_path_for(binary: &Path) -> PathBuf {
    let mut s = binary.as_os_str().to_os_string();
    s.push(".sig");
    PathBuf::from(s)
}

/// Whether the helper binary at `exe` carries a valid org signature in
/// `<exe>.sig`. The enforcement primitive: `false` for any failure — missing,
/// unreadable, wrong length, or mismatched — never assumed true.
pub(crate) fn verify_own_binary(exe: &Path) -> bool {
    verify_own_binary_with(&ORG_SIGNING_PUBKEY, exe)
}

/// [`verify_own_binary`] with an explicit key, so the file-verification logic
/// (not just the crypto primitive) is testable without the org private key.
pub(crate) fn verify_own_binary_with(pubkey: &[u8; 32], exe: &Path) -> bool {
    let Ok(sig) = std::fs::read(sig_path_for(exe)) else {
        return false;
    };
    let Ok(data) = std::fs::read(exe) else {
        return false;
    };
    verify_data(pubkey, &data, &sig)
}

/// A reason to refuse relaunching onto the on-disk helper binary, or `None`
/// when it verifies. Wraps [`verify_own_binary`] with a diagnosable message for
/// the fail-closed callers (the binary watcher, `restart`, and `shutdown`).
pub(crate) fn relaunch_guard(exe: &Path) -> Option<String> {
    if verify_own_binary(exe) {
        return None;
    }
    Some(format!(
        "the binary at {} is not signed with the org's key (or its {} is \
         missing/invalid); refusing to relaunch onto it",
        exe.display(),
        sig_path_for(exe).display()
    ))
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
        let data = b"the genuine binary";
        let sig = signing.sign(data);
        assert!(verify_data(&vk.to_bytes(), data, &sig.to_bytes()));
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
        assert!(verify_own_binary_with(&vk.to_bytes(), &binary));

        // Tamper the binary → fails.
        std::fs::write(&binary, b"binary-tampered").unwrap();
        assert!(!verify_own_binary_with(&vk.to_bytes(), &binary));

        // Delete the .sig → fails (fail-closed, never assumed safe).
        std::fs::remove_file(sig_path_for(&binary)).unwrap();
        assert!(!verify_own_binary_with(&vk.to_bytes(), &binary));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
