//! Gateway configuration: **env-var-first** with an optional JSON file.
//!
//! A containerized deployment needs zero files — every setting has a
//! `VELD_GATEWAY_*` env var. A config file (`VELD_GATEWAY_CONFIG=/path`, or
//! `--config /path`) covers operators who prefer mounted config; env vars win
//! over file values so a container can always override a baked-in file.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use veld_core::config::{RelayEntry, RelayPolicy, SecretSource};

/// How long a registration lives without a heartbeat before the gateway drops
/// it. Origins re-`POST` (heartbeat) well inside this window.
const DEFAULT_LEASE_SECS: u64 = 90;

const DEFAULT_LISTEN: &str = "0.0.0.0:8080";

/// Default cap on concurrently live + in-flight registrations. Bounds a leaked
/// token's blast radius; generous enough that one org's real environments stay
/// well under it. Raise via `VELD_GATEWAY_MAX_REGISTRATIONS` for a large fleet.
const DEFAULT_MAX_REGISTRATIONS: usize = 512;

/// Upper bound on the configured cap — far above any real fleet, but low
/// enough to keep `tokio::sync::Semaphore::new` well inside its valid range
/// (it panics near `usize::MAX`).
const MAX_REGISTRATIONS_CEILING: usize = 1_000_000;

/// Fully resolved gateway configuration.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Public base domain. Shares surface as `https://<slug>.<domain>`; the
    /// registration API answers on the apex (`https://<domain>/api/v1/…`).
    pub domain: String,
    /// Socket the gateway binds.
    pub listen: SocketAddr,
    /// Wildcard TLS cert/key paths. `None` = plain HTTP behind an external TLS
    /// terminator (platform load balancer / ingress).
    pub tls: Option<TlsPaths>,
    /// The registration auth token origins must present (`Authorization:
    /// Bearer`). Required — the gateway refuses to start without one, so the
    /// registration API is never open.
    pub auth_token: SecretSource,
    /// Relay policy. `None` or `Public`: join whatever relays a ticket
    /// advertises (no confinement). `Custom(list)`: **allow-list** — tickets
    /// advertising a relay outside the list are rejected, and each entry may
    /// carry the auth token for that relay.
    pub relays: Option<RelayPolicy>,
    /// Registration lease duration (heartbeat window).
    pub lease: Duration,
    /// Directory for the persistent node key (stable iroh identity across
    /// restarts when a volume is mounted). `None` = platform data dir.
    pub state_dir: Option<PathBuf>,
    /// Hard cap on concurrently live + in-flight registrations.
    pub max_registrations: usize,
    /// Trust the immediate upstream (a sanitising TLS-terminating LB) for
    /// `X-Forwarded-For`: the last entry is taken as the real client IP
    /// (rate-limit keying) and the inbound chain is forwarded upstream.
    /// Default **off** — a directly-exposed gateway must overwrite the chain,
    /// or any viewer could spoof it. Enable ONLY behind an LB that appends
    /// the true peer address. Deliberately does NOT extend to
    /// `X-Forwarded-Host` — that is [`Self::trust_forwarded_host`]'s own
    /// opt-in, so enabling IP trust never silently starts routing (and
    /// forwarding to origin apps) by a viewer-suppliable host header.
    pub trust_forwarded_headers: bool,
    /// Trust the edge's `X-Forwarded-Host` (first entry) as the host the
    /// viewer addressed: it overrides `Host` for slug routing, the upstream
    /// `X-Forwarded-Host`, and `Referer` rewriting. Required behind a CDN
    /// (CloudFront) that rewrites `Host` to its origin's hostname. Default
    /// **off**. Enable ONLY behind an edge that **overwrites or strips** any
    /// inbound `X-Forwarded-Host` — an edge that merely passes it through
    /// lets a viewer inject the value that origin apps then see.
    pub trust_forwarded_host: bool,
}

