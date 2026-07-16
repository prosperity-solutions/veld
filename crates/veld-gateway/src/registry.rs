//! Live share registrations: join engine + slug routing table + lease
//! bookkeeping.
//!
//! The registry holds no persistent state (SHARING_V2.md §5.3): registrations
//! are leases the origin daemon refreshes by re-`POST`ing, slugs are
//! deterministic (`slug::derive`), and a gateway restart is recovered by the
//! next heartbeat. Cleanup is belt-and-braces: the tunnel's `conn.closed()`
//! drops a registration the moment the host goes away, and the lease sweeper
//! reaps anything whose origin stopped heartbeating without the tunnel
//! noticing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::endpoint::Connection;
use iroh::{Endpoint, RelayUrl, SecretKey};
use tokio::sync::Mutex;
use tracing::{info, warn};
use veld_core::config::RelayPolicy;
use veld_core::share::{Capability, ShareTicket};
use veld_share::endpoint::{
    RelayAuth, RelayChoice, bind_endpoint, relay_auth_status, resolve_secret,
};
use veld_share::join;

use crate::slug;

/// Max time to dial the host (hole-punch + the host's approval flow; mirrors
/// the daemon's join dial budget).
const DIAL_TIMEOUT: Duration = Duration::from_secs(75);
/// How long to watch a custom relay for an auth denial before dialing anyway.
const RELAY_AUTH_TIMEOUT: Duration = Duration::from_secs(8);
/// Cap on concurrently live registrations. Bounds the blast radius of a leaked
/// gateway token: each distinct capability holds an iroh connection + watcher
/// task, so an attacker spamming fresh capabilities can't grow state without
/// limit. Generous — one org's real environments stay far under it.
const MAX_REGISTRATIONS: usize = 512;

/// One publicly exposed hostname of a registered share.
#[derive(Debug, Clone)]
pub struct RegisteredNode {
    /// Node name from the manifest (e.g. `frontend`).
    pub node: String,
    /// Origin hostname — the tunnel routing key, the upstream `Host` header,
    /// and the match key for response-header rewrites.
    pub hostname: String,
    /// The service's origin — `scheme://authority` on the sharing machine
    /// (e.g. `https://app.demo.p.localhost` or `…:18443` in unprivileged
    /// mode). Used to rewrite the browser's `Origin`/`Referer` headers back to
    /// what the dev server expects, in lockstep with the `Host` rewrite.
    pub origin: String,
    /// Deterministic public slug.
    pub slug: String,
    /// Minted public URL: `https://<slug>.<domain>`.
    pub public_url: String,
}

/// A live registration: one joined share, its tunnel, and its public slugs.
pub struct Registration {
    /// Deterministic registration id (`hex(SHA-256-tagged(capability))[..32]`),
    /// so the origin can address DELETE/heartbeat without the gateway inventing
    /// server-side state.
    pub id: String,
    pub conn: Connection,
    pub run: String,
    pub project: String,
    pub nodes: Vec<RegisteredNode>,
    /// The relay policy this registration's endpoint is bound on (never evict
    /// an endpoint a live registration uses).
    relay: RelayChoice,
    /// Lease deadline; refreshed by heartbeat re-`POST`s.
    deadline: std::sync::Mutex<Instant>,
}

impl Registration {
    fn expired(&self, now: Instant) -> bool {
        *self.deadline.lock().expect("deadline lock") <= now
    }

    fn refresh(&self, lease: Duration) {
        *self.deadline.lock().expect("deadline lock") = Instant::now() + lease;
    }
}

/// Where a slug routes: which registration, and which node in it (by origin
/// hostname + origin scheme://authority for header rewrites).
#[derive(Clone)]
pub struct SlugTarget {
    pub registration: Arc<Registration>,
    pub hostname: String,
    pub origin: String,
}

