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
    /// The registration slot this occupies. Released when the last
    /// `Arc<Registration>` drops — normally at `remove`, though an in-flight
    /// proxy request holding a `SlugTarget` clone can pin it briefly longer
    /// (bounded by the request). Always the safe direction: the count never
    /// exceeds the cap, at most it under-admits for a moment.
    _permit: tokio::sync::OwnedSemaphorePermit,
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
    /// Hard bound on registrations, covering both settled and in-flight
    /// attempts: a permit is taken before dialing and held until the
    /// registration is removed. Closes the leaked-token exhaustion vector
    /// (unbounded concurrent 75s dials) that a `regs.len()` check alone missed.
    slots: Arc<tokio::sync::Semaphore>,
    /// Configured cap, kept for accurate error messages.
    max_registrations: usize,
    /// One iroh endpoint per relay policy, **reference-counted** by its actual
    /// users (in-flight dials + settled registrations). Endpoints are shared:
    /// every share over the same relay reuses one endpoint, and iroh
    /// `Endpoint::close()` aborts *all* its connections. So an endpoint is only
    /// torn down when its user count hits zero — never out from under a
    /// concurrent registration still mid-dial.
    endpoints: Mutex<HashMap<RelayChoice, (Endpoint, usize)>>,
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
        max_registrations: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            domain,
            lease,
            relays,
            secret_key,
            slots: Arc::new(tokio::sync::Semaphore::new(max_registrations)),
            max_registrations,
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
        // Stale entry (tunnel died between sweeps): drop it (releasing its
        // slot), then re-join.
        self.remove(&id, "re-registering").await;

        // Take a registration slot BEFORE the expensive dial and hold it for
        // the registration's lifetime. This bounds settled + in-flight
        // registrations by one hard count: a leaked token can't drive
        // unbounded concurrent 75s dials (task/fd/QUIC exhaustion), and the
        // settled count can't overshoot under concurrency. Released when the
        // permit drops — on any early return below, or when the registration
        // is removed. Checked after the heartbeat fast path, so a live share
        // always refreshes even at the cap.
        let permit = Arc::clone(&self.slots).try_acquire_owned().map_err(|_| {
            anyhow::anyhow!(
                "gateway registration limit reached ({} max); refusing new shares",
                self.max_registrations
            )
        })?;

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

        // We now hold one endpoint user ref (get_or_bind incremented it). Every
        // failure path below must release it so a token holder naming
        // unique-but-unreachable relays can't leak endpoints — but release only
        // *closes* the endpoint at zero users, so a concurrent registration
        // dialing over the same shared relay is never aborted.

        // Distinguish "relay rejected our token" from "host unreachable" up
        // front (a probe endpoint bound only to discover the denial must not
        // leak).
        if matches!(choice, RelayChoice::Custom(_)) {
            if let RelayAuth::Denied(relay_url) =
                relay_auth_status(&endpoint, RELAY_AUTH_TIMEOUT).await
            {
                self.release_endpoint(&choice).await;
                bail!(
                    "relay {relay_url} denied the gateway's authentication — the relay token \
                     configured on the gateway is missing or wrong"
                );
            }
        }

        let label = format!("gateway {}", self.domain);
        let dial = tokio::time::timeout(
            DIAL_TIMEOUT,
            join::dial(&endpoint, addr, &ticket.capability, &label),
        )
        .await;
        let (conn, manifest) = match dial {
            Ok(Ok(ok)) => ok,
            Ok(Err(e)) => {
                self.release_endpoint(&choice).await;
                return Err(e).context("dialing the share host");
            }
            Err(_) => {
                self.release_endpoint(&choice).await;
                bail!("timed out connecting to the host (unreachable, or no relay path)");
            }
        };

        if manifest.nodes.is_empty() {
            conn.close(0u32.into(), b"empty manifest");
            self.release_endpoint(&choice).await;
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
            _permit: permit,
        });

        // Publish registration and slugs atomically enough: regs first, then
        // slugs, then the drop-watcher (which removes via `remove`, taking the
        // same locks in the same order).
        //
        // If a concurrent register for the SAME id (same capability) raced us
        // and settled first, our insert replaces it. That prior registration
        // never goes through `remove()` (it's no longer in `regs` under its
        // id), so we must release its endpoint ref and close its now-orphaned
        // tunnel here — otherwise both leak. (Reachable only by a client
        // POSTing the same ticket concurrently; a well-behaved origin
        // heartbeats sequentially.)
        let replaced = self.regs.lock().await.insert(id.clone(), Arc::clone(&reg));
        if let Some(old) = replaced {
            old.conn
                .close(0u32.into(), b"superseded by re-registration");
            self.release_endpoint(&old.relay).await;
        }
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
        // Guarded to THIS instance: a superseding re-registration closes the
        // old connection (see the replaced-insert above), which wakes this
        // watcher — but by then `regs[id]` is the winner, so an id-based remove
        // would tear down the live registration. `remove_instance` only removes
        // if this exact `Arc` is still the registered one. `Weak` so the
        // watcher never pins the registration alive.
        let watcher = Arc::clone(self);
        let weak = Arc::downgrade(&reg);
        tokio::spawn(async move {
            conn.closed().await;
            if let Some(reg) = weak.upgrade() {
                watcher.remove_instance(&reg, "tunnel closed").await;
            }
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
    ///
    /// The **live iroh connection is authoritative** for whether a share is
    /// up; the `conn.closed()` watcher (in `register`) is what normally reaps a
    /// share the instant its tunnel dies. The lease is only a belt-and-braces
    /// backstop for a registration whose connection is *already dead* but whose
    /// watcher didn't run (e.g. lost during a crash). So the sweep reaps only
    /// entries that are **both** past their lease **and** have a closed
    /// connection — it must never tear down a healthy tunnel just because a
    /// heartbeat was missed. (The heartbeat rides HTTPS and the tunnel rides
    /// iroh/relay: independent failure domains. A transient HTTPS blip must not
    /// 404 a live share and force a 75s re-dial + host re-approval.)
    pub async fn sweep_expired_leases(self: Arc<Self>) {
        let interval = (self.lease / 3).max(Duration::from_secs(1));
        loop {
            tokio::time::sleep(interval).await;
            let now = Instant::now();
            let reapable: Vec<String> = {
                let regs = self.regs.lock().await;
                regs.values()
                    .filter(|r| r.expired(now) && r.conn.close_reason().is_some())
                    .map(|r| r.id.clone())
                    .collect()
            };
            for id in reapable {
                self.remove(&id, "lease expired and tunnel already closed")
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

    /// Tear down whatever registration is currently at `id` (used by the API
    /// DELETE, the lease sweep, and the stale-entry cleanup — all of which
    /// legitimately target "the current registration for this id").
    async fn remove(&self, id: &str, reason: &str) {
        let Some(reg) = self.regs.lock().await.remove(id) else {
            return;
        };
        self.teardown(reg, reason).await;
    }

    /// Tear down a registration only if `reg` is *still the instance* at its id.
    /// Used by the per-registration tunnel watcher: a superseding
    /// re-registration replaces `regs[id]` and closes the old connection, which
    /// wakes the old watcher — this guard stops it from tearing down the
    /// winner. Mirrors the per-slug `Arc::ptr_eq` guard in `teardown`.
    async fn remove_instance(&self, reg: &Arc<Registration>, reason: &str) {
        let removed = {
            let mut regs = self.regs.lock().await;
            if regs.get(&reg.id).is_some_and(|cur| Arc::ptr_eq(cur, reg)) {
                regs.remove(&reg.id)
            } else {
                None
            }
        };
        if let Some(reg) = removed {
            self.teardown(reg, reason).await;
        }
    }

    /// Shared teardown for an already-removed registration: unpublish its slugs,
    /// close its tunnel, and release its endpoint ref (closing the endpoint only
    /// if it was the last user).
    async fn teardown(&self, reg: Arc<Registration>, reason: &str) {
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
        self.release_endpoint(&reg.relay).await;
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

    /// Get (or bind on demand) the endpoint for `requested`, **incrementing its
    /// user count**. Every successful call must be paired with exactly one
    /// [`release_endpoint`](Self::release_endpoint) — on any failure that
    /// abandons the attempt, and (for a settled registration) in `remove`.
    async fn get_or_bind(&self, requested: &RelayChoice) -> Result<Endpoint> {
        let mut endpoints = self.endpoints.lock().await;
        if let Some((ep, users)) = endpoints.get_mut(requested) {
            *users += 1;
            return Ok(ep.clone());
        }
        let key = match requested {
            RelayChoice::Public => self.secret_key.clone(),
            RelayChoice::Custom(_) => SecretKey::generate(),
        };
        let ep = bind_endpoint(key, requested).await?;
        info!(node_id = %ep.id(), relays = %requested, "gateway iroh endpoint bound");
        endpoints.insert(requested.clone(), (ep.clone(), 1));
        Ok(ep)
    }

    /// Release one user of `choice`'s endpoint; when the last user drops, close
    /// and remove it. Because endpoints are shared and `close()` aborts *all*
    /// their connections, this must only fire at zero users — never while a
    /// concurrent registration is still dialing over the same relay.
    async fn release_endpoint(&self, choice: &RelayChoice) {
        let to_close = {
            let mut endpoints = self.endpoints.lock().await;
            match endpoints.get_mut(choice) {
                Some((_, users)) => {
                    *users = users.saturating_sub(1);
                    if *users == 0 {
                        endpoints.remove(choice).map(|(ep, _)| ep)
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        // Close outside the lock (close() awaits).
        if let Some(ep) = to_close {
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
            512,
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

    #[tokio::test]
    async fn registration_cap_refuses_new_shares_when_slots_exhausted() {
        // A registry with a single slot, already taken, refuses the next dial
        // BEFORE attempting it (so a leaked token can't drive parked dials).
        let reg = Registry::new(
            "share.example".into(),
            Duration::from_secs(90),
            RelayAllowList::Unconfined,
            SecretKey::generate(),
            1,
        );
        // Hold the only slot, as a live registration would.
        let held = Arc::clone(&reg.slots).try_acquire_owned().unwrap();

        // A minimal well-formed ticket — register must reject on the slot
        // check before it ever parses/dials.
        let ticket = ShareTicket {
            iroh_ticket: "does-not-matter".into(),
            capability: Capability::generate(),
            relay_tokens: Default::default(),
        };
        let Err(err) = reg.register(&ticket).await else {
            panic!("expected the registration to be refused at the cap");
        };
        assert!(
            err.to_string().contains("registration limit reached"),
            "{err}"
        );

        drop(held);
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
