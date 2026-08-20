use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Caddy admin API base URL.
const CADDY_ADMIN_API: &str = "http://localhost:2019";

// Reserved hostname for the browser management UI — **veld-core's constant, not a
// second literal.** The daemon's `Origin` allowlist derives the origins it accepts
// a WebSocket upgrade from out of the same one (`veld-daemon/src/pty.rs`), so a
// hostname served here that the daemon does not know is a dashboard whose every
// terminal and IDE channel is refused — exactly the fault this coupling closes.
use veld_core::instance::MANAGEMENT_HOST;

/// Port the daemon's HTTP server listens on (feedback + management).
const DAEMON_HTTP_PORT: u16 = 19899;

/// Overall timeout for a single Caddy admin API request. A half-dead Caddy
/// (e.g. after a macOS sleep/wake that reset the network stack) can accept a
/// TCP connection but never respond; without this bound the helper's request
/// handler — and any daemon/CLI call waiting on it — would hang forever.
const CADDY_ADMIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Connect timeout for the Caddy admin API (localhost, so fast).
const CADDY_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a leaf certificate Caddy issues is good for.
///
/// Caddy's own default is 12 hours, which veld took silently — and a 12-hour
/// leaf makes *one* missed renewal a broken browser by the next morning. It is
/// the whole blast radius of a certificate-maintenance loop that stops: whatever
/// stops it, veld has until the current leaf expires to notice and act. A week
/// buys that time; nothing about a local development CA needs the certificate to
/// be short-lived.
///
/// **Do not shorten this to a handful of minutes.** Measured against Caddy
/// 2.11.4: a `1m` lifetime makes issuance fail outright — smallstep answers
/// `createCertificateRequest 'lifetime' cannot be 0` and Caddy serves no
/// certificate at all, which is a harder failure than the one this constant
/// exists to soften. `168h` was verified end to end: a real Caddy issued 7-day
/// leaves for both the management host and a run hostname.
const LEAF_LIFETIME: &str = "168h";

/// How long the local CA's *intermediate* is good for.
///
/// Raised together with [`LEAF_LIFETIME`] and never below it: Caddy clamps an
/// issued leaf to its issuer's `NotAfter` (with a warning, not an error), so a
/// 7-day leaf under Caddy's default 7-day intermediate would silently shrink to
/// whatever was left of the intermediate. Must stay under the root's remaining
/// lifetime, which Caddy generates for 10 years.
const INTERMEDIATE_LIFETIME: &str = "720h";

/// Consecutive overdue certificate probes before Caddy is restarted. The remedy
/// drops live connections, so one probe is not enough to earn it.
const CERT_STRIKES_BEFORE_RESTART: u32 = 2;

/// Minimum gap between two certificate-driven restarts. A restarted Caddy
/// renews in the background, so it legitimately serves the old certificate for a
/// moment; without this, "still broken" would mean "restart again", forever.
const CERT_RESTART_COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// How many restarts veld will spend before concluding that restarting is not
/// the answer. The cooldown alone bounds the *rate*, not the total: a fault a new
/// process cannot fix would otherwise have root killing Caddy 144 times a day,
/// every one of them dropping every live connection on the machine.
const CERT_RESTART_ATTEMPTS: u32 = 3;

/// Cap on Caddy's own log file before it rolls, and how many rolls to keep.
///
/// Caddy is spawned with its stdout and stderr discarded, which is why the one
/// log line that would have named this whole class of fault —
/// certmagic's `renewing managed certificates` error — went nowhere. Caddy's
/// `file` writer keeps it instead, bounded by construction: no access logs are
/// configured for the server, so this file carries only lifecycle and error
/// lines and rolls long before it can matter.
const LOG_ROLL_SIZE_MB: u32 = 4;
const LOG_ROLL_KEEP: u32 = 2;

/// Manages the Caddy process and its routes.
#[derive(Debug)]
pub struct CaddyManager {
    inner: Arc<Mutex<CaddyState>>,
    /// Active per-run routes, keyed by route `@id`, persisted to disk so they
    /// survive a Caddy restart, a helper restart (update/crash), or a reboot.
    /// Caddy itself keeps routes only in memory, so the helper is the durable
    /// source of truth and replays them on every (re)load.
    routes: Arc<Mutex<RouteStore>>,
    /// Serialises `reload`, so no `/load` can apply a snapshot older than one
    /// already applied. See [`CaddyManager::reload`]; it exists separately from
    /// `routes` so a reload in flight does not block a reader of the store.
    reload_lock: Arc<Mutex<()>>,
    client: reqwest::Client,
    https_port: u16,
    http_port: u16,
    /// Override for the Caddy binary path (avoids lib_dir() issues under sudo).
    caddy_bin_override: Option<std::path::PathBuf>,
}

#[derive(Debug)]
struct CaddyState {
    /// PID of the managed Caddy process, if running.
    child_pid: Option<u32>,
    /// Whether an overdue certificate has earned a restart yet.
    cert_gate: CertGate,
}

/// The policy half of the certificate watchdog: how many bad probes, and how
/// often, justify restarting Caddy.
///
/// Separate from the restart itself so the arithmetic is testable — the remedy
/// drops every live connection, and "one probe too eager" and "restarts forever"
/// are both faults that would only ever show up on a user's machine.
#[derive(Debug, Default)]
struct CertGate {
    /// Consecutive probes that found renewal overdue.
    strikes: u32,
    /// When the last certificate-driven restart happened.
    last_restart: Option<std::time::Instant>,
    /// Restarts since the last time the certificate came back healthy. The
    /// remedy is only a remedy while it works; see [`CERT_RESTART_ATTEMPTS`].
    restarts_without_recovery: u32,
}

/// What [`CertGate::weigh`] concluded about one probe.
#[derive(Debug, PartialEq, Eq)]
enum CertVerdict {
    /// Nothing to do — healthy, or a probe that learned nothing.
    Fine,
    /// Overdue, but not yet enough to restart Caddy for.
    Overdue { strikes: u32 },
    /// Overdue, and restarting Caddy is now the right move.
    Restart,
    /// The certificate is healthy again after having been overdue.
    Recovered,
    /// Overdue, restarts have not helped, and veld has stopped restarting.
    GaveUp,
}

impl CertGate {
    /// `all_healthy` is the *set's* verdict, not something derivable from
    /// `health`: `health` is the worst of many hostnames, and the worst verdict is
    /// `Unreachable` for as long as any single hostname cannot be issued at all.
    /// Reading recovery out of it would leave a helper that had once given up
    /// stuck there for the rest of its uptime — see `tls_health::all_healthy`.
    fn weigh(
        &mut self,
        health: &veld_core::tls_health::TlsHealth,
        all_healthy: bool,
        now: std::time::Instant,
    ) -> CertVerdict {
        // Only a certificate a browser would actually accept re-arms anything.
        // `!renewal_is_overdue()` is *not* that test: it is also false for a
        // probe that reached nothing (`Unreachable`), one that could not read
        // what it got (`Unreadable`), and a certificate the clock rejects
        // (`NotYetValid`). Resetting on those would (a) log "healthy again" about
        // a certificate that was never read, in the one log this whole change
        // exists to make trustworthy, and (b) hand back the give-up cap below on
        // every restart, because the probe taken while the new Caddy is still
        // starting reads `Unreachable` — Expired, Expired, restart, Unreachable,
        // repeat, one root-driven restart every cooldown for as long as the
        // machine is up. Which is exactly what the cap exists to stop.
        if all_healthy {
            let recovered = self.strikes > 0 || self.restarts_without_recovery > 0;
            self.strikes = 0;
            self.restarts_without_recovery = 0;
            return if recovered {
                CertVerdict::Recovered
            } else {
                CertVerdict::Fine
            };
        }

        // A probe that learned nothing changes nothing: it neither counts
        // towards a restart nor clears what earlier probes established. The
        // certificate did not change while veld failed to look at it.
        if !health.renewal_is_overdue() {
            return CertVerdict::Fine;
        }

        // Restarting is only worth its cost — every live TLS connection on the
        // machine, dropped — while it is actually fixing something. Some
        // certificate faults a new process cannot touch: an unwritable storage
        // tree, a CA whose key is gone, an intermediate clamped to nothing. Past
        // this many restarts with no healthy probe in between, veld says so and
        // leaves Caddy alone; the state is still red in `veld doctor` and the
        // reason is in Caddy's own log.
        if self.restarts_without_recovery >= CERT_RESTART_ATTEMPTS {
            return CertVerdict::GaveUp;
        }

        self.strikes = self.strikes.saturating_add(1);
        if self.strikes < CERT_STRIKES_BEFORE_RESTART {
            return CertVerdict::Overdue {
                strikes: self.strikes,
            };
        }
        // A Caddy that was just restarted renews in the background, so it
        // legitimately still serves the old certificate for a moment. Without
        // this, "still broken" would mean "restart again", every minute, forever.
        if let Some(last) = self.last_restart {
            if now.duration_since(last) < CERT_RESTART_COOLDOWN {
                return CertVerdict::Overdue {
                    strikes: self.strikes,
                };
            }
        }
        // Cleared before the restart, not after: the probes taken while the new
        // Caddy is still renewing must not count towards the next one.
        self.strikes = 0;
        self.last_restart = Some(now);
        self.restarts_without_recovery = self.restarts_without_recovery.saturating_add(1);
        CertVerdict::Restart
    }
}

/// Durable store of the Caddy routes this helper has been asked to serve.
#[derive(Debug)]
struct RouteStore {
    /// route `@id` -> the fully-built Caddy route JSON (as `build_route_json`
    /// produces it), ready to splice back into a config on reload.
    routes: HashMap<String, serde_json::Value>,
    /// Where the store is persisted on disk.
    path: PathBuf,
}

