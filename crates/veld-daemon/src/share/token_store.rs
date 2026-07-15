//! Joiner-side cache of relay auth tokens, keyed by relay URL.
//!
//! When a joiner supplies a token for a token-gated relay (interactively, via
//! the CLI prompt or the management UI), it is cached here so future joins to
//! the same relay don't re-prompt. Stored at
//! `<data_dir>/veld/relay-tokens.json`, permission-restricted to `0600` like the
//! node key — it holds secrets.
//!
//! The cache is best-effort: a missing or corrupt file reads as empty rather
//! than failing a join. Keys are canonical [`iroh::RelayUrl`] strings so they
//! match the join-side lookup (see `RelayChoice::resolve_join_tokens`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Path to the joiner's relay-token cache: `<data_dir>/veld/relay-tokens.json`.
pub fn path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("veld").join("relay-tokens.json"))
}

/// Load the cached tokens (relay URL → token). A missing or unreadable/corrupt
/// file reads as an empty map — the cache must never break joining.
pub fn load() -> BTreeMap<String, String> {
    path().map(|p| load_from(&p)).unwrap_or_default()
}

/// Persist `token` for `url`, merging into the existing cache. Writes the file
/// `0600` since it holds secrets.
pub fn save(url: &str, token: &str) -> Result<()> {
    let p = path().context("no platform data directory for the relay token cache")?;
    save_to(&p, url, token)
}

fn load_from(p: &Path) -> BTreeMap<String, String> {
    let Ok(bytes) = std::fs::read(p) else {
        return BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_to(p: &Path, url: &str, token: &str) -> Result<()> {
    let mut map = load_from(p);
    map.insert(url.to_owned(), token.to_owned());
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(&map).context("serializing relay token cache")?;
    write_private(p, &json).with_context(|| format!("writing {}", p.display()))?;
    Ok(())
}

/// Write `bytes` to `p`, creating the file `0600` **up front** so the secret is
/// never briefly world-readable (a plain write + later chmod would leave a
/// window at the umask default). Also re-restricts an existing file's mode,
/// since `create` does not reset the mode of a file that already exists.
#[cfg(unix)]
fn write_private(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(p)?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(p, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("relay-tokens.json");
        assert!(load_from(&p).is_empty());
    }

    #[test]
    fn save_then_load_round_trips_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("relay-tokens.json");
        save_to(&p, "https://a.example/", "tok-a").unwrap();
        save_to(&p, "https://b.example/", "tok-b").unwrap();
        // Overwrite a key.
        save_to(&p, "https://a.example/", "tok-a2").unwrap();
        let map = load_from(&p);
        assert_eq!(
            map.get("https://a.example/").map(String::as_str),
            Some("tok-a2")
        );
        assert_eq!(
            map.get("https://b.example/").map(String::as_str),
            Some("tok-b")
        );
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("relay-tokens.json");
        save_to(&p, "https://a.example/", "tok").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token cache must be private");
    }

    #[test]
    fn corrupt_file_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("relay-tokens.json");
        std::fs::write(&p, b"not json").unwrap();
        assert!(load_from(&p).is_empty());
    }
}
