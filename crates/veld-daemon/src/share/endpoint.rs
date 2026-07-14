//! iroh endpoint construction and the persistent node identity.
//!
//! The node's ed25519 secret key is persisted once under the platform data
//! directory so the node's public key (its `EndpointId`) is stable across daemon
//! restarts. Everything else about a share is ephemeral in-memory state.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use iroh::endpoint::presets;
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey};
use tracing::warn;
use veld_core::config::RelayPolicy;

/// ALPN protocol identifier for veld's share tunnels.
pub const ALPN: &[u8] = b"veld/share/1";

/// Env var to point the endpoint at a self-hosted relay instead of n0's public
/// relays (e.g. `VELD_SHARE_RELAY=https://relay.example.com`). Only consulted
/// when a project does not declare `sharing.relays` in its config.
const RELAY_ENV: &str = "VELD_SHARE_RELAY";

/// The concrete relay decision an endpoint is bound with. Derived from a
/// project's `sharing.relays` policy, falling back to the `VELD_SHARE_RELAY`
/// env override, or `None` when nothing is opted in (relays are never chosen
/// implicitly). Used as the key of the daemon's per-policy endpoint map (see
/// `ShareManager`), so each distinct choice gets its own endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RelayChoice {
    /// n0's public relays (only via an explicit `"public"` opt-in).
    Public,
    /// Self-hosted relay URLs, sorted for a stable identity/comparison.
    Custom(Vec<String>),
}

impl RelayChoice {
    /// Resolve the effective relay choice, or `None` if no relay is opted into.
    /// Relays must be chosen explicitly — including public — so nothing is ever
    /// routed over n0's public relays by accident. Config policy wins; when
    /// absent, the `VELD_SHARE_RELAY` env override applies; otherwise `None`.
    ///
    /// The env var is read from the *daemon* process's environment at bind time,
    /// not the shell that ran `veld share` — a long-lived daemon won't see an
    /// export made after it started. Prefer `sharing.relays` in config.
    pub fn resolve(policy: Option<&RelayPolicy>) -> Option<Self> {
        Self::resolve_with_env(policy, std::env::var(RELAY_ENV).ok())
    }

    /// Core of `resolve`, with the env override injected so it can be unit-tested
    /// without mutating the process environment (which would be a data race under
    /// multithreaded `cargo test`).
    fn resolve_with_env(policy: Option<&RelayPolicy>, env: Option<String>) -> Option<Self> {
        match policy {
            Some(RelayPolicy::Public) => Some(RelayChoice::Public),
            Some(RelayPolicy::Custom(urls)) => Some(RelayChoice::custom(urls.clone())),
            None => match env {
                Some(raw) if !raw.trim().is_empty() => {
                    Some(RelayChoice::custom(vec![raw.trim().to_owned()]))
                }
                _ => None,
            },
        }
    }

    fn custom(urls: Vec<String>) -> Self {
        // Normalize (trim + drop a trailing slash) before sort/dedup so the
        // choice has a stable identity: it keys the per-policy endpoint map, so
        // `https://r` and `https://r/` must map to the same endpoint. Case and
        // default-port differences are left as-is.
        let mut urls: Vec<String> = urls
            .into_iter()
            .map(|u| u.trim().trim_end_matches('/').to_owned())
            .collect();
        urls.sort();
        urls.dedup();
        RelayChoice::Custom(urls)
    }
}

impl fmt::Display for RelayChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RelayChoice::Public => write!(f, "public"),
            RelayChoice::Custom(urls) => write!(f, "[{}]", urls.join(", ")),
        }
    }
}

/// Path to the persistent node key: `<data_dir>/veld/node.key`.
pub fn key_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("veld").join("node.key"))
}

