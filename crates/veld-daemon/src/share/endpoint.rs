//! iroh endpoint construction and the persistent node identity.
//!
//! The node's ed25519 secret key is persisted once under the platform data
//! directory so the node's public key (its `EndpointId`) is stable across daemon
//! restarts. Everything else about a share is ephemeral in-memory state.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::endpoint::presets;
use iroh::{Endpoint, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey};
use tracing::warn;
use veld_core::config::{RelayEntry, RelayPolicy, SecretSource};

/// ALPN protocol identifier for veld's share tunnels.
pub const ALPN: &[u8] = b"veld/share/1";

/// Env var to point the endpoint at a self-hosted relay instead of n0's public
/// relays (e.g. `VELD_SHARE_RELAY=https://relay.example.com`). Only consulted
/// when a project does not declare `sharing.relays` in its config.
const RELAY_ENV: &str = "VELD_SHARE_RELAY";

/// Env var holding the authorization token for the `VELD_SHARE_RELAY` relay.
/// Only consulted alongside `RELAY_ENV` (the config path carries its own
/// per-relay tokens). Read from the daemon's environment at bind time.
const RELAY_TOKEN_ENV: &str = "VELD_SHARE_RELAY_TOKEN";

/// Upper bound on how long resolving a relay token may take from a source that
/// can block on external I/O — a `command` (hung secret-manager CLI, network
/// stall reaching a vault) or a `file` (a FIFO with no writer, a hung network
/// mount). Token resolution runs on the share/bind path (see `ShareManager`), so
/// neither must wedge sharing indefinitely — it fails after this instead.
const TOKEN_RESOLVE_TIMEOUT: Duration = Duration::from_secs(20);