/// The gateway's relay confinement, with any tokens **already resolved once**
/// at startup — so a `command`/`file` relay-token source can't be turned into
/// a per-registration process-spawn / file-read by a stolen-token registrant
/// spamming fresh capabilities, and a bad token config fails at boot.
#[derive(Clone)]
pub enum RelayAllowList {
    /// No `sharing.relays` configured: join whatever a ticket advertises.
    Unconfined,
    /// Explicit `public`: no confinement, no tokens.
    Public,
    /// Self-hosted allow-list: parsed URL + resolved token. A ticket relay
    /// must be one of these, or the registration is refused.
    Custom(Vec<(RelayUrl, Option<String>)>),
}

impl RelayAllowList {
    /// Resolve a config relay policy into an allow-list, running every token
    /// source exactly once. Invalid URLs are dropped with a warning (matching
    /// the daemon); a token that fails to resolve is a hard error.
    pub async fn resolve(policy: Option<&RelayPolicy>) -> Result<Self> {
        match policy {
            None => Ok(Self::Unconfined),
            Some(RelayPolicy::Public) => Ok(Self::Public),
            Some(RelayPolicy::Custom(entries)) => {
                let mut out = Vec::new();
                for e in entries {
                    let url = match e.url.parse::<RelayUrl>() {
                        Ok(u) => u,
                        Err(err) => {
                            warn!(error = %err, url = %e.url, "ignoring invalid relay URL in gateway config");
                            continue;
                        }
                    };
                    let token = match &e.token {
                        Some(src) => Some(resolve_secret(src).await.with_context(|| {
                            format!("resolving the gateway's relay auth token for {}", e.url)
                        })?),
                        None => None,
                    };
                    out.push((url, token));
                }
                Ok(Self::Custom(out))
            }
        }
    }
}

/// Owns the gateway's iroh endpoints and all live registrations.
pub struct Registry {
    domain: String,
    lease: Duration,
    relays: RelayAllowList,
    secret_key: SecretKey,
    endpoints: Mutex<HashMap<RelayChoice, Endpoint>>,
    regs: Mutex<HashMap<String, Arc<Registration>>>,
    slugs: Mutex<HashMap<String, SlugTarget>>,
}

/// Outcome of a successful register/heartbeat, serialized by the API layer.
pub struct RegistrationInfo {
    pub id: String,
    pub lease_secs: u64,
    pub nodes: Vec<RegisteredNode>,
}

impl Registry {
    pub fn new(
        domain: String,
        lease: Duration,
        relays: RelayAllowList,
        secret_key: SecretKey,
    ) -> Arc<Self> {
        Arc::new(Self {
            domain,
            lease,
            relays,
            secret_key,
            endpoints: Mutex::new(HashMap::new()),
            regs: Mutex::new(HashMap::new()),
            slugs: Mutex::new(HashMap::new()),
        })
    }

    /// Register a share (or refresh its lease — the same call is the
    /// heartbeat). Idempotent per capability: a live registration is refreshed
    /// and returned as-is; a dead one (tunnel closed) is torn down and
    /// re-joined, minting the *same* slugs.
    pub async fn register(self: &Arc<Self>, ticket: &ShareTicket) -> Result<RegistrationInfo> {
        let id = registration_id(&ticket.capability);

        // Heartbeat fast path: same share, live tunnel → refresh the lease.
        {
            let regs = self.regs.lock().await;
            if let Some(reg) = regs.get(&id) {
                if reg.conn.close_reason().is_none() {
                    reg.refresh(self.lease);
                    return Ok(self.info(reg));
                }
            }
        }
        // Stale entry (tunnel died between sweeps): drop it, then re-join.
        self.remove(&id, "re-registering").await;

        // Cap live registrations (a leaked token can't grow state without
        // bound). Checked after the heartbeat fast path, so an existing share
        // always refreshes; only genuinely-new registrations are turned away.
        if self.regs.lock().await.len() >= MAX_REGISTRATIONS {
            bail!("gateway registration limit reached ({MAX_REGISTRATIONS}); refusing new shares");
        }

        let addr = {
            use std::str::FromStr as _;
            iroh_tickets::endpoint::EndpointTicket::from_str(&ticket.iroh_ticket)
                .context("parsing iroh ticket")?
                .endpoint_addr()
                .clone()
        };

        // Relay confinement (allow-list): when the gateway is configured with
        // an explicit relay list, refuse tickets advertising anything else —
        // an org gateway must never dial out to arbitrary relays named by a
        // registration.
        let ticket_relays: Vec<RelayUrl> = addr.relay_urls().cloned().collect();
        let tokens = self.allowed_tokens(&ticket_relays)?;

        let choice = RelayChoice::for_join(ticket_relays.iter(), &tokens);
        let endpoint = self.get_or_bind(&choice).await?;

        // Distinguish "relay rejected our token" from "host unreachable" up
        // front, with the same eviction discipline as the daemon: a probe
        // endpoint bound only to discover the denial must not leak.
        if matches!(choice, RelayChoice::Custom(_)) {
            if let RelayAuth::Denied(relay_url) =
                relay_auth_status(&endpoint, RELAY_AUTH_TIMEOUT).await
            {
                drop(endpoint);
                self.evict_endpoint(&choice).await;
                bail!(
                    "relay {relay_url} denied the gateway's authentication — the relay token \
                     configured on the gateway is missing or wrong"
                );
            }
        }

        let label = format!("gateway {}", self.domain);
        let (conn, manifest) = tokio::time::timeout(
            DIAL_TIMEOUT,
            join::dial(&endpoint, addr, &ticket.capability, &label),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!("timed out connecting to the host (unreachable, or no relay path)")
        })??;