impl CaddyManager {
    pub fn new(https_port: u16, http_port: u16, caddy_bin: Option<std::path::PathBuf>) -> Self {
        let store_path = routes_store_path(&caddy_bin);
        let mut routes = load_route_store(&store_path);
        if !routes.is_empty() {
            info!(count = routes.len(), "restored persisted Caddy routes");
        }
        // Bring routes stored by a pre-#170 helper onto the hostname-keyed id
        // format. Safe to run on every boot: the canonical id is a function of
        // the entry's own hostname, so re-keying is idempotent.
        if canonicalize_route_keys(&mut routes) > 0 {
            write_route_store_blocking(&store_path, &routes);
        }
        Self {
            inner: Arc::new(Mutex::new(CaddyState {
                child_pid: None,
                cert_gate: CertGate::default(),
            })),
            routes: Arc::new(Mutex::new(RouteStore {
                routes,
                path: store_path,
            })),
            reload_lock: Arc::new(Mutex::new(())),
            client: reqwest::Client::builder()
                .connect_timeout(CADDY_CONNECT_TIMEOUT)
                .timeout(CADDY_ADMIN_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            https_port,
            http_port,
            caddy_bin_override: caddy_bin,
        }
    }

    /// Start the Caddy process if it is not already running, and ensure the
    /// base config is loaded.
    pub async fn start(&self) -> Result<()> {
        // Hold the lock across the whole start sequence so a concurrent caller
        // (e.g. the watchdog racing a client `caddy_start`) can't double-spawn
        // Caddy — the second caller waits, then sees the running pid and no-ops.
        let mut state = self.inner.lock().await;
        self.start_locked(&mut state).await
    }

    /// Start Caddy, assuming the caller already holds the state lock. Split out
    /// so `ensure_healthy` can re-check liveness and tear down + restart within
    /// a single held-lock critical section (no concurrent `caddy_start` window).
    async fn start_locked(&self, state: &mut CaddyState) -> Result<()> {
        if let Some(pid) = state.child_pid {
            if is_process_alive(pid) {
                info!(pid, "caddy is already running");
                return Ok(());
            }
            // Stale PID.
            state.child_pid = None;
        }

        // Check if Caddy is already running externally (e.g. an orphaned Caddy
        // from a previous helper instance that exited without stopping it). If
        // so, re-adopt it: record its pid (from the pid file) so we can control
        // it later, then reload our full config (base + persisted routes).
        if self.is_running().await {
            if let Some(pid) = read_caddy_pid(&self.caddy_bin_override) {
                // Only adopt a pid we can positively confirm is caddy. If we
                // can't confirm, don't record it — `stop()` still falls back to
                // the pid file when recovery is actually needed.
                if is_process_alive(pid) && pid_is_caddy(pid).await == Some(true) {
                    state.child_pid = Some(pid);
                    info!(pid, "re-adopted orphaned caddy from pid file");
                }
            }
            info!("caddy admin API already reachable, reloading full config");
            self.reload()
                .await
                .context("failed to reload caddy config on existing instance")?;
            return Ok(());
        }

        let caddy_bin = self
            .caddy_bin_override
            .clone()
            .unwrap_or_else(veld_core::paths::caddy_bin);
        if !caddy_bin.exists() {
            anyhow::bail!("caddy not found at {}", caddy_bin.display());
        }

        let child = tokio::process::Command::new(&caddy_bin)
            .arg("run")
            // Enable HTTP/2 Extended CONNECT (RFC 8441) so Caddy can translate
            // HTTP/2 WebSocket upgrades to HTTP/1.1 for the upstream. Without
            // this, Go's HTTP/2 server doesn't advertise the capability and
            // browsers get 404 on WebSocket connections (kills HMR, live reload).
            // See: golang/go#71128, caddyserver/caddy#7309
            .env("GODEBUG", "http2xconnect=1")
            // Put Caddy in its own process group (pgid = its own pid) so a signal
            // delivered to the helper's launchd job — e.g. `veld update`'s
            // `launchctl kill TERM system/dev.veld.helper`, or a bootout — cannot
            // reach Caddy through the shared group. The helper only ever controls
            // Caddy deliberately (admin-API `/stop` or an explicit kill-by-pid),
            // so detaching the group keeps every live URL up across a helper
            // restart. Mirrors the Linux unit's `KillMode=process`.
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("spawning caddy at {}", caddy_bin.display()))?;

        let pid = child.id().context("failed to get caddy PID")?;
        state.child_pid = Some(pid);
        // Record the pid so a future helper instance that adopts this Caddy (or
        // needs to kill it when wedged) can regain control of it.
        write_caddy_pid(&self.caddy_bin_override, pid);

        // Wait for the admin API to become available, then load the full config
        // (base + persisted routes). The lock is intentionally still held here.
        info!(pid, "caddy process started, loading config...");
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if self
                .client
                .get(format!("{CADDY_ADMIN_API}/config/"))
                .send()
                .await
                .is_ok()
            {
                break;
            }
        }
        self.reload().await.context("failed to load caddy config")?;
        info!(pid, "caddy started with full config");
        Ok(())
    }

    /// Stop the Caddy process, ensuring it is really gone.
    ///
    /// Tries a graceful admin-API `/stop` first (works even for an adopted
    /// orphan while its admin API is responsive), then — critically for the
    /// wedged case where `/stop` timed out — signals the pid (SIGTERM, then
    /// SIGKILL) so the listener ports are freed for a restart. The pid comes
    /// from the tracked child or, failing that, the pid file, so a helper that
    /// adopted an orphaned Caddy can still tear it down.
    pub async fn stop(&self) -> Result<()> {
        let mut state = self.inner.lock().await;
        self.stop_locked(&mut state).await
    }

    /// Stop Caddy, assuming the caller already holds the state lock.
    async fn stop_locked(&self, state: &mut CaddyState) -> Result<()> {
        let known_pid = state.child_pid.take();

        let stop_url = format!("{CADDY_ADMIN_API}/stop");
        match self.client.post(&stop_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("caddy stopped via admin API");
            }
            Ok(resp) => {
                debug!(status = %resp.status(), "caddy /stop non-success; will signal by pid");
            }
            Err(e) => {
                debug!("caddy /stop unreachable: {e}; will signal by pid");
            }
        }

        // Ensure the process is actually dead so its ports are released.
        if let Some(pid) = known_pid.or_else(|| read_caddy_pid(&self.caddy_bin_override)) {
            terminate_pid(pid).await;
        }
        clear_caddy_pid(&self.caddy_bin_override);

        Ok(())
    }

    /// Reload caddy configuration via the admin API.
    ///
    /// The posted config is the base config **plus every persisted per-run
    /// route**. Caddy's `/load` replaces the entire configuration, so emitting
    /// only the base (as this used to) would silently drop all app routes on
    /// every reload — the exact bug that made URLs die after a helper/Caddy
    /// restart. Splicing the stored routes back in makes reload idempotent and
    /// self-healing.
    /// **Serialised.** The snapshot of the route store and the `/load` that
    /// carries it have to be one critical section, because `/load` *replaces*
    /// Caddy's whole configuration: two reloads racing can apply out of order,
    /// and the one that arrives last wins even if its snapshot is older. That
    /// loses a route — Caddy stops serving a hostname the durable store still
    /// lists, with no error anywhere, until something reloads again.
    ///
    /// It was a narrow race before (only a re-added route id reloaded); making
    /// every `add_route` reload put it on the path two runs starting at the same
    /// time take. The lock is held across the network call deliberately: what
    /// needs ordering is the *application* of the config, not just the read of
    /// the store. It is its own lock rather than the route store's so that the
    /// certificate watchdog can still read hostnames while a reload is in flight.
    pub async fn reload(&self) -> Result<()> {
        let _ordering = self.reload_lock.lock().await;
        let load_url = format!("{CADDY_ADMIN_API}/load");
        let stored = self.routes.lock().await.routes.clone();
        // Prepared here, not inside the builder: this is the one path that is
        // about to hand the config to a real Caddy, so it is the only one with
        // any business creating files. A log that cannot be prepared is left out
        // of the config entirely — naming an unopenable log makes Caddy reject
        // every route with it.
        let log_path = caddy_log_path(&self.caddy_bin_override);
        let log_path = prepare_caddy_log(&log_path).then_some(log_path);
        let config = build_full_config(
            self.https_port,
            self.http_port,
            &self.caddy_bin_override,
            &stored,
            log_path.as_deref(),
        );

        let resp = self
            .client
            .post(&load_url)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&config)?)
            .send()
            .await
            .context("posting config to caddy /load")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("caddy /load returned error: {body}");
        }

        info!("caddy configuration reloaded");
        Ok(())
    }

    /// Add a reverse-proxy route via the Caddy admin API.
    pub async fn add_route(
        &self,
        route_id: &str,
        hostname: &str,
        upstream: &str,
        feedback: Option<FeedbackConfig<'_>>,
        proxy: &veld_core::config::ResolvedProxy,
    ) -> Result<()> {
        let route = build_route_json(route_id, hostname, upstream, feedback, proxy);

        // Record in the durable store first (and persist) so the route is
        // replayed on any future reload/restart even if the live reload below
        // fails or Caddy later dies. The store is the source of truth.
        let existed = self.store_route(route_id, route).await;

        // Reload the whole config rather than POSTing the one route.
        //
        // Not a simplification of the old fast path — a requirement of it. A
        // route carries a hostname, and a hostname only gets veld's certificate
        // lifetime if it is named in the certificate policy (see
        // `cert_subjects_mut`), which means the config that adds the route and
        // the config that names it have to be the same config. Posting the route
        // alone would leave Caddy issuing this hostname a 12-hour certificate
        // under a policy of its own making, and by the time any later reload
        // named it, that certificate would already be cached and not reissued
        // until it expired.
        //
        // Measured at ~30ms for a config with 20 routes, and this path already
        // reloaded whenever a route id was re-added (every restart of a run), so
        // it is a well-travelled one rather than a new risk.
        // `existed` is kept in the line because it was a distinguishable signal
        // before this path stopped branching on it: a route id that is already in
        // the store means a run restarting under a reused id, and a log that
        // cannot tell that from a first start is a log that lost something.
        info!(
            route_id,
            hostname,
            upstream,
            replacing = existed,
            "adding caddy route"
        );
        self.reload().await
    }

    /// Remove a route by its `@id` via the Caddy admin API.
    ///
    /// Deliberately *not* a reload, unlike [`Self::add_route`]: this leaves the
    /// removed hostname named in the live certificate policy's `subjects` until
    /// something else reloads. That is inert, and measured to be — certificate
    /// management follows the server's own hostnames, so a subject with no route
    /// has nothing issued or renewed for it. The asymmetry is on purpose: adding a
    /// hostname must name it in the same config or it gets the wrong lifetime,
    /// while un-naming one buys nothing and a full reload would drop live
    /// connections for a route that is going away anyway.
    pub async fn remove_route(&self, route_id: &str) -> Result<()> {
        // Under the same lock a reload takes, because the two mutate Caddy's live
        // config through different doors. A reload that snapshotted the store
        // before this call, and lands after it, replays the route this DELETE just
        // removed — resurrecting a stopped run's hostname, still proxying to a
        // dead upstream, with nothing anywhere saying so. That is reachable the
        // moment one run stops while another starts, which is ordinary.
        let _ordering = self.reload_lock.lock().await;
        self.remove_route_locked(route_id).await
    }

    /// [`Self::remove_route`] for a caller that already holds the ordering lock.
    ///
    /// This split is not tidiness. `tokio::sync::Mutex` is **not** reentrant, so
    /// the version that takes the lock cannot be called from inside a critical
    /// section that already holds it — and `remove_routes_by_prefix` does exactly
    /// that, in a loop. Nesting them deadlocked the helper on its first matching
    /// route: the task waits on a lock it is itself holding, and because
    /// `ensure_healthy` reaches `reload` while holding `inner`, the next liveness
    /// tick then wedges `inner` too — no pid query, no start, no stop, no route
    /// added, until the helper is killed. `veld-daemon` purges `veld-join-*`
    /// routes on startup, so it was reachable on an ordinary boot.
    async fn remove_route_locked(&self, route_id: &str) -> Result<()> {
        // Drop from the durable store first so it is not replayed on reload.
        self.forget_route(route_id).await;

        let url = format!("{CADDY_ADMIN_API}/id/{route_id}");

        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .context("removing route from caddy")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("caddy remove route returned error: {body}");
        }

        info!(route_id, "caddy route removed");
        Ok(())
    }

    /// Remove every route whose `@id` starts with `prefix`. Returns how many were
    /// removed. Used to purge orphaned `veld-join-*` routes on daemon startup.
    pub async fn remove_routes_by_prefix(&self, prefix: &str) -> Result<usize> {
        // Same ordering lock as `remove_route` and `reload`, for the same reason:
        // a reload carrying an older snapshot would put these routes back.
        let _ordering = self.reload_lock.lock().await;
        // Drop matching routes from the durable store first so they are not
        // replayed on the next reload.
        self.forget_routes_by_prefix(prefix).await;

        let list_url = format!("{CADDY_ADMIN_API}/config/apps/http/servers/veld/routes");
        let resp = self
            .client
            .get(&list_url)
            .send()
            .await
            .context("listing caddy routes")?;
        if !resp.status().is_success() {
            // Server/routes not configured yet — nothing to purge.
            return Ok(0);
        }
        let routes: serde_json::Value = resp.json().await.context("parsing caddy routes")?;
        let ids = filter_route_ids_by_prefix(&routes, prefix);
        let mut removed = 0;
        for id in ids {
            // The `_locked` form: this loop already holds the ordering lock, and
            // the lock is not reentrant. See `remove_route_locked`.
            if self.remove_route_locked(&id).await.is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            info!(prefix, removed, "purged caddy routes by prefix");
        }
        Ok(removed)
    }

    /// Check whether caddy is running and reachable by querying the veld
    /// sentinel route. Returns `true` only when our Caddy instance is running
    /// (i.e. the sentinel route exists), not an unrelated Caddy process.
    pub async fn is_running(&self) -> bool {
        let url = format!("{CADDY_ADMIN_API}/id/veld-sentinel");
        matches!(self.client.get(&url).send().await, Ok(r) if r.status().is_success())
    }

    /// Return the stored Caddy child PID, if known.
    pub async fn pid(&self) -> Option<u32> {
        self.inner.lock().await.child_pid
    }

    /// Ensure Caddy is alive and serving. Returns `true` if a recovery restart
    /// was performed. Called by the watchdog loop: if Caddy has died or wedged
    /// (its sentinel route is unreachable within the admin timeout), it is
    /// torn down and respawned, then all persisted routes are replayed via the
    /// base-config reload inside `start()`.
    pub async fn ensure_healthy(&self) -> Result<bool> {
        // Cheap unlocked pre-check: the common case is a healthy Caddy, and we
        // don't want to contend for the lock every tick.
        if self.is_running().await {
            return Ok(false);
        }
        // Acquire the lock and re-check *while holding it*. Any in-flight
        // `caddy_start` holds this same lock, so by the time we get it that
        // start has fully completed — if Caddy is now healthy we must not tear
        // it down (which would drop live connections/HMR). Teardown + restart
        // then happen atomically in this one critical section.
        let mut state = self.inner.lock().await;
        if self.is_running().await {
            return Ok(false);
        }
        warn!("caddy is not responding — attempting recovery restart");
        // Best-effort teardown of any dead/wedged process so the respawn can
        // bind the listeners cleanly.
        let _ = self.stop_locked(&mut state).await;
        self.start_locked(&mut state)
            .await
            .context("failed to restart caddy during recovery")?;
        info!("caddy recovered");
        Ok(true)
    }

    /// Ensure the certificate Caddy serves is one a browser accepts, restarting
    /// Caddy when renewal has provably stopped. Returns `true` if it restarted.
    ///
    /// **A restart, not a reload** — this is the whole reason this exists next to
    /// [`Self::ensure_healthy`] rather than inside it. Caddy answering its admin
    /// API says nothing about its certificate maintenance, which is one
    /// goroutine on one ticker: whatever stalls it, Caddy keeps serving the leaf
    /// it already has, forever, and every route veld adds or removes reloads a
    /// config that cannot fix it. certmagic's `manageOne` returns early for any
    /// name already cached as managed ("maintenance will continue"), so a reload
    /// never re-examines an expired certificate. Only a new process, whose cache
    /// starts empty and is filled from storage, renews it.
    ///
    /// Deliberately conservative: it acts only on a certificate verdict (never
    /// on an unreachable or unreadable probe — [`ensure_healthy`] owns that
    /// failure), only after [`CERT_STRIKES_BEFORE_RESTART`] consecutive bad
    /// probes, at most once per [`CERT_RESTART_COOLDOWN`], and at most
    /// [`CERT_RESTART_ATTEMPTS`] times before it gives up and says so.
    ///
    /// **Every hostname veld serves is probed, not just the management host.**
    /// Each one carries its own leaf, issued when its run first started, so a run
    /// URL can be expired while `veld.localhost` is still valid — and a watchdog
    /// that only asked the canary would sit out exactly that outage. The worst
    /// verdict wins, so one hostname that answers nothing cannot hide another's
    /// expired certificate.
    ///
    /// [`ensure_healthy`]: Self::ensure_healthy
    pub async fn ensure_cert_healthy(&self) -> Result<bool> {
        // The pid this verdict is about. The liveness watchdog runs on its own
        // (faster) tick and may replace Caddy while the probes below are in
        // flight; restarting on a verdict about the process it already replaced
        // would tear down a fresh, renewing Caddy — the mistake `ensure_healthy`
        // documents at its own re-check.
        let probed_pid = self.pid().await;

        let mut hosts: Vec<String> = vec![MANAGEMENT_HOST.to_owned()];
        hosts.extend(self.routes.lock().await.hostnames());
        hosts.sort();
        hosts.dedup();

        // Concurrently, because these are bounded by a timeout rather than by
        // work: a wedged Caddy that accepts connections and never answers costs
        // `REQUEST_TIMEOUT` *per host*, so a machine with twenty routes would
        // have taken three and a half minutes to finish one 60-second tick — and
        // the pid re-check below would then usually throw the result away.
        let mut probes = tokio::task::JoinSet::new();
        for host in hosts {
            let port = self.https_port;
            probes.spawn(async move {
                let health = veld_core::tls_health::probe_host(&host, port).await;
                (host, health)
            });
        }
        let mut verdicts = Vec::new();
        while let Some(joined) = probes.join_next().await {
            match joined {
                Ok(verdict) => verdicts.push(verdict),
                // A probe task that panicked has told us nothing about a
                // certificate; the remaining hosts still have.
                Err(e) => warn!(error = %e, "a certificate probe task failed"),
            }
        }
        let all_healthy = veld_core::tls_health::all_healthy(&verdicts);
        let Some((host, health)) = veld_core::tls_health::worst(verdicts) else {
            return Ok(false);
        };

        let mut state = self.inner.lock().await;
        // Before weighing, not after: a verdict about a process that no longer
        // exists must not spend a strike or stamp a restart the gate would then
        // count against the next real one.
        if state.child_pid != probed_pid {
            info!(
                "caddy was replaced while its certificate was being probed; re-checking next tick"
            );
            return Ok(false);
        }

        match state
            .cert_gate
            .weigh(&health, all_healthy, std::time::Instant::now())
        {
            CertVerdict::Fine => return Ok(false),
            CertVerdict::Recovered => {
                // Deliberately *not* logging `host`/`health` here. Those come from
                // `worst()`, and the state this arm newly covers is "the
                // certificates are fine again even though one hostname still
                // answers nothing" — so the fields would have read
                // `healthy again host=never.localhost health=Unreachable`, a line
                // that argues with itself in the one log this change exists to
                // make trustworthy. The worst verdict is already logged, every
                // tick it is not healthy, by the arms below.
                info!("caddy certificates are healthy again");
                return Ok(false);
            }
            CertVerdict::Overdue { strikes } => {
                warn!(
                    host,
                    ?health,
                    strikes,
                    "caddy is serving a certificate its renewal should have replaced"
                );
                return Ok(false);
            }
            CertVerdict::GaveUp => {
                warn!(
                    host,
                    ?health,
                    attempts = CERT_RESTART_ATTEMPTS,
                    "restarting caddy has not renewed its certificate; leaving it alone — \
                     see caddy's own log for why issuance is failing"
                );
                return Ok(false);
            }
            CertVerdict::Restart => {}
        }

        warn!(
            host,
            ?health,
            "restarting caddy to renew the certificate its own maintenance did not"
        );
        let _ = self.stop_locked(&mut state).await;
        self.start_locked(&mut state)
            .await
            .context("failed to restart caddy to renew its certificates")?;
        info!("caddy restarted for certificate renewal");
        Ok(true)
    }

    /// On helper startup, re-adopt and reload an already-running Caddy — e.g.
    /// one left running across our own binary self-restart. This re-applies the
    /// current base config (so an updated Caddy binary / new built-in routes
    /// take effect), replays persisted routes, and records the pid so the
    /// watchdog supervises it thereafter. It deliberately does NOT spawn a new
    /// Caddy: a cold boot with no runs stays idle until the first `caddy_start`.
    pub async fn reconcile_on_startup(&self) {
        if !self.is_running().await {
            return;
        }
        let mut state = self.inner.lock().await;
        if let Err(e) = self.start_locked(&mut state).await {
            warn!(error = %format!("{e:#}"), "startup caddy reconcile failed");
        }
    }

    // -- Route store ----------------------------------------------------------

    /// Insert or replace a route in the durable store and persist it. Returns
    /// `true` if a route with this id was already present (i.e. a replace).
    ///
    /// The persist happens while the store lock is held so concurrent route
    /// operations can't write stale snapshots over each other (route ops are
    /// infrequent, so serializing them is cheap and keeps the file correct).
    async fn store_route(&self, route_id: &str, route: serde_json::Value) -> bool {
        let mut store = self.routes.lock().await;
        let existed = store.routes.insert(route_id.to_owned(), route).is_some();
        let snapshot = store.snapshot();
        persist_route_store(&snapshot).await;
        existed
    }

    /// Remove a route from the durable store and persist it.
    async fn forget_route(&self, route_id: &str) {
        let mut store = self.routes.lock().await;
        if store.routes.remove(route_id).is_none() {
            return;
        }
        let snapshot = store.snapshot();
        persist_route_store(&snapshot).await;
    }

    /// Remove every stored route whose id starts with `prefix` and persist.
    async fn forget_routes_by_prefix(&self, prefix: &str) {
        let mut store = self.routes.lock().await;
        let before = store.routes.len();
        store.routes.retain(|id, _| !id.starts_with(prefix));
        if store.routes.len() == before {
            return;
        }
        let snapshot = store.snapshot();
        persist_route_store(&snapshot).await;
    }

    /// Number of routes currently in the durable store (for status/tests).
    pub async fn stored_route_count(&self) -> usize {
        self.routes.lock().await.routes.len()
    }
}