/// The concrete relay decision an endpoint is bound with. Derived from a
/// project's `sharing.relays` policy, falling back to the `VELD_SHARE_RELAY`
/// env override, or `None` when nothing is opted in (relays are never chosen
/// implicitly). Used as the key of the daemon's per-policy endpoint map (see
/// `ShareManager`), so each distinct choice gets its own endpoint.
///
/// The `Custom` variant carries the full [`RelayEntry`] list (URL + optional
/// token *declaration*), sorted by URL for a stable identity. Tokens are held
/// unresolved here — the command/file/env is only read at `bind_endpoint` time —
/// so this stays a cheap, hashable map key and no secret value lives in it. Two
/// configs that differ only in their token declaration key distinct endpoints;
/// rotating the *underlying* secret behind an unchanged declaration reuses the
/// already-bound endpoint until the daemon restarts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RelayChoice {
    /// n0's public relays (only via an explicit `"public"` opt-in).
    Public,
    /// Self-hosted relays, sorted by URL for a stable identity/comparison.
    Custom(Vec<RelayEntry>),
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
        Self::resolve_with_env(
            policy,
            std::env::var(RELAY_ENV).ok(),
            std::env::var(RELAY_TOKEN_ENV).ok(),
        )
    }

    /// Core of `resolve`, with the env overrides injected so it can be unit-tested
    /// without mutating the process environment (which would be a data race under
    /// multithreaded `cargo test`).
    fn resolve_with_env(
        policy: Option<&RelayPolicy>,
        relay_env: Option<String>,
        token_env: Option<String>,
    ) -> Option<Self> {
        match policy {
            Some(RelayPolicy::Public) => Some(RelayChoice::Public),
            Some(RelayPolicy::Custom(entries)) => Some(RelayChoice::custom(entries.clone())),
            None => match relay_env {
                Some(raw) if !raw.trim().is_empty() => {
                    let token = token_env
                        .map(|t| t.trim().to_owned())
                        .filter(|t| !t.is_empty())
                        .map(SecretSource::Literal);
                    Some(RelayChoice::custom(vec![RelayEntry {
                        url: raw.trim().to_owned(),
                        token,
                    }]))
                }
                _ => None,
            },
        }
    }

    /// Derive the relay choice for the *consumer* side of a join, mirroring the
    /// relay(s) the host advertised in its ticket. A share minted on a custom
    /// relay must be joined over that same relay — never silently over n0's
    /// public relays — so the join is confined to exactly the ticket's relays.
    /// When the ticket carries no relay URL at all — a host reachable only via
    /// direct addresses — this falls back to public. (A public-relay host still
    /// advertises its relay URL in the ticket, so it takes the mirror path above,
    /// not this fallback; and a custom-relay host refuses to mint a relay-less
    /// ticket, see `ShareManager::start_share`.)
    ///
    /// Tickets carry relay URLs but never tokens (a ticket is a shareable link;
    /// a token in it would leak). A token is attached to a relay only when the
    /// joiner has explicitly configured that *same* relay via the
    /// `VELD_SHARE_RELAY` / `VELD_SHARE_RELAY_TOKEN` env pair — matched by parsed
    /// [`RelayUrl`] equality, not string comparison — so a hostile ticket naming
    /// an attacker-controlled relay cannot harvest the joiner's token.
    pub fn for_join<'a>(ticket_relay_urls: impl IntoIterator<Item = &'a RelayUrl>) -> Self {
        Self::for_join_with_env(
            ticket_relay_urls,
            std::env::var(RELAY_ENV).ok(),
            std::env::var(RELAY_TOKEN_ENV).ok(),
        )
    }

    /// Core of [`for_join`](Self::for_join), with the env overrides injected so
    /// it can be unit-tested without mutating the process environment.
    fn for_join_with_env<'a>(
        ticket_relay_urls: impl IntoIterator<Item = &'a RelayUrl>,
        env_relay: Option<String>,
        env_token: Option<String>,
    ) -> Self {
        // The relay the joiner explicitly configured (if any), parsed so the
        // comparison against the ticket's RelayUrl is canonical rather than
        // string-wise (RelayUrl normalizes host case, trailing dot/slash, etc.).
        let configured: Option<RelayUrl> = env_relay
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| match s.parse() {
                Ok(url) => Some(url),
                Err(e) => {
                    // A set-but-unparseable VELD_SHARE_RELAY means the joiner's
                    // token can't be matched to any ticket relay and is silently
                    // dropped (the join then fails against a token-gated relay).
                    // Surface it rather than fail mysteriously.
                    warn!(error = %e, value = %s, "ignoring invalid VELD_SHARE_RELAY; relay token will not be attached");
                    None
                }
            });
        let token: Option<String> = env_token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);

        let entries: Vec<RelayEntry> = ticket_relay_urls
            .into_iter()
            .map(|u| {
                // Attach the token only to the relay the joiner explicitly named.
                let tok = match &token {
                    Some(t) if configured.as_ref() == Some(u) => {
                        Some(SecretSource::Literal(t.clone()))
                    }
                    _ => None,
                };
                RelayEntry {
                    url: u.to_string(),
                    token: tok,
                }
            })
            .collect();

        if entries.is_empty() {
            RelayChoice::Public
        } else {
            RelayChoice::custom(entries)
        }
    }

    fn custom(entries: Vec<RelayEntry>) -> Self {
        // Normalize each URL (trim + drop a trailing slash) then sort/dedup by
        // URL so the choice has a stable identity: it keys the per-policy
        // endpoint map, so `https://r` and `https://r/` must map to the same
        // endpoint. Case and default-port differences are left as-is. On a
        // duplicate URL the first entry's token wins (dedup keeps the earlier).
        let mut entries: Vec<RelayEntry> = entries
            .into_iter()
            .map(|mut e| {
                e.url = e.url.trim().trim_end_matches('/').to_owned();
                e
            })
            .collect();
        entries.sort_by(|a, b| a.url.cmp(&b.url));
        entries.dedup_by(|a, b| a.url == b.url);
        RelayChoice::Custom(entries)
    }
}

