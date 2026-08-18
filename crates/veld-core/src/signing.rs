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

/// Whether `binary` carries a valid org signature in `<binary>.sig`.
///
/// Bounds the `.sig` read to 64 bytes (a valid ed25519 signature is exactly
/// that): the file sits in the user-writable lib dir, so a huge one must not be
/// slurped into memory here (a memory DoS on every relaunch attempt or doctor
/// run). `false` for any failure.
pub fn verify_binary_signed(binary: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(sig_path_for(binary)) else {
        return false;
    };
    let mut sig = [0u8; 64];
    let n = match file.read(&mut sig) {
        Ok(n) => n,
        Err(_) => return false,
    };
    if n != 64 {
        return false;
    }
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