impl RouteStore {
    /// Every hostname this helper is serving, as the stored routes name it.
    ///
    /// The same source `build_full_config` names in the certificate policy, so
    /// what the watchdog probes and what veld asked Caddy to certify cannot
    /// disagree.
    fn hostnames(&self) -> Vec<String> {
        self.routes
            .values()
            .filter_map(|route| stored_route_hostname(route).map(str::to_ascii_lowercase))
            .collect()
    }

    /// Clone the routes + path for persisting outside the lock.
    fn snapshot(&self) -> RouteSnapshot {
        RouteSnapshot {
            path: self.path.clone(),
            routes: self.routes.clone(),
        }
    }
}

/// A point-in-time copy of the route store, used to persist to disk without
/// holding the store lock across the async write.
struct RouteSnapshot {
    path: PathBuf,
    routes: HashMap<String, serde_json::Value>,
}

/// Caddy's data directory — where we persist the route store and pid file.
/// Derived from the caddy binary's parent (a sibling `caddy-data`) so it shares
/// the same (helper-writable) location across privileged/user modes.
pub(crate) fn caddy_data_dir(caddy_bin_override: &Option<PathBuf>) -> PathBuf {
    caddy_bin_override
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.join("caddy-data"))
        .unwrap_or_else(veld_core::paths::caddy_data_dir)
}

/// Where the persisted route store lives.
fn routes_store_path(caddy_bin_override: &Option<PathBuf>) -> PathBuf {
    caddy_data_dir(caddy_bin_override).join("veld-routes.json")
}

/// Where Caddy writes its own log — inside its data directory. See
/// [`veld_core::paths::caddy_log_path`] for why that directory and not the one
/// the other service logs live in.
pub(crate) fn caddy_log_path(caddy_bin_override: &Option<PathBuf>) -> PathBuf {
    caddy_data_dir(caddy_bin_override).join(veld_core::paths::CADDY_LOG_FILENAME)
}

/// Open `path` for appending, creating it, and **never** following a symlink.
///
/// Extracted from [`prepare_caddy_log`] so the part that actually closes the
/// symlink door has something a test can point at: the `symlink_metadata` check
/// in the caller refuses a link that is already there, so it alone accounts for
/// every assertion a test of the caller can make, and deleting the flag here
/// would not have failed any of them.
fn open_log_for_append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Make `path` a file Caddy can open for appending, and say whether it is.
///
/// **A missing log must never cost the user their routes.** Configuring the
/// `logging` app makes the whole config all-or-nothing on this one file: Caddy's
/// `provisionCommon` fails the *entire* `/load` — every route with it — when the
/// default log's writer cannot be opened. A root-owned `0600` `caddy.log` left
/// behind by a privileged install, met by an unprivileged one, is exactly that
/// file, and this repo has already paid for the lesson once in `prepare_daemon_log`.
/// So the helper proves the path is usable *before* naming it in a config, and the
/// caller omits the `logging` block when it is not.
///
/// Also refuses a symlink rather than following one: Caddy opens its log with
/// `O_CREATE` and no `O_NOFOLLOW`, so in privileged mode a symlink here is root
/// appending to a file somebody else chose. The containing directory is
/// root-owned, which is what makes this a belt-and-braces check rather than the
/// only defence.
fn prepare_caddy_log(path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(error = %e, path = %parent.display(), "cannot create caddy log directory — caddy will run without a log");
            return false;
        }
    }

    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            warn!(
                path = %path.display(),
                "caddy log path is a symlink; refusing to let caddy write through it"
            );
            return false;
        }
        Ok(meta) if !meta.is_file() => {
            warn!(path = %path.display(), "caddy log path is not a regular file");
            return false;
        }
        _ => {}
    }

    // Create it ourselves, world-readable, so the person the docs send here can
    // actually read it: Caddy would create it `0600`, and in privileged mode that
    // means root-owned and unreadable to the user who needs it. Setting Caddy's
    // own `mode` key instead would make Caddy `chmod` the path on every load, and
    // `chmod` follows symlinks.
    //
    // `O_NOFOLLOW` and `fchmod` — not the check above — are what actually close
    // that door. The `symlink_metadata` check is check-then-use: a symlink
    // planted between it and this open would be followed, and a *path*-based
    // `set_permissions` would then chmod whatever it points at. Opening with
    // `O_NOFOLLOW` fails instead of following, and permissions go through the
    // descriptor already open, which nothing can redirect afterwards.
    match open_log_for_append(path) {
        Ok(file) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) = file.set_permissions(std::fs::Permissions::from_mode(0o644)) {
                    debug!(error = %e, "could not relax caddy log permissions");
                }
            }
            true
        }
        Err(e) => {
            warn!(error = %e, path = %path.display(), "caddy log is not writable — caddy will run without a log");
            false
        }
    }
}

/// Where the managed Caddy's pid is recorded, so a restarted helper can
/// re-adopt (or forcibly stop) an orphaned Caddy it did not itself spawn.
fn caddy_pid_path(caddy_bin_override: &Option<PathBuf>) -> PathBuf {
    caddy_data_dir(caddy_bin_override).join("caddy.pid")
}

fn write_caddy_pid(caddy_bin_override: &Option<PathBuf>, pid: u32) {
    let path = caddy_pid_path(caddy_bin_override);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Warn (don't silently swallow) on failure: e.g. after a mode switch the
    // data dir may be owned by the other mode's helper, and a silent EACCES here
    // would quietly disable pid-based recovery.
    if let Err(e) = std::fs::write(&path, pid.to_string()) {
        warn!(error = %e, path = %path.display(), "failed to write caddy pid file");
    }
}

fn read_caddy_pid(caddy_bin_override: &Option<PathBuf>) -> Option<u32> {
    std::fs::read_to_string(caddy_pid_path(caddy_bin_override))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn clear_caddy_pid(caddy_bin_override: &Option<PathBuf>) {
    let _ = std::fs::remove_file(caddy_pid_path(caddy_bin_override));
}

/// Whether `pid` currently belongs to a caddy process, used to guard signalling
/// against PID reuse after a stale pid file. Returns:
/// - `Some(true)`  — confirmed a caddy process,
/// - `Some(false)` — `ps` ran and it is not caddy (or the pid is gone),
/// - `None`        — `ps` could not be executed, so we can't tell.
///
/// Uses an absolute `/bin/ps` path (present on macOS and Linux) so PATH quirks
/// in a launchd/systemd environment don't turn this into a false "not caddy".
async fn pid_is_caddy(pid: u32) -> Option<bool> {
    match tokio::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .await
    {
        Ok(o) if o.status.success() => {
            // macOS `ps -o comm=` prints the full executable path; Linux prints
            // the command name. Compare the basename EXACTLY so we don't match
            // an unrelated process like `/x/notcaddy` or `mycaddy`.
            let comm = String::from_utf8_lossy(&o.stdout);
            let name = comm.trim().rsplit('/').next().unwrap_or("").trim();
            Some(name == "caddy")
        }
        // ps ran but the pid wasn't found → not a live caddy.
        Ok(_) => Some(false),
        // ps couldn't be executed → undetermined.
        Err(_) => None,
    }
}

/// Terminate a caddy process by pid: SIGTERM, wait for graceful exit, then
/// escalate to SIGKILL. Guarded by [`pid_is_caddy`] so a stale/reused pid is
/// never signalled. Bounded so it cannot hang the caller.
async fn terminate_pid(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    if !is_process_alive(pid) {
        return;
    }
    // Only refuse to signal when we've CONFIRMED the pid is some other process
    // (stale pid file + reuse). If `ps` is unavailable (`None`) we proceed:
    // the pid came from our own pid file, and refusing would strand a wedged
    // Caddy and defeat recovery — the whole point of this path.
    if pid_is_caddy(pid).await == Some(false) {
        warn!(
            pid,
            "pid is not a caddy process; not signalling (stale pid file?)"
        );
        return;
    }

    let nix_pid = Pid::from_raw(pid as i32);
    let _ = kill(nix_pid, Signal::SIGTERM);
    info!(pid, "sent SIGTERM to caddy");
    // Up to ~3s for graceful shutdown.
    for _ in 0..30 {
        if !is_process_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    warn!(pid, "caddy did not exit after SIGTERM; sending SIGKILL");
    let _ = kill(nix_pid, Signal::SIGKILL);
    for _ in 0..20 {
        if !is_process_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    warn!(pid, "caddy still present after SIGKILL");
}

/// Load the persisted route store from disk. Missing/corrupt files start empty
/// (self-healing rather than fatal).
fn load_route_store(path: &Path) -> HashMap<String, serde_json::Value> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            warn!(error = %e, "failed to parse persisted routes; starting with an empty store");
            HashMap::new()
        }),
        Err(_) => HashMap::new(),
    }
}

/// Route ids the startup canonicalisation must leave alone.
///
/// - `veld-join-` — share-join routes. The daemon keeps `(hostname, route_id)`
///   pairs in memory and removes by the *stored* id, so re-keying one behind its
///   back would leak the route when the joiner leaves.
/// - `veld-mgmt-{host}` — a dev instance's management route, added and removed by
///   the daemon under an id it recomputes from `VELD_MANAGEMENT_HOST`.
/// - `veld-management` / `veld-sentinel` — part of the base config, never stored,
///   listed defensively.
///
/// Matched **structurally**, not by bare prefix: a legacy run route id is
/// `veld-{run_name}-{node}-{variant}` and run names are nearly unconstrained
/// (`is_safe_identifier`), so a run called `join-feature` or `mgmt-2` produces a
/// key that a prefix test would mistake for a reserved one — and skipping it
/// would leave a stale entry to out-sort the canonical route for its hostname.
///
/// - `veld-mgmt-{host}` is self-verifying: the suffix *is* the route's own host.
/// - `veld-join-{join_id}-{node}` uses `join_` + 8 hex (`share::manager::gen_id`),
///   which no plausible `{run_name}-{node}-{variant}` reproduces.
fn is_reserved_route_id(id: &str, route: &serde_json::Value) -> bool {
    if id == "veld-management" || id == "veld-sentinel" {
        return true;
    }
    if let Some(host) = id.strip_prefix("veld-mgmt-") {
        if stored_route_hostname(route) == Some(host) {
            return true;
        }
    }
    if let Some(tail) = id
        .strip_prefix("veld-join-")
        .and_then(|rest| rest.strip_prefix("join_"))
    {
        // 8 ASCII hex characters then the node separator. The hex check keeps
        // byte index 8 on a char boundary.
        let is_join_id = tail.len() > 8
            && tail[..8].chars().all(|c| c.is_ascii_hexdigit())
            && tail.as_bytes()[8] == b'-';
        if is_join_id {
            return true;
        }
    }
    false
}

/// Read a stored route's hostname back out of the route JSON itself
/// (`match[0].host[0]`, as `build_route_json` writes it).
fn stored_route_hostname(route: &serde_json::Value) -> Option<&str> {
    route.get("match")?.get(0)?.get("host")?.get(0)?.as_str()
}

/// Re-key persisted routes to `veld_core::url::run_route_id(hostname)`,
/// returning how many entries moved.
///
/// This is the whole compatibility story for #170: routes written by an older
/// helper are keyed `veld-{run}-{node}-{variant}`, which collides across
/// projects. Because the new id derives from the hostname — and every stored
/// route carries its own hostname — the store can migrate itself with no
/// version marker, no dual-read window, and no information from the caller.
///
/// Several entries claiming one hostname collapse into one, with a warning:
/// Caddy matches on host, so such a set had no defined winner to begin with
/// (`build_full_config` orders by `@id`). An id that is already occupied is
/// never overwritten — the occupant was either written by a current helper or
/// reached first in sorted key order, so the survivor is the same on every boot.
fn canonicalize_route_keys(routes: &mut HashMap<String, serde_json::Value>) -> usize {
    let mut keys: Vec<String> = routes.keys().cloned().collect();
    keys.sort();

    let mut moved = 0;
    for key in keys {
        let Some(route) = routes.get(&key) else {
            continue;
        };
        if is_reserved_route_id(&key, route) {
            continue;
        }
        let Some(hostname) = stored_route_hostname(route) else {
            warn!(
                route_id = %key,
                "persisted route has no host match — leaving its id alone"
            );
            continue;
        };
        // Normalised exactly as the add and removal sides do: a `urlTemplate` can
        // carry a literal port or path (`app.localhost:3000`), and re-keying to an
        // id derived from the un-normalised host would produce an entry no
        // teardown path can ever recompute. Two such hosts differing only in port
        // do collapse onto one id — correctly: Caddy's host matcher compares
        // against the request's host with the port already split off, so a `host`
        // match carrying a port never matched anything to begin with.
        let canonical = veld_core::url::run_route_id(veld_core::url::hostname_of_url(hostname));
        if canonical == key {
            continue;
        }
        if routes.contains_key(&canonical) {
            warn!(
                route_id = %key,
                canonical_id = %canonical,
                hostname = %hostname,
                "dropping a persisted route whose hostname is already claimed"
            );
            routes.remove(&key);
            moved += 1;
            continue;
        }
        let Some(mut route) = routes.remove(&key) else {
            continue;
        };
        route["@id"] = serde_json::json!(&canonical);
        routes.insert(canonical, route);
        moved += 1;
    }

    if moved > 0 {
        info!(count = moved, "re-keyed persisted Caddy routes by hostname");
    }
    moved
}