impl fmt::Display for RelayChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RelayChoice::Public => write!(f, "public"),
            // URLs only — never render token declarations here (this feeds logs).
            RelayChoice::Custom(entries) => {
                let urls: Vec<&str> = entries.iter().map(|e| e.url.as_str()).collect();
                write!(f, "[{}]", urls.join(", "))
            }
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
/// intent of pinning relays. Each surviving relay's token declaration is
/// resolved here (env / file / command) and attached as the relay's
/// authorization token; a token that fails to resolve is a hard error — we never
/// bind unauthenticated when a token was declared. A relay whose URL fails to
/// parse is skipped *before* its token is resolved, so no token command runs for
/// a dead URL.
///
/// Note: the resolved token is moved into iroh's `RelayConfig`, whose derived
/// `Debug` prints `auth_token` in the clear. Veld's own types redact it (see
/// `SecretSource`'s `Debug`) and never log the built `RelayConfig`/`RelayMap` —
/// keep it that way (no `debug!(?config)` on these).
pub async fn bind_endpoint(secret_key: SecretKey, choice: &RelayChoice) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![ALPN.to_vec()]);

    if let RelayChoice::Custom(entries) = choice {
        let mut configs: Vec<RelayConfig> = Vec::new();
        for entry in entries {
            let url = match entry.url.parse::<RelayUrl>() {
                Ok(url) => url,
                Err(e) => {
                    warn!(error = %e, value = %entry.url, "ignoring invalid share relay URL");
                    continue;
                }
            };
            let mut config = RelayConfig::from(url);
            if let Some(source) = &entry.token {
                let token = resolve_secret(source)
                    .await
                    .with_context(|| format!("resolving relay auth token for {}", entry.url))?;
                config = config.with_auth_token(token);
            }
            configs.push(config);
        }
        if configs.is_empty() {
            let urls: Vec<&str> = entries.iter().map(|e| e.url.as_str()).collect();
            bail!(
                "no valid relay URLs to bind ({}); set via sharing.relays in veld.json or \
                 the VELD_SHARE_RELAY env var. Refusing to fall back to public relays.",
                urls.join(", ")
            );
        }
        builder = builder.relay_mode(RelayMode::Custom(RelayMap::from_iter(configs)));
    }

    builder.bind().await.context("binding iroh endpoint")
}