        if manifest.nodes.is_empty() {
            conn.close(0u32.into(), b"empty manifest");
            bail!("share manifest has no nodes to expose");
        }

        // The slug binds to the host's *authenticated* node id (the identity
        // proven by the QUIC handshake), not whatever the ticket claims.
        let host_node_id = conn.remote_id();
        let nodes: Vec<RegisteredNode> = manifest
            .nodes
            .iter()
            .map(|n| {
                let s = slug::derive(&host_node_id, &n.hostname, &ticket.capability);
                RegisteredNode {
                    node: n.node.clone(),
                    hostname: n.hostname.clone(),
                    origin: origin_of(&n.url, &n.hostname),
                    slug: s.clone(),
                    public_url: format!("https://{s}.{}", self.domain),
                }
            })
            .collect();

        let reg = Arc::new(Registration {
            id: id.clone(),
            conn: conn.clone(),
            run: manifest.run.clone(),
            project: manifest.project.clone(),
            nodes: nodes.clone(),
            relay: choice,
            deadline: std::sync::Mutex::new(Instant::now() + self.lease),
        });

        // Publish registration and slugs atomically enough: regs first, then
        // slugs, then the drop-watcher (which removes via `remove`, taking the
        // same locks in the same order).
        self.regs.lock().await.insert(id.clone(), Arc::clone(&reg));
        {
            let mut slugs = self.slugs.lock().await;
            for n in &nodes {
                slugs.insert(
                    n.slug.clone(),
                    SlugTarget {
                        registration: Arc::clone(&reg),
                        hostname: n.hostname.clone(),
                        origin: n.origin.clone(),
                    },
                );
            }
        }

        // Self-heal: the moment the host closes the tunnel (unshare, expiry,
        // daemon stop/crash) the registration and its public URLs vanish.
        let watcher = Arc::clone(self);
        let watch_id = id.clone();
        tokio::spawn(async move {
            conn.closed().await;
            watcher.remove(&watch_id, "tunnel closed").await;
        });

