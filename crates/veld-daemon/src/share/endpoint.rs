//! iroh endpoint construction and the persistent node identity.
//!
//! The node's ed25519 secret key is persisted once under the platform data
//! directory so the node's public key (its `EndpointId`) is stable across daemon
//! restarts. Everything else about a share is ephemeral in-memory state.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use iroh::endpoint::presets;
use iroh::{Endpoint, SecretKey};

/// ALPN protocol identifier for veld's share tunnels.
pub const ALPN: &[u8] = b"veld/share/1";

/// Path to the persistent node key: `<data_dir>/veld/node.key`.
pub fn key_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("veld").join("node.key"))
}

/// Load the node's secret key from `path`, generating and persisting one if it
/// does not yet exist. The file holds the raw 32-byte ed25519 secret.
pub fn load_or_create_secret_key(path: &Path) -> Result<SecretKey> {
    if path.exists() {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading node key {}", path.display()))?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("node key {} is not 32 bytes", path.display()))?;
        Ok(SecretKey::from_bytes(&arr))
    } else {
        let secret = SecretKey::generate();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(path, secret.to_bytes())
            .with_context(|| format!("writing node key {}", path.display()))?;
        restrict_permissions(path);
        Ok(secret)
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// Bind an iroh endpoint using n0's default relays + discovery, advertising the
/// veld share ALPN. The endpoint accepts inbound share connections and can dial
/// out to peers.
pub async fn bind_endpoint(secret_key: SecretKey) -> Result<Endpoint> {
    Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .context("binding iroh endpoint")
}