/// Resolve a [`SecretSource`] into the actual secret string at use time.
///
/// All forms trim trailing whitespace, since secret stores commonly append a
/// newline (`op read`, a `printf`'d file, a Kubernetes `envFrom` value):
///
/// - `Literal` is returned as-is apart from that trim.
/// - `Env` reads a process environment variable (the daemon's, not the caller's
///   shell).
/// - `File` reads a file, bounded by [`TOKEN_RESOLVE_TIMEOUT`]. A relative path
///   resolves against the *daemon's* working directory, not the project — prefer
///   an absolute path (the doc examples use `/run/secrets/…`).
/// - `Command` runs the string through `sh -c` and takes its stdout, bounded by
///   [`TOKEN_RESOLVE_TIMEOUT`]. The child is killed if that bound elapses.
///
/// A resolved-but-empty secret is treated as a misconfiguration and errors,
/// rather than silently sending an empty `Authorization: Bearer` header.
async fn resolve_secret(source: &SecretSource) -> Result<String> {
    let value = match source {
        SecretSource::Literal(v) => v.trim_end().to_owned(),
        SecretSource::Env(name) => std::env::var(name)
            .with_context(|| format!("reading env var {name}"))?
            .trim_end()
            .to_owned(),
        SecretSource::File(path) => {
            tokio::time::timeout(TOKEN_RESOLVE_TIMEOUT, tokio::fs::read_to_string(path))
                .await
                .with_context(|| {
                    format!(
                        "reading token file {path} timed out after {}s",
                        TOKEN_RESOLVE_TIMEOUT.as_secs()
                    )
                })?
                .with_context(|| format!("reading token file {path}"))?
                .trim_end()
                .to_owned()
        }
        SecretSource::Command(cmd) => {
            let output = tokio::time::timeout(
                TOKEN_RESOLVE_TIMEOUT,
                // kill_on_drop so a command that outlives the timeout (a hung
                // CLI, a vault stall) is reaped when the timed-out future drops,
                // rather than orphaned and left running on the daemon.
                tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .with_context(|| {
                format!(
                    "token command `{cmd}` timed out after {}s",
                    TOKEN_RESOLVE_TIMEOUT.as_secs()
                )
            })?
            .with_context(|| format!("running token command `{cmd}`"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "token command `{cmd}` failed ({}): {}",
                    output.status,
                    stderr.trim()
                );
            }
            String::from_utf8(output.stdout)
                .with_context(|| format!("token command `{cmd}` produced non-UTF-8 output"))?
                .trim_end()
                .to_owned()
        }
    };
    if value.is_empty() {
        bail!("resolved relay auth token is empty");
    }
    Ok(value)
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
            RelayEntry::url("https://b.example"),
            RelayEntry::url("https://a.example"),
            RelayEntry::url("https://b.example"),
        ]);
        assert_eq!(
            RelayChoice::resolve(Some(&policy)),
            Some(RelayChoice::Custom(vec![
                RelayEntry::url("https://a.example"),
                RelayEntry::url("https://b.example")
            ]))
        );
    }

    #[test]
    fn custom_choices_compare_regardless_of_input_order() {
        let a = RelayChoice::resolve(Some(&RelayPolicy::Custom(vec![
            RelayEntry::url("https://x"),
            RelayEntry::url("https://y"),
        ])));
        let b = RelayChoice::resolve(Some(&RelayPolicy::Custom(vec![
            RelayEntry::url("https://y"),
            RelayEntry::url("https://x"),
        ])));
        assert_eq!(a, b);
    }

    #[test]
    fn custom_preserves_tokens_and_keys_distinct_declarations() {
        // Same URL, different token declarations → distinct endpoint keys.
        let with_env = RelayChoice::custom(vec![RelayEntry {
            url: "https://r.example".into(),
            token: Some(SecretSource::Env("A".into())),
        }]);
        let with_lit = RelayChoice::custom(vec![RelayEntry {
            url: "https://r.example".into(),
            token: Some(SecretSource::Literal("x".into())),
        }]);
        let no_token = RelayChoice::custom(vec![RelayEntry::url("https://r.example")]);
        assert_ne!(with_env, with_lit);
        assert_ne!(with_env, no_token);
        // The token declaration survives normalization.
        assert_eq!(
            with_env,
            RelayChoice::Custom(vec![RelayEntry {
                url: "https://r.example".into(),
                token: Some(SecretSource::Env("A".into())),
            }])
        );
    }

    #[test]
    fn custom_dedup_keeps_first_token() {
        // On a duplicate URL the earlier entry's token wins.
        let choice = RelayChoice::custom(vec![
            RelayEntry {
                url: "https://r.example".into(),
                token: Some(SecretSource::Env("FIRST".into())),
            },
            RelayEntry {
                url: "https://r.example".into(),
                token: Some(SecretSource::Env("SECOND".into())),
            },
        ]);
        assert_eq!(
            choice,
            RelayChoice::Custom(vec![RelayEntry {
                url: "https://r.example".into(),
                token: Some(SecretSource::Env("FIRST".into())),
            }])
        );
    }

    #[test]
    fn custom_normalizes_trailing_slash_and_whitespace() {
        let a = RelayChoice::resolve(Some(&RelayPolicy::Custom(vec![RelayEntry::url(
            "https://relay.example/",
        )])));
        let b = RelayChoice::resolve(Some(&RelayPolicy::Custom(vec![RelayEntry::url(
            " https://relay.example ",
        )])));
        assert_eq!(a, b);
        assert_eq!(
            a,
            Some(RelayChoice::Custom(vec![RelayEntry::url(
                "https://relay.example"
            )]))
        );
    }

    #[test]
    fn config_policy_wins_over_env_var() {
        let env = Some("https://env-relay.example".to_owned());
        // A `Some(..)` policy never consults the env, so config always wins.
        assert_eq!(
            RelayChoice::resolve_with_env(Some(&RelayPolicy::Public), env.clone(), None),
            Some(RelayChoice::Public)
        );
        assert_eq!(
            RelayChoice::resolve_with_env(
                Some(&RelayPolicy::Custom(vec![RelayEntry::url(
                    "https://cfg.example"
                )])),
                env.clone(),
                None,
            ),
            Some(RelayChoice::Custom(vec![RelayEntry::url(
                "https://cfg.example"
            )]))
        );
        // With no config policy, the env override is consulted.
        assert_eq!(
            RelayChoice::resolve_with_env(None, env, None),
            Some(RelayChoice::Custom(vec![RelayEntry::url(
                "https://env-relay.example"
            )]))
        );
    }

    #[test]
    fn env_relay_carries_token_env() {
        // VELD_SHARE_RELAY_TOKEN pairs with VELD_SHARE_RELAY as a literal token.
        assert_eq!(
            RelayChoice::resolve_with_env(
                None,
                Some("https://env-relay.example".to_owned()),
                Some(" tok3n ".to_owned()),
            ),
            Some(RelayChoice::Custom(vec![RelayEntry {
                url: "https://env-relay.example".into(),
                token: Some(SecretSource::Literal("tok3n".into())),
            }]))
        );
        // A blank token env is ignored, not treated as an empty token.
        assert_eq!(
            RelayChoice::resolve_with_env(
                None,
                Some("https://env-relay.example".to_owned()),
                Some("   ".to_owned()),
            ),
            Some(RelayChoice::Custom(vec![RelayEntry::url(
                "https://env-relay.example"
            )]))
        );
    }

    fn relay_url(s: &str) -> RelayUrl {
        s.parse().expect("valid relay url")
    }

    #[test]
    fn for_join_no_relay_falls_back_to_public() {
        // A ticket advertising no relay (host public or direct-only) → public.
        let urls: [RelayUrl; 0] = [];
        assert_eq!(
            RelayChoice::for_join_with_env(urls.iter(), None, None),
            RelayChoice::Public
        );
    }

    #[test]
    fn for_join_mirrors_ticket_relay_confining_off_public() {
        // The core fix: a custom-relay ticket is joined over that same relay,
        // never over n0 public — even when the joiner set no env at all.
        let urls = [relay_url("https://relay.example")];
        assert_eq!(
            RelayChoice::for_join_with_env(urls.iter(), None, None),
            RelayChoice::Custom(vec![RelayEntry::url("https://relay.example")])
        );
    }

    #[test]
    fn for_join_attaches_token_only_for_the_configured_relay() {
        // Joiner explicitly configured THIS relay → the token is attached.
        let urls = [relay_url("https://relay.example")];
        assert_eq!(
            RelayChoice::for_join_with_env(
                urls.iter(),
                Some("https://relay.example".to_owned()),
                Some("tok3n".to_owned()),
            ),
            RelayChoice::Custom(vec![RelayEntry {
                url: "https://relay.example".to_owned(),
                token: Some(SecretSource::Literal("tok3n".to_owned())),
            }])
        );
    }

    #[test]
    fn for_join_never_leaks_token_to_a_relay_the_joiner_did_not_configure() {
        // Hostile ticket names a relay the joiner never configured: the token
        // stays home. Confinement to the ticket's relay still applies (so the
        // join simply fails against a token-gated attacker relay), but the
        // secret is never sent there.
        let urls = [relay_url("https://attacker.example")];
        let choice = RelayChoice::for_join_with_env(
            urls.iter(),
            Some("https://relay.example".to_owned()),
            Some("tok3n".to_owned()),
        );
        assert_eq!(
            choice,
            RelayChoice::Custom(vec![RelayEntry::url("https://attacker.example")])
        );
        // Belt and braces: the token string never appears anywhere in the choice.
        assert!(!format!("{choice:?}").contains("tok3n"));
    }

    #[test]
    fn for_join_mirrors_all_ticket_relays() {
        let urls = [
            relay_url("https://a.example"),
            relay_url("https://b.example"),
        ];
        let choice = RelayChoice::for_join_with_env(urls.iter(), None, None);
        let RelayChoice::Custom(entries) = choice else {
            panic!("expected custom");
        };
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn for_join_attaches_token_to_only_the_matching_relay_among_many() {
        // The security-critical per-entry case: a ticket lists several relays,
        // the joiner configured exactly one of them. The token must land on that
        // one entry and NONE of the others.
        let urls = [
            relay_url("https://other.example"),
            relay_url("https://mine.example"),
        ];
        let choice = RelayChoice::for_join_with_env(
            urls.iter(),
            Some("https://mine.example".to_owned()),
            Some("tok3n".to_owned()),
        );
        let RelayChoice::Custom(entries) = choice else {
            panic!("expected custom");
        };
        for e in &entries {
            match e.url.as_str() {
                "https://mine.example" => {
                    assert_eq!(e.token, Some(SecretSource::Literal("tok3n".to_owned())))
                }
                "https://other.example" => assert_eq!(e.token, None),
                other => panic!("unexpected relay {other}"),
            }
        }
    }

    #[test]
    fn for_join_blank_env_is_not_a_token() {
        // Whitespace-only token env is ignored, not sent as an empty token.
        let urls = [relay_url("https://relay.example")];
        assert_eq!(
            RelayChoice::for_join_with_env(
                urls.iter(),
                Some("https://relay.example".to_owned()),
                Some("   ".to_owned()),
            ),
            RelayChoice::Custom(vec![RelayEntry::url("https://relay.example")])
        );
    }

    #[test]
    fn resolve_requires_explicit_opt_in() {
        // No config policy and no env override → nothing is opted in; never
        // falls back to public relays implicitly.
        assert_eq!(RelayChoice::resolve_with_env(None, None, None), None);
        // Blank / whitespace-only env is ignored, not treated as an opt-in.
        assert_eq!(
            RelayChoice::resolve_with_env(None, Some("   ".into()), None),
            None
        );
    }

    #[test]
    fn display_renders_urls_only() {
        assert_eq!(RelayChoice::Public.to_string(), "public");
        // Even with tokens set, Display shows URLs only — never the token.
        let choice = RelayChoice::Custom(vec![
            RelayEntry::url("https://a"),
            RelayEntry {
                url: "https://b".into(),
                token: Some(SecretSource::Literal("secret".into())),
            },
        ]);
        let rendered = choice.to_string();
        assert_eq!(rendered, "[https://a, https://b]");
        assert!(!rendered.contains("secret"));
    }

    #[tokio::test]
    async fn resolve_secret_literal_and_command() {
        // A literal is returned trimmed (trailing newline/space dropped).
        assert_eq!(
            resolve_secret(&SecretSource::Literal("abc\n".into()))
                .await
                .unwrap(),
            "abc"
        );
        // Command stdout is captured with trailing whitespace trimmed.
        assert_eq!(
            resolve_secret(&SecretSource::Command("printf 'tok3n\\n'".into()))
                .await
                .unwrap(),
            "tok3n"
        );
    }

    #[tokio::test]
    async fn resolve_secret_file_trims_trailing_newline() {
        // A token file (as `op read > file` would write) is read and trimmed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.token");
        tokio::fs::write(&path, "file-tok3n\n").await.unwrap();
        assert_eq!(
            resolve_secret(&SecretSource::File(path.to_string_lossy().into_owned()))
                .await
                .unwrap(),
            "file-tok3n"
        );
    }

    #[tokio::test]
    async fn resolve_secret_missing_file_errors() {
        let err = resolve_secret(&SecretSource::File("/no/such/relay.token".into()))
            .await
            .unwrap_err();
        // Never falls back to an empty/absent token — it fails loudly.
        assert!(err.to_string().contains("token file"));
    }

    #[tokio::test]
    async fn bind_skips_token_resolution_for_invalid_url() {
        // A relay whose URL fails to parse is dropped before its token is
        // resolved — no token command runs for a dead URL (guards the ordering
        // in `bind_endpoint`). With no valid URL left, binding bails without
        // touching the network.
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("ran");
        let choice = RelayChoice::Custom(vec![RelayEntry {
            url: "not-a-valid-relay-url".into(),
            token: Some(SecretSource::Command(format!(
                "touch {}",
                sentinel.display()
            ))),
        }]);
        let err = bind_endpoint(SecretKey::generate(), &choice)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no valid relay URLs"));
        assert!(
            !sentinel.exists(),
            "token command ran for an unparseable relay URL"
        );
    }

    #[tokio::test]
    async fn resolve_secret_command_failure_errors() {
        assert!(
            resolve_secret(&SecretSource::Command("exit 7".into()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn resolve_secret_rejects_empty() {
        assert!(
            resolve_secret(&SecretSource::Literal(String::new()))
                .await
                .is_err()
        );
        // Command producing only whitespace trims to empty → error.
        assert!(
            resolve_secret(&SecretSource::Command("printf '\\n'".into()))
                .await
                .is_err()
        );
    }
}