        info!(
            id = %reg.id,
            run = %reg.run,
            project = %reg.project,
            nodes = reg.nodes.len(),
            "share registered"
        );
        Ok(self.info(&reg))
    }

    /// Unregister by id. Idempotent: unknown ids are a no-op (the origin may
    /// retry a DELETE after the lease already expired).
    pub async fn unregister(&self, id: &str) {
        self.remove(id, "unregistered by origin").await;
    }

    /// Route a public slug to its registration + origin hostname.
    pub async fn lookup(&self, slug: &str) -> Option<SlugTarget> {
        self.slugs.lock().await.get(slug).cloned()
    }

    /// Run the lease sweeper until the process exits.
    pub async fn sweep_expired_leases(self: Arc<Self>) {
        // Sweep well inside the lease window so an expired registration
        // lingers at most a fraction of a lease past its deadline.
        let interval = (self.lease / 3).max(Duration::from_secs(1));
        loop {
            tokio::time::sleep(interval).await;
            let now = Instant::now();
            let expired: Vec<String> = {
                let regs = self.regs.lock().await;
                regs.values()
                    .filter(|r| r.expired(now))
                    .map(|r| r.id.clone())
                    .collect()
            };
            for id in expired {
                self.remove(&id, "lease expired (origin stopped heartbeating)")
                    .await;
            }
        }
    }

    fn info(&self, reg: &Registration) -> RegistrationInfo {
        RegistrationInfo {
            id: reg.id.clone(),
            lease_secs: self.lease.as_secs(),
            nodes: reg.nodes.clone(),
        }
    }

    /// Tear down one registration: unpublish its slugs, close its tunnel, and
    /// release its endpoint if nothing else uses that relay policy.
    async fn remove(&self, id: &str, reason: &str) {
        let Some(reg) = self.regs.lock().await.remove(id) else {
            return;
        };
        {
            let mut slugs = self.slugs.lock().await;
            for n in &reg.nodes {
                // Guard against a racing re-register that already replaced the
                // slug with a fresh target: only remove entries still pointing
                // at *this* registration instance.
                if slugs
                    .get(&n.slug)
                    .is_some_and(|t| Arc::ptr_eq(&t.registration, &reg))
                {
                    slugs.remove(&n.slug);
                }
            }
        }
        reg.conn.close(0u32.into(), b"registration removed");
        self.evict_endpoint(&reg.relay).await;
        info!(id = %reg.id, run = %reg.run, reason, "registration removed");
    }

    /// The relay auth tokens to attach for `ticket_relays`, enforcing the
    /// allow-list when one is configured. Tokens come only from the gateway's
    /// own (pre-resolved) config — never from the ticket — so a hostile
    /// registration can't make the gateway present a secret anywhere the
    /// operator didn't configure, and resolution never runs on this hot path.
    fn allowed_tokens(
        &self,
        ticket_relays: &[RelayUrl],
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let mut tokens = std::collections::BTreeMap::new();
        let allowed = match &self.relays {
            // No confinement (unconfigured, or explicit public): join whatever
            // the ticket advertises, token-less.
            RelayAllowList::Unconfined | RelayAllowList::Public => return Ok(tokens),
            RelayAllowList::Custom(allowed) => allowed,
        };

        // Confinement means the gateway reaches the host *over an allow-listed
        // relay*. A relay-less ticket (direct addresses only) would otherwise
        // bind a public endpoint and hole-punch out — bypassing the allow-list
        // entirely. Refuse it: a Custom allow-list gateway only ever joins over
        // its listed relays.
        if ticket_relays.is_empty() {
            bail!(
                "ticket advertises no relay, but this gateway is confined to an explicit relay \
                 allow-list — refusing to fall back to public/direct dialing"
            );
        }

        for ticket_url in ticket_relays {
            // Canonical `RelayUrl` equality: `https://r` and `https://r/` match.
            let Some((_, token)) = allowed.iter().find(|(u, _)| u == ticket_url) else {
                bail!(
                    "ticket advertises relay {ticket_url}, which is not in this gateway's \
                     relay allow-list — refusing to dial an unlisted relay"
                );
            };
            if let Some(value) = token {
                tokens.insert(ticket_url.to_string(), value.clone());
            }
        }
        Ok(tokens)
    }

    /// Get (or bind on demand) the endpoint for `requested` — same
    /// double-bind-free discipline as the daemon's endpoint map.
    async fn get_or_bind(&self, requested: &RelayChoice) -> Result<Endpoint> {
        let mut endpoints = self.endpoints.lock().await;
        if let Some(ep) = endpoints.get(requested) {
            return Ok(ep.clone());
        }
        let key = match requested {
            RelayChoice::Public => self.secret_key.clone(),
            RelayChoice::Custom(_) => SecretKey::generate(),
        };
        let ep = bind_endpoint(key, requested).await?;
        info!(node_id = %ep.id(), relays = %requested, "gateway iroh endpoint bound");
        endpoints.insert(requested.clone(), ep.clone());
        Ok(ep)
    }

    /// Close and drop an endpoint no live registration uses (probe endpoints
    /// bound only to discover a relay auth denial must not leak).
    async fn evict_endpoint(&self, choice: &RelayChoice) {
        if self.regs.lock().await.values().any(|r| &r.relay == choice) {
            return;
        }
        let ep = self.endpoints.lock().await.remove(choice);
        if let Some(ep) = ep {
            ep.close().await;
        }
    }
}

