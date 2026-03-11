//! Sensitive variable encryption for Veld.
//!
//! Values marked as sensitive outputs are:
//! - Masked as `[REDACTED]` in all terminal output, debug logs, and run logs
//! - Stored encrypted at rest in `state.json` using a machine-local key
//! - Never visible in `veld graph` output
//!
//! V1 uses XOR-based obfuscation with a SHA-256 key derived from the machine's
//! hardware UUID. This protects against casual inspection of state files.

use base64::prelude::*;
use sha2::{Digest, Sha256};

/// Prefix added to encrypted values in state.json so we can identify them.
const ENCRYPTED_PREFIX: &str = "veld:enc:";

/// The display string for redacted sensitive values.
pub const REDACTED: &str = "[REDACTED]";

/// Derive a 32-byte key from the machine's hardware UUID.
///
/// - macOS: reads `IOPlatformUUID` via `ioreg`
/// - Linux: reads `/etc/machine-id`
/// - Fallback: uses a static salt (still provides obfuscation, just not
///   machine-bound)
pub fn get_machine_key() -> [u8; 32] {
    let uuid = get_machine_uuid();
    let mut hasher = Sha256::new();
    hasher.update(b"veld-sensitive-v1:");
    hasher.update(uuid.as_bytes());
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// Encrypt a plaintext value using XOR with the machine key.
/// Returns a string prefixed with `veld:enc:` followed by base64.
pub fn encrypt_value(plaintext: &str) -> String {
    let key = get_machine_key();
    let encrypted = xor_cipher(plaintext.as_bytes(), &key);
    let encoded = BASE64_STANDARD.encode(&encrypted);
    format!("{ENCRYPTED_PREFIX}{encoded}")
}

/// Decrypt a value that was encrypted with `encrypt_value`.
/// If the value does not have the encrypted prefix, returns it as-is.
pub fn decrypt_value(ciphertext: &str) -> String {
    if let Some(encoded) = ciphertext.strip_prefix(ENCRYPTED_PREFIX) {
        if let Ok(encrypted) = BASE64_STANDARD.decode(encoded) {
            let key = get_machine_key();
            let decrypted = xor_cipher(&encrypted, &key);
            if let Ok(s) = String::from_utf8(decrypted) {
                return s;
            }
        }
    }
    // If decryption fails or value is not encrypted, return as-is.
    ciphertext.to_owned()
}

/// Returns `true` if the value is an encrypted sensitive value.
pub fn is_encrypted(value: &str) -> bool {
    value.starts_with(ENCRYPTED_PREFIX)
}

/// Mask a value for display purposes.
pub fn mask_value(_value: &str) -> String {
    REDACTED.to_owned()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// XOR cipher: repeats the key to match the data length.
fn xor_cipher(data: &[u8], key: &[u8]) -> Vec<u8> {
    data.iter()
        .enumerate()
        .map(|(i, &b)| b ^ key[i % key.len()])
        .collect()
}

/// Get the machine UUID string.
fn get_machine_uuid() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Some(uuid) = macos_hardware_uuid() {
            return uuid;
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
            let trimmed = id.trim().to_owned();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
    }

    // Fallback: static salt. Still provides obfuscation but not machine-bound.
    "veld-fallback-machine-id-v1".to_owned()
}

#[cfg(target_os = "macos")]
fn macos_hardware_uuid() -> Option<String> {
    let output = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("IOPlatformUUID") {
            // Line looks like: "IOPlatformUUID" = "XXXXXXXX-XXXX-..."
            if let Some(start) = line.rfind('"') {
                let before = &line[..start];
                if let Some(quote_start) = before.rfind('"') {
                    let uuid = &line[quote_start + 1..start];
                    if !uuid.is_empty() {
                        return Some(uuid.to_owned());
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let original = "my-secret-api-key-12345";
        let encrypted = encrypt_value(original);
        assert!(encrypted.starts_with(ENCRYPTED_PREFIX));
        assert_ne!(encrypted, original);

        let decrypted = decrypt_value(&encrypted);
        assert_eq!(decrypted, original);
    }

    #[test]
    fn test_decrypt_unencrypted_returns_as_is() {
        let plain = "just-a-regular-value";
        assert_eq!(decrypt_value(plain), plain);
    }

    #[test]
    fn test_is_encrypted() {
        assert!(is_encrypted("veld:enc:abc123"));
        assert!(!is_encrypted("plain-value"));
    }

    #[test]
    fn test_mask_value() {
        assert_eq!(mask_value("anything"), REDACTED);
    }

    #[test]
    fn test_encrypt_empty_string() {
        let encrypted = encrypt_value("");
        let decrypted = decrypt_value(&encrypted);
        assert_eq!(decrypted, "");
    }
}
