//! Joiner-side cache of relay auth tokens, keyed by relay URL.
//!
//! When a joiner supplies a token for a token-gated relay (interactively, via
//! the CLI prompt or the management UI), it is cached here so future joins to
//! the same relay don't re-prompt. Stored in the central veld database
//! (`relay_tokens` table), which is `0600` like the node key — it holds
//! secrets.
//!
//! The cache is best-effort: an unreadable database reads as empty rather
//! than failing a join. Keys are canonical [`iroh::RelayUrl`] strings so they
//! match the join-side lookup (see `RelayChoice::resolve_join_tokens`).

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use veld_core::db::Db;

/// Load the cached tokens (relay URL → token). An unreadable database reads
/// as an empty map — the cache must never break joining.
pub fn load() -> BTreeMap<String, String> {
    Db::open().map(|db| db.relay_tokens()).unwrap_or_default()
}

/// Persist `token` for `url`, merging into the existing cache.
pub fn save(url: &str, token: &str) -> Result<()> {
    let db = Db::open().context("opening the veld database for the relay token cache")?;
    db.save_relay_token(url, token)
        .context("saving relay token")?;
    Ok(())
}