/// Blocking twin of [`persist_route_store`], for the one caller that runs before
/// the store is behind its async lock: `CaddyManager::new`.
fn write_route_store_blocking(path: &Path, routes: &HashMap<String, serde_json::Value>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = match serde_json::to_vec_pretty(routes) {
        Ok(j) => j,
        Err(e) => {
            warn!(error = %e, "failed to serialize re-keyed routes");
            return;
        }
    };
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &json) {
        warn!(error = %e, path = %tmp.display(), "failed to write re-keyed routes store");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        warn!(error = %e, "failed to move re-keyed routes store into place");
    }
}

/// Persist the route store atomically (write to a temp file, then rename).
async fn persist_route_store(snapshot: &RouteSnapshot) {
    if let Some(parent) = snapshot.path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let json = match serde_json::to_vec_pretty(&snapshot.routes) {
        Ok(j) => j,
        Err(e) => {
            warn!(error = %e, "failed to serialize routes for persistence");
            return;
        }
    };
    // Per-process tmp name so two helpers sharing a data dir don't clobber
    // each other's temp file mid-write.
    let tmp = snapshot
        .path
        .with_extension(format!("json.tmp.{}", std::process::id()));
    if let Err(e) = tokio::fs::write(&tmp, &json).await {
        warn!(error = %e, path = %tmp.display(), "failed to write routes store");
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp, &snapshot.path).await {
        warn!(error = %e, "failed to move routes store into place");
    }
}

// ---------------------------------------------------------------------------
// Feedback config
// ---------------------------------------------------------------------------

/// Configuration for feedback overlay / client-side injection on a route.
pub struct FeedbackConfig<'a> {
    pub upstream: &'a str,
    pub run_name: &'a str,
    pub project_root: &'a str,
    /// Comma-separated client log levels (e.g. "log,warn,error").
    pub client_log_levels: &'a str,
    /// Whether to automatically inject bootstrap scripts into HTML responses.
    /// When `false`, the `/__veld__/*` routes are still created for manual injection.
    pub inject: bool,
    /// Whether to inject the feedback overlay toolbar.
    pub inject_feedback_overlay: bool,
    /// Whether to inject the client-side log collector.
    pub inject_client_logs: bool,
}

// ---------------------------------------------------------------------------
// Caddy JSON config builders
// ---------------------------------------------------------------------------

/// Build the full Caddy config: the base config plus every persisted per-run
/// route, spliced in after the built-in management/sentinel routes and sorted
/// by `@id` for deterministic ordering. This is what `reload()` posts to
/// Caddy's `/load`, so a reload never drops app routes.
fn build_full_config(
    https_port: u16,
    http_port: u16,
    caddy_bin_override: &Option<std::path::PathBuf>,
    stored: &HashMap<String, serde_json::Value>,
    log_path: Option<&Path>,
) -> serde_json::Value {
    let mut config = build_base_config(https_port, http_port, caddy_bin_override, log_path);

    let mut entries: Vec<_> = stored.values().cloned().collect();
    entries.sort_by(|a, b| {
        a.get("@id")
            .and_then(|v| v.as_str())
            .cmp(&b.get("@id").and_then(|v| v.as_str()))
    });
    if !entries.is_empty() {
        if let Some(routes) = config["apps"]["http"]["servers"]["veld"]["routes"].as_array_mut() {
            routes.extend(entries);
        }
    }

    // Every hostname veld serves has to be *named* in the certificate policy, or
    // it does not get veld's certificate lifetime. See `cert_subjects_mut`.
    //
    // **Deduplicated across the whole list, management host included.** Caddy
    // rejects a config that names one host in more than one policy — and its
    // check is a single `hostSet` map, so it catches a repeat *within* one policy
    // too (`caddytls/tls.go`'s `Validate`): `cannot apply more than one
    // automation policy to host`. A `veld.json` is free to template a hostname
    // that comes out as `veld.localhost`, and the route is persisted before the
    // reload, so an un-deduplicated list would make every later `/load` fail —
    // leaving Caddy with no config at all, no working URL on the machine, and the
    // liveness watchdog respawning it forever. Measured against Caddy 2.11.4: a
    // duplicated subject fails `caddy validate` outright.
    // Lowercased, because Caddy normalises the two sides of this differently: a
    // host *matcher* is lowercased when it is provisioned, while an automation
    // policy's subjects are only IDNA-encoded — and autohttps excludes a name
    // from its own 12-hour issuer on an exact string compare. So one capital
    // letter in a project's URL template means the subject never matches its own
    // route, and that hostname silently gets the short certificate this list
    // exists to prevent. `url::run_route_id` already lowercases; the hostname
    // itself reaches the store unnormalised.
    let mut hostnames: Vec<String> = std::iter::once(MANAGEMENT_HOST.to_ascii_lowercase())
        .chain(
            stored
                .values()
                .filter_map(|route| stored_route_hostname(route).map(str::to_ascii_lowercase)),
        )
        .collect();
    hostnames.sort();
    hostnames.dedup();
    if let Some(subjects) = cert_subjects_mut(&mut config) {
        *subjects = hostnames
            .into_iter()
            .map(serde_json::Value::String)
            .collect();
    }
    config
}

/// The `subjects` array of the certificate policy that carries veld's lifetimes.
///
/// **Naming every hostname is load-bearing, not documentation.** Caddy's
/// automatic HTTPS collects the hostnames a server serves and, for any of them
/// that cannot hold a public certificate — which is every `*.localhost`, i.e.
/// nearly every veld URL — builds a *fresh* internal issuer of its own and
/// overwrites whatever the matching policy configured, "bypassing the
/// JSON-unmarshaling step" (`caddyhttp/autohttps.go`). The one escape is an
/// automation policy that lists the name: a hostname is left alone only when it
/// matches some policy's subject **exactly** — no wildcards, a string compare.
/// So `"lifetime": "168h"` on a catch-all policy is silently ignored for
/// `*.localhost` and every such certificate is issued with Caddy's 12-hour
/// default. Measured, not deduced: with the name listed a leaf comes back 7
/// days, without it 12 hours, same config otherwise.
///
/// A subject with no route is inert — also measured: certificate management
/// follows the *server's* hostnames, so a name left here after its run stopped
/// has no certificate issued or renewed for it.
fn cert_subjects_mut(config: &mut serde_json::Value) -> Option<&mut Vec<serde_json::Value>> {
    config["apps"]["tls"]["automation"]["policies"][0]["subjects"].as_array_mut()
}

/// Build a minimal base Caddy config with a server block for Veld.
/// `log_path` is `Some` only when the caller has already proved the file is one
/// Caddy can open — see [`prepare_caddy_log`]. Passed in rather than derived here
/// so that building a config stays a pure function: it used to prepare the file
/// itself, which meant every test that built a config wrote into the developer's
/// real install, and which branch the test took depended on that machine.
fn build_base_config(
    https_port: u16,
    http_port: u16,
    caddy_bin_override: &Option<std::path::PathBuf>,
    log_path: Option<&Path>,
) -> serde_json::Value {
    // If caddy_bin was overridden, derive data_dir from its parent (sibling "caddy-data").
    let data_dir = caddy_bin_override
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.join("caddy-data"))
        .unwrap_or_else(veld_core::paths::caddy_data_dir);
    // Ensure the data directory exists so Caddy can write PKI data.
    let _ = std::fs::create_dir_all(&data_dir);

    let https_listen = format!(":{https_port}");
    let http_listen = format!(":{http_port}");
    let management_upstream = format!("127.0.0.1:{DAEMON_HTTP_PORT}");

    let mut config = serde_json::json!({
        "storage": {
            "module": "file_system",
            "root": data_dir.to_string_lossy()
        },
        "apps": {
            "http": {
                "servers": {
                    "veld": {
                        "listen": [https_listen, http_listen],
                        "routes": [
                            // First, and on every hostname: the one route Caddy
                            // answers entirely by itself. Anything checking
                            // "is Caddy serving?" — `veld doctor`, the TLS
                            // health probe — gets an answer that does not
                            // depend on the daemon being up, which it would if
                            // the management route below (host-matched and
                            // terminal, so it wins for `MANAGEMENT_HOST`) got
                            // to this path first.
                            {
                                "@id": "veld-sentinel",
                                "match": [{"path": ["/__veld_sentinel__"]}],
                                "handle": [{"handler": "static_response", "body": "veld"}],
                                "terminal": true
                            },
                            {
                                "@id": "veld-management",
                                "match": [{"host": [MANAGEMENT_HOST]}],
                                "handle": [{
                                    "handler": "reverse_proxy",
                                    "upstreams": [{"dial": management_upstream}]
                                }],
                                "terminal": true
                            }
                        ]
                    }
                }
            },
            "pki": {
                "certificate_authorities": {
                    "local": {
                        "name": "Veld Local CA",
                        "intermediate_lifetime": INTERMEDIATE_LIFETIME
                    }
                }
            },
            "tls": {
                "automation": {
                    "policies": [
                        // Veld's own hostnames, named one by one because that is
                        // the only way the lifetime below survives Caddy's
                        // automatic HTTPS — see `cert_subjects_mut`.
                        {
                            "subjects": [MANAGEMENT_HOST],
                            "issuers": [{
                                "module": "internal",
                                "lifetime": LEAF_LIFETIME
                            }]
                        },
                        // The catch-all, which must stay: without a policy of
                        // its own, a hostname that *does* qualify for a public
                        // certificate (a project serving `*.dev.example.com`
                        // locally) is handed Caddy's default ACME issuer, and
                        // veld would try to get a real certificate from Let's
                        // Encrypt for a local development URL.
                        {
                            "issuers": [{
                                "module": "internal",
                                "lifetime": LEAF_LIFETIME
                            }]
                        }
                    ]
                }
            }
        }
    });

    // Caddy writes its own log, because the process we spawn has stdout and
    // stderr pointed at /dev/null, and certificate issuance and renewal — the
    // part of Caddy veld depends on completely and cannot do anything about — is
    // only ever reported there. Added **only** once the file has been proven
    // openable: naming an unopenable log makes Caddy reject the entire config,
    // routes included. See `prepare_caddy_log`.
    if let Some(log_path) = log_path {
        config["logging"] = serde_json::json!({
            "logs": {
                "default": {
                    "level": "INFO",
                    "encoder": {"format": "console"},
                    "writer": {
                        "output": "file",
                        "filename": log_path.to_string_lossy(),
                        "roll_size_mb": LOG_ROLL_SIZE_MB,
                        "roll_keep": LOG_ROLL_KEEP
                    }
                }
            }
        });
    }
    config
}

/// Build a single route entry with hostname matching, TLS, and reverse proxy.
///
/// When feedback is configured:
/// 1. `/__veld__/*` routes to the daemon's feedback HTTP server (API + assets)
///    with `X-Veld-Run` and `X-Veld-Project` headers injected by Caddy.
/// 2. The main app proxy uses the `veld_inject` Caddy handler to prepend a
///    bootstrap `<script>` tag to HTML responses. The handler streams the
///    response without buffering — it writes the prefix before the first body
///    chunk and passes the rest through. This enables streaming SSR, WebSocket
///    upgrades, and SSE without any bypass routes (the handler properly
///    delegates Flusher and Hijacker).
// Route ids starting with the given prefix, taken from a Caddy routes array.
fn filter_route_ids_by_prefix(routes: &serde_json::Value, prefix: &str) -> Vec<String> {
    routes
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.get("@id").and_then(|v| v.as_str()))
                .filter(|id| id.starts_with(prefix))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Translate resolved proxy header rules into a Caddy `reverse_proxy` `headers`
/// object (`{"request": {...}, "response": {...}}`). Caddy's `set` takes an
/// array of values per header, so single values are wrapped. Returns `None` when
/// there are no rules, so the caller can omit the `headers` key entirely.
fn caddy_proxy_headers(proxy: &veld_core::config::ResolvedProxy) -> Option<serde_json::Value> {
    fn side(rules: &veld_core::config::HeaderRules) -> Option<serde_json::Value> {
        if rules.is_empty() {
            return None;
        }
        let mut obj = serde_json::Map::new();
        if !rules.remove.is_empty() {
            obj.insert("delete".into(), serde_json::json!(rules.remove));
        }
        if !rules.set.is_empty() {
            let set: serde_json::Map<String, serde_json::Value> = rules
                .set
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::json!([v])))
                .collect();
            obj.insert("set".into(), serde_json::Value::Object(set));
        }
        Some(serde_json::Value::Object(obj))
    }

    if proxy.is_empty() {
        return None;
    }
    // At least one side is non-empty (guarded above), so `headers` is always
    // populated here.
    let mut headers = serde_json::Map::new();
    if let Some(req) = side(&proxy.request) {
        headers.insert("request".into(), req);
    }
    if let Some(resp) = side(&proxy.response) {
        headers.insert("response".into(), resp);
    }
    Some(serde_json::Value::Object(headers))
}

fn build_route_json(
    route_id: &str,
    hostname: &str,
    upstream: &str,
    feedback: Option<FeedbackConfig<'_>>,
    proxy: &veld_core::config::ResolvedProxy,
) -> serde_json::Value {
    let mut subroutes = Vec::new();

    // Caddy `reverse_proxy` header manipulation, built from the resolved
    // `proxy` config. `None` when there are no rules — the `headers` key is
    // then omitted entirely and headers pass through untouched (the default;
    // veld no longer strips `Origin` unless the config asks it to).
    //
    // Unlike the public web gateway (which MUST rewrite Origin/Host/Referer
    // coherently because its public host differs from the origin host), the
    // local proxy does NO intrinsic Origin/Host rewrite: the browser's request
    // to `*.localhost` is already same-origin with the upstream, so nothing
    // needs translating. Only the user-config layer below applies here — don't
    // "align" the two by re-adding an unconditional Origin rewrite/strip.
    let proxy_headers = caddy_proxy_headers(proxy);

    if let Some(fb) = feedback {
        // /__veld__/* → strip prefix, proxy to daemon with context headers.
        subroutes.push(serde_json::json!({
            "match": [{ "path": ["/__veld__/*"] }],
            "handle": [
                {
                    "handler": "rewrite",
                    "strip_path_prefix": "/__veld__"
                },
                {
                    "handler": "reverse_proxy",
                    "headers": {
                        "request": {
                            "set": {
                                "X-Veld-Run": [fb.run_name],
                                "X-Veld-Project": [fb.project_root]
                            }
                        }
                    },
                    "upstreams": [{ "dial": fb.upstream }]
                }
            ]
        }));

        let bootstrap = if fb.inject {
            build_bootstrap_script(&fb)
        } else {
            String::new()
        };

        // Accept-Encoding: identity is set by the veld_inject handler itself
        // (not the proxy config) so non-HTML requests get normal compression.

        if bootstrap.is_empty() {
            // No injection (either inject:false or both features disabled).
            // Plain reverse proxy, but /__veld__/* routes above are still active
            // for manual script tag usage.
            let mut handler = serde_json::json!({
                "handler": "reverse_proxy",
                "flush_interval": -1,
                "upstreams": [{ "dial": upstream }]
            });
            if let Some(h) = &proxy_headers {
                handler["headers"] = h.clone();
            }
            subroutes.push(serde_json::json!({ "handle": [handler] }));
        } else {
            // veld_inject prepends the bootstrap script to text/html responses
            // without buffering. Accept-Encoding: identity ensures the upstream
            // sends uncompressed HTML (can't prepend to gzipped bytes).
            // flush_interval: -1 disables response buffering so that React
            // streaming hydration (chunked transfer-encoding) works correctly.
            let mut handler = serde_json::json!({
                "handler": "reverse_proxy",
                "flush_interval": -1,
                "upstreams": [{ "dial": upstream }]
            });
            if let Some(h) = &proxy_headers {
                handler["headers"] = h.clone();
            }
            subroutes.push(serde_json::json!({
                "handle": [
                    {
                        "handler": "veld_inject",
                        "prefix": bootstrap
                    },
                    handler
                ]
            }));
        }
    } else {
        // No feedback — plain reverse proxy.
        // flush_interval: -1 passes through chunked/streamed responses
        // immediately (required for React streaming hydration, SSE, etc.).
        let mut handler = serde_json::json!({
            "handler": "reverse_proxy",
            "flush_interval": -1,
            "upstreams": [{ "dial": upstream }]
        });
        if let Some(h) = &proxy_headers {
            handler["headers"] = h.clone();
        }
        subroutes.push(serde_json::json!({ "handle": [handler] }));
    }

    serde_json::json!({
        "@id": route_id,
        "match": [{
            "host": [hostname]
        }],
        "handle": [
            {
                "handler": "subroute",
                "routes": subroutes
            }
        ],
        "terminal": true
    })
}