/// Deterministic registration id: a tagged one-way hash of the capability, so
/// the origin daemon can recompute it and the id leaks nothing.
pub fn registration_id(capability: &Capability) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"veld-gateway-reg/1");
    h.update(capability.as_bytes());
    let digest = h.finalize();
    hex(&digest[..16])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `scheme://authority` origin for a manifest node's URL, used to rewrite
/// the browser's `Origin`/`Referer` headers. Falls back to `https://<hostname>`
/// if the URL can't be parsed (manifest URLs are well-formed in practice).
fn origin_of(url: &str, hostname: &str) -> String {
    if let Some(rest) = url.split_once("://") {
        let (scheme, after) = rest;
        // authority ends at the first '/', '?' or '#'.
        let authority = after
            .split(['/', '?', '#'])
            .next()
            .filter(|a| !a.is_empty())
            .unwrap_or(hostname);
        return format!("{scheme}://{authority}");
    }
    format!("https://{hostname}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allowed_tokens_confines_to_the_allow_list() {
        use veld_core::config::{RelayEntry, SecretSource};

        // Resolve the allow-list the way startup does (token source → value).
        let policy = RelayPolicy::Custom(vec![RelayEntry {
            url: "https://relay.acme.internal".into(),
            token: Some(SecretSource::Literal("s3cret".into())),
        }]);
        let relays = RelayAllowList::resolve(Some(&policy)).await.unwrap();
        let reg = Registry::new(
            "share.example".into(),
            Duration::from_secs(90),
            relays,
            SecretKey::generate(),
        );

        let listed: RelayUrl = "https://relay.acme.internal".parse().unwrap();
        let attacker: RelayUrl = "https://attacker.example".parse().unwrap();

        // A listed relay → the token comes from gateway config (never a ticket).
        let tokens = reg.allowed_tokens(&[listed]).unwrap();
        assert_eq!(
            tokens.values().next().map(String::as_str),
            Some("s3cret"),
            "the allow-listed relay's configured token is attached"
        );

        // A ticket naming an unlisted relay is refused — the gateway never
        // dials out to a relay the operator didn't configure.
        let err = reg.allowed_tokens(&[attacker]).unwrap_err();
        assert!(err.to_string().contains("not in this gateway's"), "{err}");

        // A relay-less ticket under a Custom allow-list is refused too (no
        // silent fall back to public/direct dialing).
        let err = reg.allowed_tokens(&[]).unwrap_err();
        assert!(err.to_string().contains("no relay"), "{err}");
    }

    #[test]
    fn origin_of_extracts_scheme_authority() {
        assert_eq!(
            origin_of("https://app.demo.p.localhost", "app.demo.p.localhost"),
            "https://app.demo.p.localhost"
        );
        // Port preserved (unprivileged mode), path/query stripped.
        assert_eq!(
            origin_of(
                "https://app.demo.p.localhost:18443/path?q=1",
                "app.demo.p.localhost"
            ),
            "https://app.demo.p.localhost:18443"
        );
        // Unparseable URL falls back to https://<hostname>.
        assert_eq!(
            origin_of("garbage", "app.demo.p.localhost"),
            "https://app.demo.p.localhost"
        );
    }

    #[test]
    fn registration_id_is_deterministic_and_opaque() {
        let cap = Capability::generate();
        let a = registration_id(&cap);
        let b = registration_id(&cap);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        // One-way: the id must not contain capability bytes (spot check via
        // difference from a sibling capability's id).
        assert_ne!(a, registration_id(&Capability::generate()));
    }
}