/// Paths to a TLS certificate chain and private key (PEM).
#[derive(Debug, Clone)]
pub struct TlsPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// The optional JSON config file. Every field is optional here; required
/// fields are enforced after the env merge.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    domain: Option<String>,
    listen: Option<String>,
    tls: Option<FileTls>,
    auth: Option<FileAuth>,
    relays: Option<RelayPolicy>,
    #[serde(rename = "lease_secs")]
    lease_secs: Option<u64>,
    #[serde(rename = "state_dir")]
    state_dir: Option<String>,
    #[serde(rename = "max_registrations")]
    max_registrations: Option<usize>,
    #[serde(rename = "trust_forwarded_headers")]
    trust_forwarded_headers: Option<bool>,
    #[serde(rename = "trust_forwarded_host")]
    trust_forwarded_host: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTls {
    cert: String,
    key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAuth {
    token: SecretSource,
}

impl GatewayConfig {
    /// Load configuration from the process environment, merged over the
    /// optional config file (env wins). `config_path` comes from `--config` or
    /// `VELD_GATEWAY_CONFIG`.
    pub fn load(config_path: Option<&str>) -> Result<Self> {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let path = config_path
            .map(str::to_owned)
            .or_else(|| env.get("VELD_GATEWAY_CONFIG").cloned());
        let file = match &path {
            Some(p) => {
                let raw = std::fs::read_to_string(p)
                    .with_context(|| format!("reading config file {p}"))?;
                serde_json::from_str::<FileConfig>(&raw)
                    .with_context(|| format!("parsing config file {p}"))?
            }
            None => FileConfig::default(),
        };
        Self::from_parts(file, &env)
    }

    /// Core of `load`, with the environment injected so it is unit-testable
    /// without mutating the process environment.
    fn from_parts(file: FileConfig, env: &BTreeMap<String, String>) -> Result<Self> {
        let get = |key: &str| env.get(key).map(|v| v.trim()).filter(|v| !v.is_empty());

        let domain = get("VELD_GATEWAY_DOMAIN")
            .map(str::to_owned)
            .or(file.domain)
            .context(
                "no public domain configured; set VELD_GATEWAY_DOMAIN (or `domain` in the \
                 config file) to the base domain shares are served under, e.g. share.acme.internal",
            )?;
        let domain = normalize_domain(&domain)?;

        let listen_raw = get("VELD_GATEWAY_LISTEN")
            .map(str::to_owned)
            .or(file.listen)
            .unwrap_or_else(|| DEFAULT_LISTEN.to_owned());
        let listen: SocketAddr = listen_raw
            .parse()
            .with_context(|| format!("invalid listen address `{listen_raw}`"))?;

        let tls = match (get("VELD_GATEWAY_TLS_CERT"), get("VELD_GATEWAY_TLS_KEY")) {
            (Some(cert), Some(key)) => Some(TlsPaths {
                cert: cert.into(),
                key: key.into(),
            }),
            (None, None) => file.tls.map(|t| TlsPaths {
                cert: t.cert.into(),
                key: t.key.into(),
            }),
            _ => bail!(
                "VELD_GATEWAY_TLS_CERT and VELD_GATEWAY_TLS_KEY must be set together \
                 (or neither, to run plain HTTP behind an external TLS terminator)"
            ),
        };

        // Token source precedence: explicit env literal > env file path >
        // config file declaration. The file form (`VELD_GATEWAY_TOKEN_FILE`)
        // is preferred for container secret mounts — the secret never enters
        // the environment.
        let auth_token = if let Some(literal) = get("VELD_GATEWAY_TOKEN") {
            SecretSource::Literal(literal.to_owned())
        } else if let Some(path) = get("VELD_GATEWAY_TOKEN_FILE") {
            SecretSource::File(path.to_owned())
        } else if let Some(auth) = file.auth {
            auth.token
        } else {
            bail!(
                "no registration auth token configured; set VELD_GATEWAY_TOKEN (or \
                 VELD_GATEWAY_TOKEN_FILE, or `auth.token` in the config file). The \
                 registration API is never served without one."
            );
        };

        let relays = match get("VELD_GATEWAY_RELAYS") {
            Some(raw) => Some(parse_relays_env(raw, get("VELD_GATEWAY_RELAY_TOKEN"))?),
            None => file.relays,
        };

        let lease_secs = match get("VELD_GATEWAY_LEASE_SECS") {
            Some(raw) => raw
                .parse::<u64>()
                .with_context(|| format!("invalid VELD_GATEWAY_LEASE_SECS `{raw}`"))?,
            None => file.lease_secs.unwrap_or(DEFAULT_LEASE_SECS),
        };
        if lease_secs == 0 {
            bail!("lease_secs must be positive");
        }

        let state_dir = get("VELD_GATEWAY_STATE_DIR")
            .map(str::to_owned)
            .or(file.state_dir)
            .map(PathBuf::from);

        let max_registrations = match get("VELD_GATEWAY_MAX_REGISTRATIONS") {
            Some(raw) => raw
                .parse::<usize>()
                .with_context(|| format!("invalid VELD_GATEWAY_MAX_REGISTRATIONS `{raw}`"))?,
            None => file.max_registrations.unwrap_or(DEFAULT_MAX_REGISTRATIONS),
        };
        // Bound both ends: 0 would refuse every share; an absurd value would
        // overflow `Semaphore::new`'s internal permit representation and panic.
        if !(1..=MAX_REGISTRATIONS_CEILING).contains(&max_registrations) {
            bail!("max_registrations must be between 1 and {MAX_REGISTRATIONS_CEILING}");
        }

        let parse_bool = |key: &str, raw: &str| -> Result<bool> {
            match raw.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" => Ok(true),
                "0" | "false" | "no" => Ok(false),
                other => bail!("invalid {key} `{other}` (use true/false)"),
            }
        };
        let trust_forwarded_headers = match get("VELD_GATEWAY_TRUST_FORWARDED") {
            Some(raw) => parse_bool("VELD_GATEWAY_TRUST_FORWARDED", raw)?,
            None => file.trust_forwarded_headers.unwrap_or(false),
        };
        let trust_forwarded_host = match get("VELD_GATEWAY_TRUST_FORWARDED_HOST") {
            Some(raw) => parse_bool("VELD_GATEWAY_TRUST_FORWARDED_HOST", raw)?,
            None => file.trust_forwarded_host.unwrap_or(false),
        };

        Ok(Self {
            domain,
            listen,
            tls,
            auth_token,
            relays,
            lease: Duration::from_secs(lease_secs),
            state_dir,
            max_registrations,
            trust_forwarded_headers,
            trust_forwarded_host,
        })
    }
}