/// Build the inline bootstrap `<script>` tag that is injected into HTML
/// responses by the `veld_inject` Caddy handler.
///
/// The script is prepended to the response body (after any `<!DOCTYPE>`
/// declaration). It runs before any app code, immediately intercepts
/// console methods to capture early logs, then dynamically loads the full
/// client-log collector and/or feedback overlay assets once the DOM is ready.
fn build_bootstrap_script(fb: &FeedbackConfig<'_>) -> String {
    if !fb.inject_client_logs && !fb.inject_feedback_overlay {
        return String::new();
    }

    let mut js = String::from(
        "(function(){\"use strict\";\
         if(window.__veld_cl)return;\
         window.__veld_cl=1;",
    );

    // --- Immediate console interception (before any app code) ---
    if fb.inject_client_logs {
        // Escape levels for safe embedding in JS string.
        let levels = escape_js_string(fb.client_log_levels);
        js.push_str(&format!(
            "var V={levels}.split(','),B=window.__veld_early_logs=[],O={{}};\
             V.forEach(function(n){{\
             var o=console[n];if(typeof o!=='function')return;\
             O[n]=o;\
             console[n]=function(){{\
             B.push({{l:n,a:Array.from(arguments),t:Date.now()}});\
             o.apply(console,arguments);\
             }};}});\
             window.__veld_early_originals=O;\
             window.addEventListener('error',function(e){{\
             try{{B.push({{l:'exception',m:e.message||String(e),\
             s:e.error&&e.error.stack?e.error.stack:'',t:Date.now()}});\
             }}catch(_){{}}\
             }});\
             window.addEventListener('unhandledrejection',function(e){{\
             try{{var r=e.reason;\
             B.push({{l:'exception',m:'Unhandled Promise rejection: '+(r instanceof Error?r.message:String(r||'')),\
             s:r instanceof Error&&r.stack?r.stack:'',t:Date.now()}});\
             }}catch(_){{}}\
             }});",
            levels = levels,
        ));
    }

    // --- Dynamic asset loading ---
    // Assets are loaded after React hydration completes. DOMContentLoaded fires
    // before hydration, so loading then causes React to remove our elements.
    // requestIdleCallback fires when the main thread is idle (after hydration).
    // Fallback to setTimeout(fn, 0) for browsers without requestIdleCallback.
    js.push_str(
        "function E(t,a){var e=document.createElement(t);\
         for(var k in a)e.setAttribute(k,a[k]);\
         (document.head||document.documentElement).appendChild(e);return e;}\
         function R(fn){document.readyState==='loading'?\
         document.addEventListener('DOMContentLoaded',function(){W(fn)}):W(fn);}\
         function W(fn){typeof requestIdleCallback!=='undefined'?\
         requestIdleCallback(fn):setTimeout(fn,0);}R(function(){",
    );

    if fb.inject_client_logs {
        let levels = escape_js_string_bare(fb.client_log_levels);
        js.push_str(&format!(
            "E('script',{{'src':'/__veld__/api/client-log.js','data-veld-levels':'{levels}'}});",
            levels = levels,
        ));
    }

    if fb.inject_feedback_overlay {
        // CSS is bundled into the JS and injected via Shadow DOM — no <link> needed.
        js.push_str("E('script',{'src':'/__veld__/feedback/script.js'});");
    }

    js.push_str("});");

    // Self-remove the bootstrap <script> tag from the DOM before React (or any
    // other hydration-checking framework) walks the live DOM. The browser's
    // HTML5 parser relocates a stray <script> between <!DOCTYPE> and <html>
    // into <head>; Next.js app-router hydrates from the <html> root and would
    // see an extra child not present in the React tree, causing a hydration
    // mismatch. `document.currentScript` is set during synchronous script
    // execution, so removal here runs before hydration. Side effects (console
    // interception, error listeners, and the requestIdleCallback-deferred
    // asset loads) survive removal because they're attached to window/console.
    js.push_str(
        "var s=document.currentScript;if(s&&s.parentNode)s.parentNode.removeChild(s);})();",
    );

    format!("<script>{js}</script>")
}

/// Escape a string for safe embedding inside a JavaScript single-quoted string.
/// Returns the value wrapped in single quotes: `'escaped'`.
fn escape_js_string(s: &str) -> String {
    format!("'{}'", escape_js_string_bare(s))
}

