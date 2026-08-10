//! `veld-sign` — sign a veld helper binary with the org's ed25519 key.
//!
//! Reads a binary and the org private key (PKCS#8 PEM, from an env var — the
//! GitHub `SIGNING_PRIVATE_KEY` secret — or a file), and writes a detached
//! `<binary>.sig` containing the raw 64-byte ed25519 signature.
//!
//! The running root helper (`veld-helper`) verifies that signature against its
//! embedded public key before ever relaunching onto a changed on-disk binary
//! (see `crates/veld-helper/src/signing.rs`). CI runs this over the *final*
//! shipped bytes — on macOS that is the ad-hoc re-signed binary, so install.sh's
//! later re-sign is a byte-idempotent no-op and the `.sig` still matches.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ed25519_dalek::Signer;
use ed25519_dalek::pkcs8::DecodePrivateKey;

const USAGE: &str = "usage: veld-sign --key-env <VAR> | --key-file <path> <binary>";

/// Sign `binary` with `key_pem` (PKCS#8 PEM) and write `<binary>.sig`.
fn sign_file(binary: &Path, key_pem: &str) -> Result<PathBuf, String> {
    let signing_key = ed25519_dalek::SigningKey::from_pkcs8_pem(key_pem)
        .map_err(|e| format!("invalid ed25519 private key: {e}"))?;
    let data =
        std::fs::read(binary).map_err(|e| format!("cannot read {}: {e}", binary.display()))?;
    let sig = signing_key.sign(&data);
    let sig_path = sig_path_for(binary);
    std::fs::write(&sig_path, sig.to_bytes())
        .map_err(|e| format!("cannot write {}: {e}", sig_path.display()))?;
    Ok(sig_path)
}

/// `<path>.sig` — append, not replace any existing extension.
fn sig_path_for(binary: &Path) -> PathBuf {
    let mut s = binary.as_os_str().to_os_string();
    s.push(".sig");
    PathBuf::from(s)
}

fn main() -> ExitCode {
    let mut key_env: Option<String> = None;
    let mut key_file: Option<PathBuf> = None;
    let mut binary: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key-env" => match args.next() {
                Some(v) => key_env = Some(v),
                None => return usage("--key-env requires a value"),
            },
            "--key-file" => match args.next() {
                Some(v) => key_file = Some(PathBuf::from(v)),
                None => return usage("--key-file requires a value"),
            },
            other if other.starts_with('-') => return usage(&format!("unknown flag: {other}")),
            other => binary = Some(PathBuf::from(other)),
        }
    }

    let binary = match binary {
        Some(b) => b,
        None => return usage("missing <binary> path"),
    };

    let key_pem = match (&key_env, &key_file) {
        (Some(env), _) => match std::env::var(env) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: cannot read env {env}: {e}");
                return ExitCode::from(2);
            }
        },
        (None, Some(f)) => match std::fs::read_to_string(f) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: cannot read key file {}: {e}", f.display());
                return ExitCode::from(2);
            }
        },
        (None, None) => return usage("provide --key-env or --key-file"),
    };

    match sign_file(&binary, &key_pem) {
        Ok(sig_path) => {
            eprintln!("signed {} -> {}", binary.display(), sig_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("error: {msg}\n{USAGE}");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Verifier, VerifyingKey};

    // A throwaway ed25519 key (NOT the org key), generated with `openssl
    // genpkey -algorithm ED25519` — the exact format `veld update`/CI feed in
    // via the `SIGNING_PRIVATE_KEY` secret. Pins the production parse path.
    const TEST_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
        MC4CAQAwBQYDK2VwBCIEIG6yQcLcN3khrsV3dAHJmX/loSUSEoU9FVYNd4mqV+S1\n\
        -----END PRIVATE KEY-----\n";
    const TEST_PUBLIC: [u8; 32] = [
        0x57, 0x13, 0x06, 0xbc, 0x2b, 0xda, 0x86, 0xf9, 0x38, 0x55, 0xa7, 0xea, 0xda, 0xa7, 0x21,
        0x74, 0x11, 0x67, 0x09, 0x6c, 0xea, 0xb7, 0x03, 0x11, 0xd2, 0xf7, 0xd4, 0x33, 0x03, 0x0a,
        0xf0, 0xc7,
    ];

    #[test]
    fn sign_file_writes_a_verifiable_detached_signature() {
        let dir = std::env::temp_dir().join(format!("veld-sign-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"macho/elf payload bytes").unwrap();

        let sig_path = sign_file(&binary, TEST_PRIVATE_PEM).unwrap();
        assert_eq!(sig_path, sig_path_for(&binary));

        let sig = std::fs::read(&sig_path).unwrap();
        let vk = VerifyingKey::from_bytes(&TEST_PUBLIC).unwrap();
        let sig = ed25519_dalek::Signature::from_slice(&sig).unwrap();
        assert!(vk.verify(b"macho/elf payload bytes", &sig).is_ok());
        assert!(vk.verify(b"tampered", &sig).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sig_path_appends_not_replaces() {
        assert_eq!(
            sig_path_for(Path::new("/x/veld-helper")),
            PathBuf::from("/x/veld-helper.sig")
        );
    }
}