/// Parse `VELD_GATEWAY_RELAYS`: the literal `public`, or a comma-separated
/// list of relay URLs. `VELD_GATEWAY_RELAY_TOKEN`, when set, attaches to every
/// listed relay (the single-relay case is the typical one; a multi-relay
/// deployment with distinct tokens uses the config file's per-entry form).
fn parse_relays_env(raw: &str, token: Option<&str>) -> Result<RelayPolicy> {
    if raw.eq_ignore_ascii_case("public") {
        return Ok(RelayPolicy::Public);
    }
    let entries: Vec<RelayEntry> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|url| RelayEntry {
            url: url.to_owned(),
            token: token.map(|t| SecretSource::Literal(t.to_owned())),
        })
        .collect();
    if entries.is_empty() {
        bail!("VELD_GATEWAY_RELAYS is set but contains no relay URLs");
    }
    Ok(RelayPolicy::Custom(entries))
}

/// Validate and normalize the public base domain: lowercase, no scheme, no
/// port, no wildcard label (the wildcard lives in DNS/TLS, not here).
fn normalize_domain(raw: &str) -> Result<String> {
    let d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() {
        bail!("domain is empty");
    }
    if d.contains("://") || d.contains('/') {
        bail!("domain must be a bare DNS name, not a URL: `{raw}`");
    }
    if d.contains(':') {
        bail!("domain must not carry a port: `{raw}`");
    }
    if d.contains('*') {
        bail!("domain must not contain a wildcard label — use the base domain: `{raw}`");
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn env_only_config_is_sufficient() {
        let cfg = GatewayConfig::from_parts(
            FileConfig::default(),
            &env(&[
                ("VELD_GATEWAY_DOMAIN", "share.acme.internal"),
                ("VELD_GATEWAY_TOKEN", "s3cret"),
            ]),
        )
        .unwrap();
        assert_eq!(cfg.domain, "share.acme.internal");
        assert_eq!(cfg.listen.to_string(), "0.0.0.0:8080");
        assert!(cfg.tls.is_none());
        assert_eq!(cfg.auth_token, SecretSource::Literal("s3cret".into()));
        assert_eq!(cfg.lease, Duration::from_secs(DEFAULT_LEASE_SECS));
    }

    #[test]
    fn missing_domain_or_token_refuses_to_start() {
        let err =
            GatewayConfig::from_parts(FileConfig::default(), &env(&[("VELD_GATEWAY_TOKEN", "t")]))
                .unwrap_err();
        assert!(err.to_string().contains("VELD_GATEWAY_DOMAIN"), "{err}");

        let err = GatewayConfig::from_parts(
            FileConfig::default(),
            &env(&[("VELD_GATEWAY_DOMAIN", "share.acme.internal")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("auth token"), "{err}");
    }

    #[test]
    fn env_wins_over_file() {
        let file: FileConfig = serde_json::from_str(
            r#"{
                "domain": "file.example",
                "listen": "127.0.0.1:9999",
                "auth": { "token": { "env": "FILE_TOKEN" } },
                "lease_secs": 30
            }"#,
        )
        .unwrap();
        let cfg = GatewayConfig::from_parts(
            file,
            &env(&[
                ("VELD_GATEWAY_DOMAIN", "env.example"),
                ("VELD_GATEWAY_TOKEN", "envtok"),
            ]),
        )
        .unwrap();
        assert_eq!(cfg.domain, "env.example");
        // Unset env fields fall through to the file.
        assert_eq!(cfg.listen.to_string(), "127.0.0.1:9999");
        assert_eq!(cfg.lease, Duration::from_secs(30));
        assert_eq!(cfg.auth_token, SecretSource::Literal("envtok".into()));
    }

    #[test]
    fn max_registrations_default_env_and_zero_guard() {
        let base = [
            ("VELD_GATEWAY_DOMAIN", "share.acme.internal"),
            ("VELD_GATEWAY_TOKEN", "t"),
        ];
        // Default when unset.
        let cfg = GatewayConfig::from_parts(FileConfig::default(), &env(&base)).unwrap();
        assert_eq!(cfg.max_registrations, DEFAULT_MAX_REGISTRATIONS);

        // Env override parses.
        let mut with_cap = base.to_vec();
        with_cap.push(("VELD_GATEWAY_MAX_REGISTRATIONS", "2000"));
        let cfg = GatewayConfig::from_parts(FileConfig::default(), &env(&with_cap)).unwrap();
        assert_eq!(cfg.max_registrations, 2000);

        // Zero is rejected (a zero-permit semaphore would refuse every share).
        let mut zero = base.to_vec();
        zero.push(("VELD_GATEWAY_MAX_REGISTRATIONS", "0"));
        assert!(GatewayConfig::from_parts(FileConfig::default(), &env(&zero)).is_err());

        // An absurd value is rejected (would panic Semaphore::new).
        let mut huge = base.to_vec();
        huge.push(("VELD_GATEWAY_MAX_REGISTRATIONS", "99999999999"));
        assert!(GatewayConfig::from_parts(FileConfig::default(), &env(&huge)).is_err());
    }

    #[test]
    fn forwarded_trust_flags_are_independent() {
        let base = [
            ("VELD_GATEWAY_DOMAIN", "share.acme.internal"),
            ("VELD_GATEWAY_TOKEN", "t"),
        ];
        // Both default off.
        let cfg = GatewayConfig::from_parts(FileConfig::default(), &env(&base)).unwrap();
        assert!(!cfg.trust_forwarded_headers);
        assert!(!cfg.trust_forwarded_host);

        // Enabling X-Forwarded-For trust must NOT enable X-Forwarded-Host
        // routing — that silent expansion is exactly what the separate flag
        // exists to prevent.
        let mut xff = base.to_vec();
        xff.push(("VELD_GATEWAY_TRUST_FORWARDED", "true"));
        let cfg = GatewayConfig::from_parts(FileConfig::default(), &env(&xff)).unwrap();
        assert!(cfg.trust_forwarded_headers);
        assert!(!cfg.trust_forwarded_host);

        // And vice versa; also covers the env parse of the new flag.
        let mut xfh = base.to_vec();
        xfh.push(("VELD_GATEWAY_TRUST_FORWARDED_HOST", "yes"));
        let cfg = GatewayConfig::from_parts(FileConfig::default(), &env(&xfh)).unwrap();
        assert!(!cfg.trust_forwarded_headers);
        assert!(cfg.trust_forwarded_host);

        // Bad value is a boot error, not a silent default.
        let mut bad = base.to_vec();
        bad.push(("VELD_GATEWAY_TRUST_FORWARDED_HOST", "maybe"));
        assert!(GatewayConfig::from_parts(FileConfig::default(), &env(&bad)).is_err());
    }

    #[test]
    fn token_file_env_yields_file_source() {
        let cfg = GatewayConfig::from_parts(
            FileConfig::default(),
            &env(&[
                ("VELD_GATEWAY_DOMAIN", "share.acme.internal"),
                ("VELD_GATEWAY_TOKEN_FILE", "/run/secrets/gw-token"),
            ]),
        )
        .unwrap();
        assert_eq!(
            cfg.auth_token,
            SecretSource::File("/run/secrets/gw-token".into())
        );
    }

    #[test]
    fn relays_env_public_and_list() {
        assert_eq!(
            parse_relays_env("public", None).unwrap(),
            RelayPolicy::Public
        );
        let p = parse_relays_env("https://r1.example, https://r2.example", Some("tok")).unwrap();
        let RelayPolicy::Custom(entries) = p else {
            panic!("expected custom");
        };
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .all(|e| e.token == Some(SecretSource::Literal("tok".into())))
        );
    }

    #[test]
    fn tls_env_requires_both_halves() {
        let err = GatewayConfig::from_parts(
            FileConfig::default(),
            &env(&[
                ("VELD_GATEWAY_DOMAIN", "share.acme.internal"),
                ("VELD_GATEWAY_TOKEN", "t"),
                ("VELD_GATEWAY_TLS_CERT", "/certs/wild.pem"),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be set together"), "{err}");
    }

    #[test]
    fn domain_normalization_rejects_urls_ports_wildcards() {
        assert_eq!(
            normalize_domain("Share.Acme.Internal.").unwrap(),
            "share.acme.internal"
        );
        assert!(normalize_domain("https://share.acme.internal").is_err());
        assert!(normalize_domain("share.acme.internal:8443").is_err());
        assert!(normalize_domain("*.share.acme.internal").is_err());
        assert!(normalize_domain("  ").is_err());
    }
}