/// Escape a string for safe embedding in JS without adding outer quotes.
/// Use this when the string will be placed inside an already-quoted context.
fn escape_js_string_bare(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\x3c"), // prevent </script> injection
            '>' => out.push_str("\\x3e"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

/// Check if a process with the given PID is still alive.
fn is_process_alive(pid: u32) -> bool {
    let pid = nix::unistd::Pid::from_raw(pid as i32);
    // Signal 0 checks existence without actually sending a signal.
    nix::sys::signal::kill(pid, None).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caddy_proxy_headers_empty_is_none() {
        let empty = veld_core::config::ResolvedProxy::default();
        assert!(caddy_proxy_headers(&empty).is_none());
    }

    #[test]
    fn caddy_proxy_headers_translates_remove_and_set() {
        let mut set = std::collections::BTreeMap::new();
        set.insert("X-Foo".to_string(), "bar".to_string());
        let proxy = veld_core::config::ResolvedProxy {
            request: veld_core::config::HeaderRules {
                remove: vec!["Origin".into()],
                set,
            },
            response: veld_core::config::HeaderRules {
                remove: vec!["Server".into()],
                set: Default::default(),
            },
        };
        let h = caddy_proxy_headers(&proxy).unwrap();
        assert_eq!(h["request"]["delete"][0], "Origin");
        // Caddy `set` values are arrays.
        assert_eq!(h["request"]["set"]["X-Foo"][0], "bar");
        assert_eq!(h["response"]["delete"][0], "Server");
        // No `set` key when there's nothing to set.
        assert!(h["response"]["set"].is_null());
    }

    #[test]
    fn build_route_json_omits_headers_by_default() {
        let route = build_route_json(
            "r",
            "app.test.localhost",
            "localhost:3000",
            None,
            &veld_core::config::ResolvedProxy::default(),
        );
        let proxy = &route["handle"][0]["routes"][0]["handle"][0];
        assert_eq!(proxy["handler"], "reverse_proxy");
        assert!(
            proxy["headers"].is_null(),
            "no headers key when config is empty (Origin passes through)"
        );
    }

    #[test]
    fn build_route_json_applies_proxy_headers() {
        let proxy = veld_core::config::ResolvedProxy {
            request: veld_core::config::HeaderRules {
                remove: vec!["Origin".into()],
                set: Default::default(),
            },
            response: Default::default(),
        };
        let route = build_route_json("r", "app.test.localhost", "localhost:3000", None, &proxy);
        let handler = &route["handle"][0]["routes"][0]["handle"][0];
        assert_eq!(handler["headers"]["request"]["delete"][0], "Origin");
    }

    #[test]
    fn test_filter_route_ids_by_prefix() {
        let routes = serde_json::json!([
            { "@id": "veld-join-abc-app" },
            { "@id": "veld-join-abc-db" },
            { "@id": "veld-demo-frontend-local" },
            { "@id": "veld-management" },
            { "no_id": true },
        ]);
        let mut ids = filter_route_ids_by_prefix(&routes, "veld-join-");
        ids.sort();
        assert_eq!(ids, vec!["veld-join-abc-app", "veld-join-abc-db"]);
        // Non-array / empty inputs are safe no-ops.
        assert!(filter_route_ids_by_prefix(&serde_json::json!({}), "veld-join-").is_empty());
        assert!(filter_route_ids_by_prefix(&serde_json::json!([]), "veld-join-").is_empty());
    }

    // -----------------------------------------------------------------------
    // Route structure tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_route_json() {
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            None,
            &veld_core::config::ResolvedProxy::default(),
        );
        assert_eq!(route["@id"], "test-route");
        assert_eq!(route["match"][0]["host"][0], "app.test.localhost");
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        assert_eq!(subroutes.len(), 1);
        assert_eq!(subroutes[0]["handle"][0]["handler"], "reverse_proxy");
        assert!(
            subroutes[0]["match"].is_null(),
            "catch-all route has no matcher"
        );
    }

    #[test]
    fn test_build_route_json_with_feedback() {
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "my-run",
                project_root: "/tmp/project",
                client_log_levels: "log,warn,error",
                inject: true,
                inject_feedback_overlay: true,
                inject_client_logs: true,
            }),
            &veld_core::config::ResolvedProxy::default(),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        // /__veld__/* + veld_inject catch-all (no bypass routes needed).
        assert_eq!(subroutes.len(), 2);

        // First subroute: feedback API.
        assert_eq!(subroutes[0]["match"][0]["path"][0], "/__veld__/*");
        let fb_proxy = &subroutes[0]["handle"][1];
        assert_eq!(
            fb_proxy["headers"]["request"]["set"]["X-Veld-Run"][0],
            "my-run"
        );
        assert_eq!(
            fb_proxy["headers"]["request"]["set"]["X-Veld-Project"][0],
            "/tmp/project"
        );

        // Second subroute: veld_inject + reverse_proxy.
        let handlers = subroutes[1]["handle"].as_array().unwrap();
        assert_eq!(handlers.len(), 2);
        assert_eq!(handlers[0]["handler"], "veld_inject");
        assert!(handlers[0]["prefix"].as_str().unwrap().contains("<script>"));
        assert_eq!(handlers[1]["handler"], "reverse_proxy");
        // Accept-Encoding: identity is now set by veld_inject handler, not proxy config.
        assert!(handlers[1]["headers"]["request"]["set"]["Accept-Encoding"].is_null());
        assert_eq!(handlers[1]["upstreams"][0]["dial"], "localhost:3000");

        // Verify bootstrap script contains both features.
        let prefix = handlers[0]["prefix"].as_str().unwrap();
        assert!(prefix.contains("client-log.js"), "should load client-log");
        assert!(
            prefix.contains("__veld_early_logs"),
            "should buffer early logs"
        );
        assert!(
            prefix.contains("feedback/script.js"),
            "should load overlay JS (CSS bundled in)"
        );
        assert!(
            prefix.contains("feedback/script.js"),
            "should load overlay JS"
        );
        // font-face is now bundled in Shadow DOM CSS, not in bootstrap
    }

    #[test]
    fn test_build_route_json_feedback_overlay_only() {
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "my-run",
                project_root: "/tmp/project",
                client_log_levels: "log,warn,error",
                inject: true,
                inject_feedback_overlay: true,
                inject_client_logs: false,
            }),
            &veld_core::config::ResolvedProxy::default(),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        assert_eq!(subroutes.len(), 2);
        let prefix = subroutes[1]["handle"][0]["prefix"].as_str().unwrap();
        assert!(
            prefix.contains("feedback/script.js"),
            "should load overlay JS (CSS bundled in)"
        );
        assert!(
            prefix.contains("feedback/script.js"),
            "should load overlay JS"
        );
        assert!(
            !prefix.contains("client-log.js"),
            "should NOT load client-log"
        );
        assert!(
            !prefix.contains("__veld_early_logs"),
            "should NOT intercept console"
        );
    }

    #[test]
    fn test_build_route_json_client_logs_only() {
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "my-run",
                project_root: "/tmp/project",
                client_log_levels: "warn,error",
                inject: true,
                inject_feedback_overlay: false,
                inject_client_logs: true,
            }),
            &veld_core::config::ResolvedProxy::default(),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        assert_eq!(subroutes.len(), 2);
        let prefix = subroutes[1]["handle"][0]["prefix"].as_str().unwrap();
        assert!(prefix.contains("client-log.js"), "should load client-log");
        assert!(
            prefix.contains("__veld_early_logs"),
            "should intercept console"
        );
        assert!(
            !prefix.contains("feedback/script.js"),
            "should NOT load overlay JS"
        );
        assert!(
            !prefix.contains("feedback/script.js"),
            "should NOT load overlay JS"
        );
    }

    #[test]
    fn test_build_route_json_all_features_disabled() {
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "my-run",
                project_root: "/tmp/project",
                client_log_levels: "log,warn,error",
                inject: true,
                inject_feedback_overlay: false,
                inject_client_logs: false,
            }),
            &veld_core::config::ResolvedProxy::default(),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        // /__veld__/* + plain proxy (no veld_inject).
        assert_eq!(subroutes.len(), 2);
        assert_eq!(subroutes[0]["match"][0]["path"][0], "/__veld__/*");
        let handlers = subroutes[1]["handle"].as_array().unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["handler"], "reverse_proxy");
        assert!(
            subroutes[1]["match"].is_null(),
            "catch-all route has no matcher"
        );
    }

    #[test]
    fn test_build_route_json_inject_false_keeps_veld_routes() {
        // inject: false should disable the veld_inject handler but keep
        // the /__veld__/* proxy routes for manual script tag usage.
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "my-run",
                project_root: "/tmp/project",
                client_log_levels: "log,warn,error",
                inject: false,
                inject_feedback_overlay: true,
                inject_client_logs: true,
            }),
            &veld_core::config::ResolvedProxy::default(),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        // /__veld__/* route still present + plain proxy (no veld_inject).
        assert_eq!(subroutes.len(), 2);
        assert_eq!(subroutes[0]["match"][0]["path"][0], "/__veld__/*");
        // Second subroute: plain reverse proxy, no veld_inject.
        let handlers = subroutes[1]["handle"].as_array().unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["handler"], "reverse_proxy");
    }

    /// Verify the veld_inject route is structurally correct: it uses
    /// veld_inject + reverse_proxy (no bypass routes), proxies to the app
    /// upstream, and sets Accept-Encoding: identity.
    #[test]
    fn test_veld_inject_route_structure() {
        let route = build_route_json(
            "inject-test",
            "app.test.localhost",
            "localhost:5555",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "run",
                project_root: "/tmp",
                client_log_levels: "log",
                inject: true,
                inject_feedback_overlay: true,
                inject_client_logs: true,
            }),
            &veld_core::config::ResolvedProxy::default(),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();

        // Only 2 subroutes: /__veld__/* and catch-all. No bypass routes.
        assert_eq!(subroutes.len(), 2);
        assert_eq!(subroutes[0]["match"][0]["path"][0], "/__veld__/*");

        // Catch-all has exactly 2 handlers: veld_inject + reverse_proxy.
        let handlers = subroutes[1]["handle"].as_array().unwrap();
        assert_eq!(handlers.len(), 2);
        assert_eq!(handlers[0]["handler"], "veld_inject");
        assert!(!handlers[0]["prefix"].as_str().unwrap().is_empty());
        assert_eq!(handlers[1]["handler"], "reverse_proxy");
        assert_eq!(handlers[1]["upstreams"][0]["dial"], "localhost:5555");
        // Accept-Encoding: identity is now set by veld_inject handler, not proxy config.
        assert!(handlers[1]["headers"]["request"]["set"]["Accept-Encoding"].is_null());

        // No matcher on the catch-all route.
        assert!(
            subroutes[1]["match"].is_null(),
            "catch-all route has no matcher"
        );
    }

    // -----------------------------------------------------------------------
    // Base config tests
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Route store / full-config tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_full_config_splices_stored_routes() {
        let mut stored = HashMap::new();
        stored.insert(
            "veld-run-b".to_string(),
            build_route_json(
                "veld-run-b",
                "b.dev.localhost",
                "localhost:3001",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );
        stored.insert(
            "veld-run-a".to_string(),
            build_route_json(
                "veld-run-a",
                "a.dev.localhost",
                "localhost:3000",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );

        let config = build_full_config(443, 80, &None, &stored, None);
        let routes = config["apps"]["http"]["servers"]["veld"]["routes"]
            .as_array()
            .unwrap();
        // Base sentinel + management, then the two stored routes sorted by id.
        assert_eq!(routes.len(), 4);
        assert_eq!(routes[0]["@id"], "veld-sentinel");
        assert_eq!(routes[1]["@id"], "veld-management");
        assert_eq!(routes[2]["@id"], "veld-run-a");
        assert_eq!(routes[3]["@id"], "veld-run-b");
    }

    #[test]
    fn test_build_full_config_empty_store_is_base_only() {
        let stored = HashMap::new();
        let config = build_full_config(443, 80, &None, &stored, None);
        let routes = config["apps"]["http"]["servers"]["veld"]["routes"]
            .as_array()
            .unwrap();
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn test_load_route_store_missing_or_corrupt_is_empty() {
        // Missing file → empty.
        let missing = std::env::temp_dir().join("veld-does-not-exist-xyz.json");
        assert!(load_route_store(&missing).is_empty());

        // Corrupt file → empty (self-healing, not fatal).
        let corrupt =
            std::env::temp_dir().join(format!("veld-corrupt-{}.json", std::process::id()));
        std::fs::write(&corrupt, b"{ not json").unwrap();
        assert!(load_route_store(&corrupt).is_empty());
        let _ = std::fs::remove_file(&corrupt);
    }

    #[tokio::test]
    async fn test_route_store_persist_load_roundtrip() {
        let path =
            std::env::temp_dir().join(format!("veld-routes-roundtrip-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut routes = HashMap::new();
        routes.insert(
            "veld-run-a".to_string(),
            build_route_json(
                "veld-run-a",
                "a.dev.localhost",
                "localhost:3000",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );
        let snapshot = RouteSnapshot {
            path: path.clone(),
            routes: routes.clone(),
        };
        persist_route_store(&snapshot).await;

        let loaded = load_route_store(&path);
        assert_eq!(loaded, routes);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // Route-key canonicalisation (#170 compatibility)
    // -----------------------------------------------------------------------

    /// A route as a pre-#170 helper persisted it: id keyed by run name.
    fn legacy_entry(
        run: &str,
        node: &str,
        variant: &str,
        hostname: &str,
    ) -> (String, serde_json::Value) {
        let id = format!("veld-{run}-{node}-{variant}");
        let route = build_route_json(
            &id,
            hostname,
            "localhost:3000",
            None,
            &veld_core::config::ResolvedProxy::default(),
        );
        (id, route)
    }

    #[test]
    fn canonicalize_rekeys_legacy_ids_by_hostname() {
        let mut routes = HashMap::new();
        let (id, route) = legacy_entry("main", "web", "local", "web.main.repo-a.localhost");
        routes.insert(id.clone(), route);

        assert_eq!(canonicalize_route_keys(&mut routes), 1);

        let canonical = veld_core::url::run_route_id("web.main.repo-a.localhost");
        assert!(!routes.contains_key(&id), "legacy key survived");
        // The inner @id moves with the key — `build_full_config` reads the value,
        // not the map key, so a stale @id would serve under the old id.
        assert_eq!(routes[&canonical]["@id"], canonical);
    }

    #[test]
    fn canonicalize_separates_two_projects_that_shared_a_run_name() {
        // The #170 collision as it looks on disk: repo A's route was overwritten
        // by repo B's, so only one entry exists. After canonicalisation the two
        // hostnames can coexist, which is the whole point.
        let mut routes = HashMap::new();
        let (id_a, route_a) = legacy_entry("main", "web", "local", "web.main.repo-a.localhost");
        routes.insert(id_a, route_a);
        canonicalize_route_keys(&mut routes);

        let (id_b, route_b) = legacy_entry("main", "web", "local", "web.main.repo-b.localhost");
        routes.insert(id_b, route_b);
        canonicalize_route_keys(&mut routes);

        assert_eq!(routes.len(), 2);
        assert!(routes.contains_key(&veld_core::url::run_route_id("web.main.repo-a.localhost")));
        assert!(routes.contains_key(&veld_core::url::run_route_id("web.main.repo-b.localhost")));
    }

    #[test]
    fn canonicalize_normalises_a_host_carrying_a_port() {
        // `{service}.localhost:{port}` is a documented urlTemplate shape, so a
        // stored host can carry a port. The re-keyed id must match what the add
        // and teardown paths compute from the same URL, or the entry becomes
        // unremovable.
        let mut routes = HashMap::new();
        let (id, route) = legacy_entry("main", "web", "local", "web.main.app.localhost:9000");
        routes.insert(id, route);

        canonicalize_route_keys(&mut routes);
        assert!(routes.contains_key(&veld_core::url::run_route_id("web.main.app.localhost")));
    }

    #[test]
    fn canonicalize_is_idempotent() {
        let mut routes = HashMap::new();
        let (id, route) = legacy_entry("main", "web", "local", "web.main.app.localhost");
        routes.insert(id, route);

        assert_eq!(canonicalize_route_keys(&mut routes), 1);
        let after_first = routes.clone();
        // Every helper boot runs this; a second pass must be a no-op or the
        // store would churn (and re-persist) forever.
        assert_eq!(canonicalize_route_keys(&mut routes), 0);
        assert_eq!(routes, after_first);
    }

    #[test]
    fn canonicalize_leaves_reserved_ids_alone() {
        let mut routes = HashMap::new();
        // A join route: the daemon removes this by the id it holds in memory.
        // `join_` + 8 hex is `share::manager::gen_id("join")`'s shape.
        routes.insert(
            "veld-join-join_a1b2c3d4-app".to_string(),
            build_route_json(
                "veld-join-join_a1b2c3d4-app",
                "app.joined.localhost",
                "localhost:3000",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );
        // A dev instance's management route, likewise removed by a recomputed id.
        routes.insert(
            "veld-mgmt-veld-dev.localhost".to_string(),
            build_route_json(
                "veld-mgmt-veld-dev.localhost",
                "veld-dev.localhost",
                "localhost:19898",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );
        let before = routes.clone();

        assert_eq!(canonicalize_route_keys(&mut routes), 0);
        assert_eq!(routes, before);
    }

    #[test]
    fn canonicalize_rekeys_run_names_that_look_reserved() {
        // Run names are nearly unconstrained, so `join-feature` and `mgmt-2`
        // produce legacy ids that a bare prefix test would read as reserved.
        // Skipping them would leave a stale entry that OUT-SORTS the canonical
        // route for the same hostname (`build_full_config` orders by `@id`, and
        // `veld-join-…` < `veld-run-…`), so the dead upstream would win.
        for (run, host) in [
            ("join-feature", "web.join-feature.app.localhost"),
            ("mgmt-2", "web.mgmt-2.app.localhost"),
            ("join", "web.join.app.localhost"),
            ("management", "web.management.app.localhost"),
            ("sentinel", "web.sentinel.app.localhost"),
        ] {
            let mut routes = HashMap::new();
            let (id, route) = legacy_entry(run, "web", "local", host);
            routes.insert(id.clone(), route);

            assert_eq!(
                canonicalize_route_keys(&mut routes),
                1,
                "run `{run}` must be re-keyed, not mistaken for a reserved id"
            );
            assert!(routes.contains_key(&veld_core::url::run_route_id(host)));
        }
    }

    #[test]
    fn canonicalize_leaves_a_mgmt_route_alone_only_when_it_owns_its_host() {
        // `veld-mgmt-{host}` is self-verifying: the suffix IS the route's host.
        let mut routes = HashMap::new();
        routes.insert(
            "veld-mgmt-veld-dev.localhost".to_string(),
            build_route_json(
                "veld-mgmt-veld-dev.localhost",
                "veld-dev.localhost",
                "localhost:19898",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );
        assert_eq!(canonicalize_route_keys(&mut routes), 0);

        // A legacy run route whose id merely starts the same way does not own it.
        let mut routes = HashMap::new();
        let (id, route) = legacy_entry("mgmt", "web", "local", "web.mgmt.app.localhost");
        routes.insert(id, route);
        assert_eq!(canonicalize_route_keys(&mut routes), 1);
    }

    #[test]
    fn canonicalize_collapses_two_entries_claiming_one_hostname() {
        // Two run names, one hostname (a url_template without `{run}`): Caddy
        // matches on host, so this pair never had a defined winner.
        let mut routes = HashMap::new();
        let (id_a, route_a) = legacy_entry("alpha", "web", "local", "web.app.localhost");
        let (id_b, route_b) = legacy_entry("beta", "web", "local", "web.app.localhost");
        routes.insert(id_a, route_a);
        routes.insert(id_b, route_b);

        assert_eq!(canonicalize_route_keys(&mut routes), 2);
        assert_eq!(routes.len(), 1);
        assert!(routes.contains_key(&veld_core::url::run_route_id("web.app.localhost")));
    }

    #[test]
    fn canonicalize_keeps_an_entry_it_cannot_read_a_hostname_from() {
        // Don't discard what we don't understand — an unrecognised shape keeps
        // serving under its existing id rather than vanishing.
        let mut routes = HashMap::new();
        routes.insert(
            "veld-weird".to_string(),
            serde_json::json!({"@id": "veld-weird"}),
        );
        let before = routes.clone();

        assert_eq!(canonicalize_route_keys(&mut routes), 0);
        assert_eq!(routes, before);
    }

    #[test]
    fn write_route_store_blocking_is_readable_by_load() {
        let path =
            std::env::temp_dir().join(format!("veld-routes-rekey-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut routes = HashMap::new();
        let (id, route) = legacy_entry("main", "web", "local", "web.main.app.localhost");
        routes.insert(id, route);
        canonicalize_route_keys(&mut routes);
        write_route_store_blocking(&path, &routes);

        assert_eq!(load_route_store(&path), routes);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_routes_store_path_uses_caddy_data_sibling() {
        let bin = std::path::PathBuf::from("/opt/veld/lib/caddy");
        let path = routes_store_path(&Some(bin));
        assert_eq!(
            path,
            std::path::PathBuf::from("/opt/veld/lib/caddy-data/veld-routes.json")
        );
    }

    #[test]
    fn test_caddy_pid_path_uses_caddy_data_sibling() {
        let bin = std::path::PathBuf::from("/opt/veld/lib/caddy");
        let path = caddy_pid_path(&Some(bin));
        assert_eq!(
            path,
            std::path::PathBuf::from("/opt/veld/lib/caddy-data/caddy.pid")
        );
    }

    #[tokio::test]
    async fn test_pid_file_roundtrip() {
        // Use a unique caddy-bin so the pid path is isolated per test run.
        let dir = std::env::temp_dir().join(format!("veld-pidtest-{}", std::process::id()));
        let fake_caddy = dir.join("caddy");
        let over = Some(fake_caddy);

        write_caddy_pid(&over, 4242);
        assert_eq!(read_caddy_pid(&over), Some(4242));
        clear_caddy_pid(&over);
        assert_eq!(read_caddy_pid(&over), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_base_config() {
        let config = build_base_config(443, 80, &None, None);
        assert!(config["apps"]["http"]["servers"]["veld"].is_object());
        let listen = config["apps"]["http"]["servers"]["veld"]["listen"]
            .as_array()
            .unwrap();
        assert_eq!(listen[0], ":443");
        assert_eq!(listen[1], ":80");
        let routes = config["apps"]["http"]["servers"]["veld"]["routes"]
            .as_array()
            .unwrap();
        assert_eq!(routes.len(), 2);
        // The sentinel outranks the management route deliberately: both are
        // terminal, so whichever comes first wins for `MANAGEMENT_HOST`, and a
        // probe of "is Caddy serving?" must not be answered by the daemon.
        assert_eq!(routes[0]["@id"], "veld-sentinel");
        assert_eq!(routes[1]["@id"], "veld-management");
        assert_eq!(routes[1]["match"][0]["host"][0], MANAGEMENT_HOST);
    }

    /// Certificate lifetimes are veld's, not Caddy's defaults — a 12-hour leaf
    /// (Caddy's own default, which veld used to inherit by saying nothing) makes
    /// one missed renewal a broken browser the same day. The intermediate must
    /// outlive the leaf, because Caddy silently clamps a leaf to its issuer's
    /// expiry.
    #[test]
    fn base_config_asks_for_week_long_leaves_under_a_longer_intermediate() {
        let config = build_base_config(443, 80, &None, None);
        let issuer = &config["apps"]["tls"]["automation"]["policies"][0]["issuers"][0];
        assert_eq!(issuer["module"], "internal");
        assert_eq!(issuer["lifetime"], "168h");
        let ca = &config["apps"]["pki"]["certificate_authorities"]["local"];
        assert_eq!(ca["intermediate_lifetime"], "720h");
        assert!(
            parse_hours(LEAF_LIFETIME) < parse_hours(INTERMEDIATE_LIFETIME),
            "a leaf longer than its intermediate is silently shortened by Caddy"
        );
    }

    /// The lifetime above is only honoured for a hostname the policy *names*;
    /// Caddy overrides the issuer for any unnamed internal name, silently, and
    /// the certificate comes back with its 12-hour default. So the management
    /// host is in the base config's subjects, and every stored route's hostname
    /// joins it in the full config.
    #[test]
    fn every_served_hostname_is_named_in_the_certificate_policy() {
        let base = build_base_config(443, 80, &None, None);
        assert_eq!(
            base["apps"]["tls"]["automation"]["policies"][0]["subjects"],
            serde_json::json!([MANAGEMENT_HOST])
        );

        let mut stored = HashMap::new();
        for (id, host) in [
            ("veld-run-a", "a.dev.localhost"),
            ("veld-run-b", "b.dev.localhost"),
        ] {
            stored.insert(
                id.to_string(),
                build_route_json(
                    id,
                    host,
                    "localhost:3000",
                    None,
                    &veld_core::config::ResolvedProxy::default(),
                ),
            );
        }
        let full = build_full_config(443, 80, &None, &stored, None);
        assert_eq!(
            full["apps"]["tls"]["automation"]["policies"][0]["subjects"],
            serde_json::json!(["a.dev.localhost", "b.dev.localhost", MANAGEMENT_HOST])
        );
    }

    /// A hostname a project templates as `veld.localhost` would otherwise appear
    /// twice — once from the base config, once from its own route — and Caddy
    /// rejects a config that names a host in more than one policy, *including*
    /// twice in one policy (`caddytls/tls.go`'s `Validate`, one `hostSet` map).
    /// The route is persisted before the reload, so an un-deduplicated list would
    /// make every later `/load` fail: Caddy left with no config, no URL on the
    /// machine, and the liveness watchdog respawning it forever. Measured against
    /// Caddy 2.11.4 — a duplicated subject fails `caddy validate` outright.
    #[test]
    fn a_route_claiming_the_management_hostname_does_not_duplicate_a_subject() {
        let mut stored = HashMap::new();
        stored.insert(
            "veld-run-collides".to_string(),
            build_route_json(
                "veld-run-collides",
                MANAGEMENT_HOST,
                "localhost:3000",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );
        // ...and the same hostname twice over from two stored routes, which is
        // just as fatal and just as reachable (two projects, one URL template).
        stored.insert(
            "veld-run-twin-a".to_string(),
            build_route_json(
                "veld-run-twin-a",
                "twin.dev.localhost",
                "localhost:3001",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );
        stored.insert(
            "veld-run-twin-b".to_string(),
            build_route_json(
                "veld-run-twin-b",
                "twin.dev.localhost",
                "localhost:3002",
                None,
                &veld_core::config::ResolvedProxy::default(),
            ),
        );

        let config = build_full_config(443, 80, &None, &stored, None);
        let subjects: Vec<&str> = config["apps"]["tls"]["automation"]["policies"][0]["subjects"]
            .as_array()
            .expect("subjects is an array")
            .iter()
            .map(|s| s.as_str().expect("subjects are strings"))
            .collect();
        assert_eq!(subjects, vec!["twin.dev.localhost", MANAGEMENT_HOST]);
    }

    /// The second, subject-less policy is what keeps a hostname that *would*
    /// qualify for a public certificate on the local CA. Drop it and Caddy hands
    /// such a name its default ACME issuer, i.e. veld asks Let's Encrypt for a
    /// certificate for someone's local development URL.
    #[test]
    fn a_catch_all_policy_keeps_public_looking_hostnames_off_acme() {
        let config = build_base_config(443, 80, &None, None);
        let policies = config["apps"]["tls"]["automation"]["policies"]
            .as_array()
            .expect("policies is an array");
        let catch_all = policies
            .iter()
            .find(|p| p.get("subjects").is_none())
            .expect("a policy with no subjects must exist");
        assert_eq!(catch_all["issuers"][0]["module"], "internal");
    }

    /// Caddy's stdout and stderr go to /dev/null, so its own log file is the
    /// only place a renewal failure is ever recorded. Exercises the real sequence
    /// `reload` performs — prepare the file, then name it — under a temporary
    /// caddy-bin override, so it never touches the developer's installed tree.
    #[test]
    fn a_prepared_log_is_named_in_the_config_as_a_rolling_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let over = Some(dir.path().join("caddy"));
        let log = caddy_log_path(&over);
        assert!(prepare_caddy_log(&log), "a fresh path must be preparable");

        let config = build_base_config(443, 80, &over, Some(&log));
        let writer = &config["logging"]["logs"]["default"]["writer"];
        assert_eq!(writer["output"], "file");
        assert_eq!(
            writer["filename"].as_str().expect("a filename"),
            log.to_string_lossy()
        );
        assert!(writer["roll_size_mb"].as_u64().unwrap() > 0);
        // `mode` is deliberately absent: Caddy chmods an existing file when it is
        // set, and chmod follows symlinks. The helper creates the file itself.
        assert!(writer.get("mode").is_none());

        // The directory is pinned independently of `caddy_log_path`, so moving the
        // log back beside the other service logs — where a user-owned directory
        // makes it a root-append primitive — fails here rather than in the field.
        // Asserting `== caddy_log_path(..)` alone would pass for any path that
        // function returned, including the wrong one.
        assert_eq!(
            log.parent().expect("a parent"),
            caddy_data_dir(&over),
            "the log belongs in caddy's own data directory"
        );
        assert_eq!(log.file_name().expect("a filename"), "caddy.log");

        assert!(log.is_file(), "the helper must create the log it names");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&log).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o644, "log must be readable by its user");
        }
    }

    /// Naming a log Caddy cannot open fails the **entire** config — every route
    /// with it. A log is never worth a user's URLs, so the block is omitted.
    #[test]
    fn an_unusable_log_path_costs_the_log_and_not_the_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let over = Some(dir.path().join("caddy"));
        // A directory where the log file belongs: openable as a file, never.
        let log = caddy_log_path(&over);
        std::fs::create_dir_all(&log).expect("plant a directory");

        assert!(!prepare_caddy_log(&log));
        let config = build_base_config(443, 80, &over, None);
        assert!(
            config.get("logging").is_none(),
            "an unopenable log must not be named in the config"
        );
        // The rest of the config is intact — this is the whole point.
        assert!(config["apps"]["http"]["servers"]["veld"]["routes"].is_array());
    }

    /// In privileged mode Caddy is root and this log's directory is not reliably
    /// root-owned (nothing chowns it, so an unprivileged-first install leaves it
    /// to the user). A symlink planted at the log path would have root append
    /// wherever it points, so the helper refuses it rather than following it.
    /// The open itself refuses a symlink, which is the defence that survives a
    /// link planted *after* the caller's `symlink_metadata` check — the window the
    /// caller cannot close. Tested directly because that check would otherwise
    /// account for every assertion, and deleting `O_NOFOLLOW` would pass.
    #[cfg(unix)]
    #[test]
    fn the_log_open_refuses_a_symlink_even_with_no_prior_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("somebody-elses-file");
        std::fs::write(&target, b"untouched").expect("write target");
        let link = dir.path().join("caddy.log");
        std::os::unix::fs::symlink(&target, &link).expect("plant symlink");

        // Erroring at all is what pins the flag: without `O_NOFOLLOW` this open
        // *succeeds*, through the link. The errno is checked loosely on purpose —
        // POSIX says `ELOOP` and both CI platforms give it, but the BSDs answer
        // `EMLINK`, and a test that fails on a correct refusal is worse than one
        // that accepts two spellings of it.
        let err = open_log_for_append(&link).expect_err("must not follow the link");
        assert!(
            matches!(
                err.raw_os_error(),
                Some(nix::libc::ELOOP) | Some(nix::libc::EMLINK)
            ),
            "expected a refusal-to-follow errno, got {err}"
        );
        assert_eq!(
            std::fs::read(&target).expect("read target"),
            b"untouched",
            "the target must be untouched"
        );

        // ...and a plain path still opens, so the flag has not broken the normal case.
        let plain = dir.path().join("plain.log");
        assert!(open_log_for_append(&plain).is_ok());
        assert!(plain.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_log_path_that_is_already_a_symlink_is_refused_before_opening() {
        let dir = tempfile::tempdir().expect("tempdir");
        let over = Some(dir.path().join("caddy"));
        let log = caddy_log_path(&over);
        let target = dir.path().join("somebody-elses-file");
        std::fs::write(&target, b"untouched").expect("write target");
        // Tightened first, because 0644 is what `fs::write` produces anyway:
        // asserting the target is *not* 0644 could not tell a chmod that followed
        // the link from a file that was born that way. 0600 is a mode only the
        // helper's own `set_permissions` would move.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("tighten target");
        std::fs::create_dir_all(log.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(&target, &log).expect("plant symlink");

        assert!(!prepare_caddy_log(&log), "a symlink must not be prepared");
        assert_eq!(
            std::fs::read(&target).expect("read target"),
            b"untouched",
            "the symlink target must not be written through"
        );
        let mode = std::fs::metadata(&target)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "chmod must not have followed the link either"
        );
    }

    /// A route's hostname only gets veld's certificate lifetime if the config
    /// that adds the route also *names* it in the certificate policy, which is why
    /// `add_route` reloads the whole config instead of POSTing the one route. That
    /// is invisible in a diff and expensive to rediscover: POST the route alone and
    /// Caddy issues that hostname a 12-hour certificate under a policy of its own,
    /// cached until it expires — a bug that surfaces days later, in a browser, as
    /// the very failure this change exists to fix. There is no test that can call
    /// `add_route` without a live Caddy, so this pins the shape instead. Same idiom
    /// as `veld-daemon`'s `only_one_function_runs_git_worktree_remove`.
    #[test]
    fn adding_a_route_reloads_rather_than_posting_one_route() {
        let src = include_str!("caddy.rs");
        let body = src
            .split_once("pub async fn add_route(")
            .expect("add_route exists")
            .1
            .split_once("\n    /// Remove a route")
            .expect("add_route is followed by remove_route")
            .0;
        assert!(
            body.contains("self.reload().await"),
            "add_route must reload the whole config: the config that adds a hostname \
             has to be the one that names it in the certificate policy"
        );
        assert!(
            !body.contains("/config/apps/http/servers/veld/routes"),
            "posting the single route leaves the new hostname unnamed in the \
             certificate policy, so Caddy issues it a 12-hour certificate"
        );
    }

    /// Every path that mutates Caddy's live configuration takes the same ordering
    /// lock — otherwise a reload carrying an older snapshot of the store can
    /// resurrect a route a `DELETE` has just removed, leaving a stopped run's
    /// hostname proxying to a dead upstream with nothing to say so. Reachable
    /// whenever one run stops while another starts.
    #[test]
    fn every_live_config_mutation_takes_the_ordering_lock() {
        let src = include_str!("caddy.rs");
        for name in [
            "pub async fn reload(&self) -> Result<()> {",
            "pub async fn remove_route(&self, route_id: &str) -> Result<()> {",
            "pub async fn remove_routes_by_prefix(&self, prefix: &str) -> Result<usize> {",
        ] {
            let body = src.split_once(name).expect(name).1;
            let head: String = body.chars().take(600).collect();
            assert!(
                head.contains("self.reload_lock.lock()"),
                "{name} mutates caddy's live config and must take the ordering lock"
            );
        }
        // `add_route` reaches Caddy only through `reload`, so it inherits the lock
        // rather than taking it twice — tokio's mutex is not reentrant, and taking
        // it here as well would deadlock on the first route added.
        let add = src
            .split_once("pub async fn add_route(")
            .expect("add_route exists")
            .1
            .split_once("\n    /// Remove a route")
            .expect("add_route is followed by remove_route")
            .0;
        assert!(!add.contains("self.reload_lock.lock()"));

        // And nothing inside a critical section may call a *lock-taking* sibling.
        // Stating the rule in a comment is what this test was doing when the rule
        // was broken one function below it: `remove_routes_by_prefix` held the
        // lock and called `remove_route`, which takes it, deadlocking the helper
        // on an ordinary daemon startup. So the check is now on the code.
        let by_prefix = src
            .split_once("pub async fn remove_routes_by_prefix(")
            .expect("remove_routes_by_prefix exists")
            .1
            .split_once("\n    /// Check whether caddy is running")
            .expect("remove_routes_by_prefix is followed by is_running")
            .0;
        assert!(
            !by_prefix.contains("self.remove_route(&"),
            "a holder of the ordering lock must call the `_locked` form: \
             tokio's mutex is not reentrant, and this deadlocks the helper"
        );
        assert!(by_prefix.contains("self.remove_route_locked(&"));
    }

    /// `/load` replaces Caddy's entire configuration, so two reloads racing can
    /// apply out of order and the older snapshot wins — dropping a route Caddy
    /// should be serving with no error anywhere. Two runs starting at once is the
    /// ordinary way to reach that. The ordering lock cannot be exercised without a
    /// live admin API, so this pins its presence: the lock must be taken *before*
    /// the store is snapshotted, or the window is still open.
    #[test]
    fn reloading_is_serialised_before_the_store_is_snapshotted() {
        let src = include_str!("caddy.rs");
        let body = src
            .split_once("pub async fn reload(&self) -> Result<()> {")
            .expect("reload exists")
            .1
            .split_once("\n    /// Add a reverse-proxy route")
            .expect("reload is followed by add_route")
            .0;
        let lock_at = body
            .find("self.reload_lock.lock()")
            .expect("reload must serialise itself");
        let snapshot_at = body
            .find("self.routes.lock()")
            .expect("reload must snapshot the store");
        assert!(
            lock_at < snapshot_at,
            "the ordering lock must be held before the snapshot is taken, or a \
             stale config can still overwrite a newer one"
        );
    }

    /// Building a config must not touch the filesystem: it used to prepare the
    /// log itself, which meant every test that built one wrote into whatever
    /// `VELD_LIB_DIR` pointed at — the developer's real install — and which branch
    /// the test took depended on that machine's directory permissions.
    #[test]
    fn building_a_config_creates_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let over = Some(dir.path().join("caddy"));
        let _ = build_full_config(443, 80, &over, &HashMap::new(), None);
        assert!(
            !caddy_log_path(&over).exists(),
            "the builder must not create a log"
        );
    }

    // -----------------------------------------------------------------------
    // Certificate watchdog policy
    // -----------------------------------------------------------------------

    fn expired() -> veld_core::tls_health::TlsHealth {
        veld_core::tls_health::TlsHealth::Expired {
            expired_for: Duration::from_secs(3600),
        }
    }

    fn healthy() -> veld_core::tls_health::TlsHealth {
        veld_core::tls_health::TlsHealth::Valid {
            expires_in: Duration::from_secs(6 * 24 * 3600),
            lifetime: Duration::from_secs(7 * 24 * 3600),
        }
    }

    #[test]
    fn one_overdue_probe_does_not_restart_caddy() {
        let mut gate = CertGate::default();
        let now = std::time::Instant::now();
        assert_eq!(
            gate.weigh(&expired(), false, now),
            CertVerdict::Overdue { strikes: 1 }
        );
    }

    #[test]
    fn two_overdue_probes_in_a_row_restart_caddy() {
        let mut gate = CertGate::default();
        let now = std::time::Instant::now();
        gate.weigh(&expired(), false, now);
        assert_eq!(
            gate.weigh(&expired(), false, now + Duration::from_secs(60)),
            CertVerdict::Restart
        );
    }

    #[test]
    fn a_healthy_probe_in_between_clears_the_strike() {
        let mut gate = CertGate::default();
        let now = std::time::Instant::now();
        gate.weigh(&expired(), false, now);
        assert_eq!(
            gate.weigh(&healthy(), true, now + Duration::from_secs(60)),
            CertVerdict::Recovered
        );
        // Back to one strike, not two: the pair has to be consecutive.
        assert_eq!(
            gate.weigh(&expired(), false, now + Duration::from_secs(120)),
            CertVerdict::Overdue { strikes: 1 }
        );
    }

    /// The restarted Caddy renews in the background, so it keeps serving the old
    /// certificate for a moment — a window in which nothing must restart it again.
    #[test]
    fn the_cooldown_blocks_a_second_restart() {
        let mut gate = CertGate::default();
        let start = std::time::Instant::now();
        gate.weigh(&expired(), false, start);
        assert_eq!(
            gate.weigh(&expired(), false, start + Duration::from_secs(60)),
            CertVerdict::Restart
        );
        // Two more bad probes inside the cooldown: enough strikes, too soon.
        gate.weigh(&expired(), false, start + Duration::from_secs(120));
        assert_eq!(
            gate.weigh(&expired(), false, start + Duration::from_secs(180)),
            CertVerdict::Overdue { strikes: 2 }
        );
        // Past it, the remedy is available again.
        let past_cooldown = start + CERT_RESTART_COOLDOWN + Duration::from_secs(1);
        gate.weigh(&expired(), false, past_cooldown);
        assert_eq!(
            gate.weigh(&expired(), false, past_cooldown + Duration::from_secs(60)),
            CertVerdict::Restart
        );
    }

    /// Restarting is a remedy only while it remedies something. A fault a new
    /// process cannot fix — an unwritable storage tree, a CA with no key — would
    /// otherwise have root killing Caddy every cooldown for as long as the
    /// machine is up, each time dropping every live connection on it.
    #[test]
    fn veld_stops_restarting_once_restarts_stop_helping() {
        let mut gate = CertGate::default();
        let mut now = std::time::Instant::now();
        let mut restarts = 0;
        // Far more rounds than the cap, all of them overdue and none recovering.
        for _ in 0..40 {
            if gate.weigh(&expired(), false, now) == CertVerdict::Restart {
                restarts += 1;
            }
            now += Duration::from_secs(5 * 60);
        }
        assert_eq!(restarts, CERT_RESTART_ATTEMPTS);
        assert_eq!(gate.weigh(&expired(), false, now), CertVerdict::GaveUp);
    }

    /// ...and a certificate that does come back healthy re-arms it, so a machine
    /// that hits the cap once is not left unprotected for the rest of its uptime.
    #[test]
    fn recovery_re_arms_the_remedy_after_it_gave_up() {
        let mut gate = CertGate::default();
        let mut now = std::time::Instant::now();
        for _ in 0..40 {
            gate.weigh(&expired(), false, now);
            now += Duration::from_secs(5 * 60);
        }
        assert_eq!(gate.weigh(&expired(), false, now), CertVerdict::GaveUp);

        assert_eq!(gate.weigh(&healthy(), true, now), CertVerdict::Recovered);
        gate.weigh(&expired(), false, now + Duration::from_secs(60));
        assert_eq!(
            gate.weigh(&expired(), false, now + Duration::from_secs(120)),
            CertVerdict::Restart
        );
    }

    /// The give-up state has to stay escapable. Recovery is judged over the whole
    /// probe set, not from the worst verdict, because the worst verdict is
    /// `Unreachable` for as long as *any* one hostname cannot be issued — so a
    /// gate that read recovery from it would sit in `GaveUp` for the rest of the
    /// helper's uptime, through a later expiry it should have acted on. That is
    /// the shape this asserts: still-unreachable somewhere, healthy overall.
    #[test]
    fn giving_up_is_escapable_even_while_one_hostname_stays_unreachable() {
        let mut gate = CertGate::default();
        let mut now = std::time::Instant::now();
        for _ in 0..40 {
            gate.weigh(&expired(), false, now);
            now += Duration::from_secs(5 * 60);
        }
        assert_eq!(gate.weigh(&expired(), false, now), CertVerdict::GaveUp);

        // The certificates are fine again, but one hostname still answers
        // nothing, so `worst()` is still `Unreachable` — recovery must not depend
        // on that.
        let unreachable = veld_core::tls_health::TlsHealth::Unreachable {
            detail: "no certificate for this name".to_owned(),
        };
        assert_eq!(
            gate.weigh(&unreachable, true, now),
            CertVerdict::Recovered,
            "a set that is healthy overall must clear the give-up state"
        );
        // ...and the remedy is armed again for the next real fault.
        gate.weigh(&expired(), false, now + Duration::from_secs(60));
        assert_eq!(
            gate.weigh(&expired(), false, now + Duration::from_secs(120)),
            CertVerdict::Restart
        );
    }

    /// A probe that could not reach Caddy says nothing about its certificate.
    /// Restarting for it would hand the liveness watchdog's job to the one
    /// watchdog that cannot tell whether Caddy is even meant to be up.
    #[test]
    fn an_unreachable_probe_never_restarts_caddy() {
        let mut gate = CertGate::default();
        let now = std::time::Instant::now();
        let unreachable = veld_core::tls_health::TlsHealth::Unreachable {
            detail: "connection refused".to_owned(),
        };
        for i in 0..5 {
            assert_eq!(
                gate.weigh(&unreachable, false, now + Duration::from_secs(60 * i)),
                CertVerdict::Fine
            );
        }
    }

    /// ...and it must not *clear* what real probes established either. A probe
    /// taken while a just-restarted Caddy is still coming up reads `Unreachable`,
    /// so counting that as recovery would return the give-up cap to zero on every
    /// single restart — a root-driven restart every cooldown, forever, which is
    /// the failure the cap was added to prevent. Two angles of the review found
    /// this in the fix that added the cap; a gate that has already restarted is
    /// the only state that shows it, which is why this test starts from one.
    #[test]
    fn a_probe_that_learned_nothing_does_not_re_arm_the_give_up_cap() {
        let mut gate = CertGate::default();
        let mut now = std::time::Instant::now();
        let unreachable = veld_core::tls_health::TlsHealth::Unreachable {
            detail: "connection refused".to_owned(),
        };

        let mut restarts = 0;
        for _ in 0..40 {
            // The shape a restart actually produces: bad, bad, restart, then a
            // probe against a Caddy that has not finished starting.
            if gate.weigh(&expired(), false, now) == CertVerdict::Restart {
                restarts += 1;
            }
            now += Duration::from_secs(60);
            assert_eq!(gate.weigh(&unreachable, false, now), CertVerdict::Fine);
            now += Duration::from_secs(5 * 60);
        }
        assert_eq!(
            restarts, CERT_RESTART_ATTEMPTS,
            "an unreachable probe between restarts must not reset the cap"
        );

        // And `NotYetValid` — a clock behind the certificate — is the same kind
        // of nothing: not a renewal fault, not a recovery.
        let mut gate = CertGate::default();
        let now = std::time::Instant::now();
        gate.weigh(&expired(), false, now);
        assert_eq!(
            gate.weigh(
                &veld_core::tls_health::TlsHealth::NotYetValid {
                    valid_in: Duration::from_secs(3600)
                },
                false,
                now + Duration::from_secs(60)
            ),
            CertVerdict::Fine,
            "a clock fault is not the certificate becoming healthy"
        );
    }

    fn parse_hours(lifetime: &str) -> u64 {
        lifetime
            .strip_suffix('h')
            .expect("lifetimes are written in hours")
            .parse()
            .expect("lifetimes are a whole number of hours")
    }

    #[test]
    fn test_build_base_config_custom_ports() {
        let config = build_base_config(18443, 18080, &None, None);
        let listen = config["apps"]["http"]["servers"]["veld"]["listen"]
            .as_array()
            .unwrap();
        assert_eq!(listen[0], ":18443");
        assert_eq!(listen[1], ":18080");
    }

    // -----------------------------------------------------------------------
    // Bootstrap script tests
    // -----------------------------------------------------------------------

    fn make_fb<'a>(overlay: bool, logs: bool, levels: &'a str) -> FeedbackConfig<'a> {
        FeedbackConfig {
            upstream: "localhost:19899",
            run_name: "run",
            project_root: "/tmp",
            client_log_levels: levels,
            inject: true,
            inject_feedback_overlay: overlay,
            inject_client_logs: logs,
        }
    }

    #[test]
    fn test_bootstrap_script_both_features() {
        let script = build_bootstrap_script(&make_fb(true, true, "log,warn,error"));
        assert!(script.starts_with("<script>"));
        assert!(script.ends_with("</script>"));
        // Console interception.
        assert!(script.contains("__veld_early_logs"));
        assert!(script.contains("__veld_cl"));
        // Dynamic asset loading.
        assert!(script.contains("client-log.js"));
        assert!(script.contains("feedback/script.js"));
        // CSS is now bundled inside the JS (Shadow DOM), no separate style.css
        assert!(!script.contains("style.css"));
    }

    #[test]
    fn test_bootstrap_script_overlay_only() {
        let script = build_bootstrap_script(&make_fb(true, false, "log,warn,error"));
        assert!(script.contains("feedback/script.js"));
        assert!(!script.contains("client-log.js"));
        assert!(!script.contains("__veld_early_logs"));
    }

    #[test]
    fn test_bootstrap_script_logs_only() {
        let script = build_bootstrap_script(&make_fb(false, true, "warn,error"));
        assert!(script.contains("client-log.js"));
        assert!(script.contains("__veld_early_logs"));
        assert!(!script.contains("feedback/script.js"));
    }

    #[test]
    fn test_bootstrap_script_neither_feature() {
        let script = build_bootstrap_script(&make_fb(false, false, "log"));
        assert!(script.is_empty());
    }

    #[test]
    fn test_bootstrap_script_custom_levels() {
        let script = build_bootstrap_script(&make_fb(false, true, "debug,info"));
        assert!(script.contains("debug,info"));
    }

    #[test]
    fn test_bootstrap_script_escaping() {
        // Levels with special chars should be escaped safely.
        let script = build_bootstrap_script(&make_fb(false, true, "log'</script>"));
        assert!(!script.contains("'</script>'"));
        assert!(script.contains("\\x3c/script\\x3e"));
        assert!(script.contains("\\'"));
    }

    #[test]
    fn test_escape_js_string() {
        assert_eq!(escape_js_string("hello"), "'hello'");
        assert_eq!(escape_js_string("it's"), "'it\\'s'");
        assert_eq!(escape_js_string("a\\b"), "'a\\\\b'");
        assert_eq!(escape_js_string("<script>"), "'\\x3cscript\\x3e'");
        assert_eq!(escape_js_string("a\nb"), "'a\\nb'");
        assert_eq!(escape_js_string("a\rb"), "'a\\rb'");
    }

    #[test]
    fn test_bootstrap_script_is_valid_html() {
        let script = build_bootstrap_script(&make_fb(true, true, "log"));
        // Must be a single script tag.
        assert_eq!(
            script.matches("<script>").count(),
            1,
            "should have exactly one opening script tag"
        );
        assert_eq!(
            script.matches("</script>").count(),
            1,
            "should have exactly one closing script tag"
        );
    }

    #[test]
    fn test_bootstrap_script_dedup_guard() {
        let script = build_bootstrap_script(&make_fb(true, true, "log"));
        // Guard prevents double execution.
        assert!(script.contains("if(window.__veld_cl)return"));
        assert!(script.contains("window.__veld_cl=1"));
    }

    /// The bootstrap script must remove its own <script> tag from the DOM
    /// after running, to prevent React hydration mismatches in Next.js
    /// app-router and similar frameworks that hydrate from the <html> root.
    #[test]
    fn test_bootstrap_script_self_removes() {
        let configs = [
            make_fb(true, true, "log,warn,error"), // both
            make_fb(true, false, "log"),           // overlay only
            make_fb(false, true, "log"),           // logs only
        ];
        for fb in &configs {
            let script = build_bootstrap_script(fb);
            assert!(
                script.contains("document.currentScript"),
                "script must reference document.currentScript for self-removal"
            );
            assert!(
                script.contains("removeChild"),
                "script must call removeChild to detach itself from the DOM"
            );
        }
    }

    #[test]
    fn test_bootstrap_script_error_handlers() {
        let script = build_bootstrap_script(&make_fb(false, true, "log"));
        // Should capture unhandled errors and promise rejections.
        assert!(script.contains("addEventListener('error'"));
        assert!(script.contains("addEventListener('unhandledrejection'"));
    }

    /// Regression: the bootstrap script must not have duplicate variable/function
    /// names. Previously `L` was used for both the levels array and the DOM
    /// element helper, causing `Uncaught SyntaxError: Unexpected identifier`.
    #[test]
    fn test_bootstrap_script_no_duplicate_identifiers() {
        // Test all feature combinations that produce a non-empty script.
        let configs = [
            make_fb(true, true, "log,warn,error"),
            make_fb(true, false, "log"),
            make_fb(false, true, "log"),
        ];
        for fb in &configs {
            let script = build_bootstrap_script(fb);
            let js = script
                .strip_prefix("<script>")
                .unwrap()
                .strip_suffix("</script>")
                .unwrap();

            let mut decls: std::collections::HashMap<char, usize> =
                std::collections::HashMap::new();
            for pattern in ["var ", "function "] {
                let mut search_from = 0;
                while let Some(pos) = js[search_from..].find(pattern) {
                    let abs = search_from + pos + pattern.len();
                    if let Some(ch) = js[abs..].chars().next() {
                        if ch.is_ascii_uppercase() {
                            *decls.entry(ch).or_default() += 1;
                        }
                    }
                    search_from = abs + 1;
                }
            }
            for (name, count) in &decls {
                assert_eq!(
                    *count, 1,
                    "identifier '{name}' declared {count} times (overlay={}, logs={})",
                    fb.inject_feedback_overlay, fb.inject_client_logs
                );
            }
        }
    }

    /// Regression: the data-veld-levels attribute value must not have nested
    /// quotes. Previously `escape_js_string` wrapped the value in quotes,
    /// then it was placed inside an already-quoted JS object literal property,
    /// producing `'data-veld-levels':''log,warn,error''`.
    #[test]
    fn test_bootstrap_script_no_nested_quotes_in_attributes() {
        let script = build_bootstrap_script(&make_fb(true, true, "log,warn,error"));
        // The attribute value should be 'log,warn,error' not ''log,warn,error''
        assert!(
            !script.contains("':''"),
            "attribute value has nested quotes — would produce invalid JS"
        );
        assert!(
            script.contains("'data-veld-levels':'log,warn,error'"),
            "attribute value should be properly single-quoted"
        );
    }

    /// Verify that the bootstrap script's IIFE is properly closed for all
    /// feature combinations — unbalanced parens would cause a SyntaxError.
    #[test]
    fn test_bootstrap_script_balanced_structure() {
        let configs = [
            make_fb(true, true, "log,warn,error"),
            make_fb(true, false, "log"),
            make_fb(false, true, "warn"),
        ];
        for fb in &configs {
            let script = build_bootstrap_script(fb);
            let js = script
                .strip_prefix("<script>")
                .unwrap()
                .strip_suffix("</script>")
                .unwrap();

            // Count parens, braces, brackets — they must balance.
            let mut parens = 0i32;
            let mut braces = 0i32;
            let mut brackets = 0i32;
            let mut in_string = false;
            let mut escape_next = false;
            let mut quote_char = ' ';

            for ch in js.chars() {
                if escape_next {
                    escape_next = false;
                    continue;
                }
                if ch == '\\' && in_string {
                    escape_next = true;
                    continue;
                }
                if in_string {
                    if ch == quote_char {
                        in_string = false;
                    }
                    continue;
                }
                match ch {
                    '\'' | '"' => {
                        in_string = true;
                        quote_char = ch;
                    }
                    '(' => parens += 1,
                    ')' => parens -= 1,
                    '{' => braces += 1,
                    '}' => braces -= 1,
                    '[' => brackets += 1,
                    ']' => brackets -= 1,
                    _ => {}
                }
            }

            assert_eq!(
                parens, 0,
                "unbalanced parentheses (overlay={}, logs={})",
                fb.inject_feedback_overlay, fb.inject_client_logs
            );
            assert_eq!(
                braces, 0,
                "unbalanced braces (overlay={}, logs={})",
                fb.inject_feedback_overlay, fb.inject_client_logs
            );
            assert_eq!(
                brackets, 0,
                "unbalanced brackets (overlay={}, logs={})",
                fb.inject_feedback_overlay, fb.inject_client_logs
            );
        }
    }
}