/// Load the node's secret key from `path`, generating and persisting one if it
/// does not yet exist. The file holds the raw 32-byte ed25519 secret.
pub fn load_or_create_secret_key(path: &Path) -> Result<SecretKey> {
    if path.exists() {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading node key {}", path.display()))?;
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

/// Bind an iroh endpoint advertising the veld share ALPN, routing through the
/// relays named by `choice`. The endpoint accepts inbound share connections and
/// can dial out to peers.
///
/// For a custom relay choice, every URL that fails to parse is dropped with a
/// warning; if *no* URL survives, binding fails rather than silently falling
/// back to public relays — a silent fallback would violate the compliance
/// intent of pinning relays.
pub async fn bind_endpoint(secret_key: SecretKey, choice: &RelayChoice) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![ALPN.to_vec()]);

    if let RelayChoice::Custom(urls) = choice {
        let parsed: Vec<RelayUrl> = urls
            .iter()
            .filter_map(|raw| match raw.parse::<RelayUrl>() {
                Ok(url) => Some(url),
                Err(e) => {
                    warn!(error = %e, value = %raw, "ignoring invalid share relay URL");
                    None
                }
            })
            .collect();
        if parsed.is_empty() {
            bail!(
                "no valid relay URLs to bind ({}); set via sharing.relays in veld.json or \
                 the VELD_SHARE_RELAY env var. Refusing to fall back to public relays.",
                urls.join(", ")
            );
        }
        builder = builder.relay_mode(RelayMode::custom(parsed));
    }

    builder.bind().await.context("binding iroh endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_public_policy() {
        assert_eq!(
            RelayChoice::resolve(Some(&RelayPolicy::Public)),
            Some(RelayChoice::Public)
        );
    }

    #[test]
    fn resolve_custom_policy_sorts_and_dedups() {
        let policy = RelayPolicy::Custom(vec![
            "https://b.example".into(),
            "https://a.example".into(),
            "https://b.example".into(),
        ]);
        assert_eq!(
            RelayChoice::resolve(Some(&policy)),
            Some(RelayChoice::Custom(vec![
                "https://a.example".into(),
                "https://b.example".into()
            ]))
        );
    }

    #[test]
    fn custom_choices_compare_regardless_of_input_order() {
        let a = RelayChoice::resolve(Some(&RelayPolicy::Custom(vec![
            "https://x".into(),
            "https://y".into(),
        ])));
        let b = RelayChoice::resolve(Some(&RelayPolicy::Custom(vec![
            "https://y".into(),
            "https://x".into(),
        ])));
        assert_eq!(a, b);
    }

    #[test]
    fn custom_normalizes_trailing_slash_and_whitespace() {
        let a = RelayChoice::resolve(Some(&RelayPolicy::Custom(vec![
            "https://relay.example/".into(),
        ])));
        let b = RelayChoice::resolve(Some(&RelayPolicy::Custom(vec![
            " https://relay.example ".into(),
        ])));
        assert_eq!(a, b);
        assert_eq!(
            a,
            Some(RelayChoice::Custom(vec!["https://relay.example".into()]))
        );
    }

    #[test]
    fn config_policy_wins_over_env_var() {
        let env = Some("https://env-relay.example".to_owned());
        // A `Some(..)` policy never consults the env, so config always wins.
        assert_eq!(
            RelayChoice::resolve_with_env(Some(&RelayPolicy::Public), env.clone()),
            Some(RelayChoice::Public)
        );
        assert_eq!(
            RelayChoice::resolve_with_env(
                Some(&RelayPolicy::Custom(vec!["https://cfg.example".into()])),
                env.clone()
            ),
            Some(RelayChoice::Custom(vec!["https://cfg.example".into()]))
        );
        // With no config policy, the env override is consulted.
        assert_eq!(
            RelayChoice::resolve_with_env(None, env),
            Some(RelayChoice::Custom(vec![
                "https://env-relay.example".into()
            ]))
        );
    }

    #[test]
    fn resolve_requires_explicit_opt_in() {
        // No config policy and no env override → nothing is opted in; never
        // falls back to public relays implicitly.
        assert_eq!(RelayChoice::resolve_with_env(None, None), None);
        // Blank / whitespace-only env is ignored, not treated as an opt-in.
        assert_eq!(
            RelayChoice::resolve_with_env(None, Some("   ".into())),
            None
        );
    }

    #[test]
    fn display_renders_choice() {
        assert_eq!(RelayChoice::Public.to_string(), "public");
        assert_eq!(
            RelayChoice::Custom(vec!["https://a".into(), "https://b".into()]).to_string(),
            "[https://a, https://b]"
        );
    }
}
