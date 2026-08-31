mod caddy;
mod dns;
mod handler;
mod protocol;
mod signing;
mod sleep;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{debug, error, info, warn};
use veld_core::helper_gate::{Gate, GateSource};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often the watchdog checks that Caddy is alive and serving.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(15);

/// How often the watchdog checks that the certificate Caddy serves is one a
/// browser accepts.
///
/// Slower than [`WATCHDOG_INTERVAL`] because it costs a TLS handshake per
/// hostname and because it is watching something that changes on the scale of
/// days — but fast enough that a certificate which does expire is served broken
/// for about a minute, not for the day and a half it took a user to report it.
/// The handshakes run concurrently, so the tick is bounded by the slowest single
/// probe (`tls_health`'s own timeout) rather than by their sum; a serial loop over
/// twenty routes against a wedged Caddy would overrun this interval three times
/// over.
const CERT_WATCHDOG_INTERVAL: Duration = Duration::from_secs(60);

/// How often to check whether the helper's own binary changed on disk.
const BINARY_WATCH_INTERVAL: Duration = Duration::from_secs(10);

/// How long to wait for a candidate binary to answer `--version` before treating
/// it as unrunnable.
///
/// Generous enough that a first exec slowed by Gatekeeper is not mistaken for a
/// broken binary, and bounded by the other end: `restart` runs the whole of
/// [`restart_blocker`] inline, so the caller's round-trip is a service query
/// (`SERVICE_QUERY_TIMEOUT`, 5s) *plus* this, and `veld-core`'s `SEND_TIMEOUT`
/// gives it 15s in total including connect, write and read. 6s leaves that budget
/// intact; a larger value would surface a slow check to the caller as a dead
/// helper rather than as the refusal it is.
const BINARY_EXEC_CHECK_TIMEOUT: Duration = Duration::from_secs(6);

struct HelperConfig {
    socket_path: PathBuf,
    https_port: u16,
    http_port: u16,
    /// Override the Caddy binary path (avoids lib_dir() resolution issues under sudo).
    caddy_bin: Option<PathBuf>,
    /// `--allow-uid` as written into the service definition by
    /// `veld setup privileged`: the one uid (besides root) allowed to drive this
    /// helper over its socket. An *override*, not the whole story — a privileged
    /// helper with no flag derives the uid instead, so that installs predating
    /// the flag are gated without anyone re-running setup. See [`Gate::resolve`].
    allow_uid: Option<u32>,
}

fn default_socket_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/var/run/veld-helper.sock")
    } else {
        PathBuf::from("/run/veld-helper.sock")
    }
}

fn parse_args() -> Result<HelperConfig> {
    let args: Vec<String> = std::env::args().collect();
    let mut socket_path = default_socket_path();
    let mut https_port: u16 = 443;
    let mut http_port: u16 = 80;
    let mut caddy_bin: Option<PathBuf> = None;
    let mut allow_uid: Option<u32> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                println!("veld-helper {VERSION}");
                std::process::exit(0);
            }
            "--socket-path" => {
                i += 1;
                let path = args.get(i).context("--socket-path requires a value")?;
                socket_path = PathBuf::from(path);
            }
            "--https-port" => {
                i += 1;
                let val = args.get(i).context("--https-port requires a value")?;
                https_port = val
                    .parse()
                    .context("--https-port must be a valid port number")?;
            }
            "--http-port" => {
                i += 1;
                let val = args.get(i).context("--http-port requires a value")?;
                http_port = val
                    .parse()
                    .context("--http-port must be a valid port number")?;
            }
            "--caddy-bin" => {
                i += 1;
                let path = args.get(i).context("--caddy-bin requires a value")?;
                caddy_bin = Some(PathBuf::from(path));
            }
            "--allow-uid" => {
                i += 1;
                let value = args.get(i).context("--allow-uid requires a value")?;
                allow_uid = Some(value.parse().context("--allow-uid must be a numeric uid")?);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
        i += 1;
    }

    Ok(HelperConfig {
        socket_path,
        https_port,
        http_port,
        caddy_bin,
        allow_uid,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = parse_args()?;

    // Remove stale socket if it exists.
    if config.socket_path.exists() {
        std::fs::remove_file(&config.socket_path).with_context(|| {
            format!(
                "failed to remove stale socket at {}",
                config.socket_path.display()
            )
        })?;
    }

    // Ensure the parent directory exists.
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let listener = UnixListener::bind(&config.socket_path)
        .with_context(|| format!("failed to bind socket at {}", config.socket_path.display()))?;

    // Set socket permissions based on location.
    // System daemon sockets (/var/run, /run) need 0o777 so the unprivileged
    // CLI can connect. User sockets only need owner access (0o700).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let socket_str = config.socket_path.to_string_lossy();
        let mode = if socket_str.starts_with("/var/run") || socket_str.starts_with("/run") {
            0o777
        } else {
            0o700
        };
        std::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(mode))
            .with_context(|| {
                format!(
                    "failed to set socket permissions on {}",
                    config.socket_path.display()
                )
            })?;
    }

    info!(
        "veld-helper {VERSION} listening on {}",
        config.socket_path.display()
    );

    // The peer-uid gate. Resolved once, here, rather than read straight off
    // `config.allow_uid` in the accept loop: an existing privileged install has
    // no `--allow-uid` in its service definition (only `veld setup privileged`
    // writes one) and must still end up gated. See [`Gate::resolve`].
    let privileged = is_system_socket(&config.socket_path);
    // `current_exe()` failing and the install directory being unreadable both
    // land on `UnreadableLibDir`, whose user-facing text names the directory —
    // so log the io error here or it is gone from the one place a support
    // transcript would look for it.
    let exe = std::env::current_exe()
        .inspect_err(|e| warn!(error = %e, "could not read own executable path; the peer-uid gate cannot be derived"))
        .ok();
    let gate = Gate::resolve(config.allow_uid, privileged, exe.as_deref());
    log_gate(&gate);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let state = Arc::new(handler::State::new(
        config.https_port,
        config.http_port,
        config.caddy_bin,
        shutdown_tx,
        // The privileged system daemon (root) is the only one the swap-relaunch
        // signing gate protects; an unprivileged user helper relaunches as the
        // user and needs no signature to shut down.
        privileged,
        gate,
    ));

    // Startup reconcile: if a Caddy is already running (orphaned across our own
    // self-restart / helper crash), re-adopt it, reload the current config, and
    // start supervising it. Runs before the watchdog so an updated binary/config
    // takes effect immediately rather than on the next `veld start`.
    {
        let startup_state = Arc::clone(&state);
        tokio::spawn(async move {
            startup_state.reconcile_caddy_on_startup().await;
        });
    }

    // Keep-awake, both halves — privileged helper only. `pmset` refuses a
    // non-root caller, so the unprivileged LaunchAgent and the ephemeral
    // auto-bootstrap helper can never hold a lease; running these there would
    // cost an exec at every start and, worse, warn on every exit about a setting
    // that helper never touched — in exactly the log a support transcript reads.
    // Same predicate the binary-watcher below uses.
    {
        // Reconcile first, and awaited rather than spawned: it must finish before
        // the accept loop can take a fresh lease, or a daemon renewing across our
        // restart could race the adoption. With no ownership marker on disk this
        // reads nothing and does nothing. It *adopts* rather than reverts — a
        // daemon that is still there renews inside the grace, which is what makes
        // a helper crash (or the self-restart path below, which exits straight out
        // of a spawned task and never runs the release at the tail) invisible to
        // the user instead of a dropped hold.
        state.reconcile_sleep_on_startup().await;

        // Watchdog: hand the sleep setting back once its lease lapses. Its own
        // task rather than a second call in the Caddy tick below, because a Caddy
        // recovery can take seconds and this deadline must not slip behind
        // something unrelated.
        let sleep_state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(WATCHDOG_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                sleep_state.sleep_watchdog_tick().await;
            }
        });
    }

    // Caddy watchdog: keep Caddy alive and every persisted route served across
    // crashes, macOS sleep/wake, and reboots. launchd's KeepAlive only restarts
    // the *helper* on exit — it cannot detect a dead/wedged child Caddy, so we
    // supervise Caddy ourselves.
    let watchdog_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(WATCHDOG_INTERVAL);
        // Skip missed ticks instead of firing a burst after a long sleep.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            watchdog_state.caddy_watchdog_tick().await;
        }
    });

    // Certificate watchdog: a Caddy that answers the tick above can still be
    // serving a certificate no browser accepts, because its certificate
    // maintenance is a separate goroutine that can stop on its own. Its own task
    // rather than a slower branch of the loop above: this one does a TLS
    // handshake and may restart Caddy, and liveness must not queue behind it.
    let cert_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(CERT_WATCHDOG_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            cert_state.caddy_cert_watchdog_tick().await;
        }
    });

    // Self-restart when our own binary is replaced on disk (by `veld update`),
    // so launchd relaunches the new version as root — no sudo, no manual
    // `veld setup privileged`. Complements the plist's WatchPaths, which does
    // not reliably bounce an already-running KeepAlive daemon.
    //
    // Only for the privileged system-domain LaunchDaemon: that's the one whose
    // restart needs root (the exact gap this closes). The unprivileged
    // LaunchAgent is already restarted by the installer via user-domain
    // launchctl (no sudo), and the auto-bootstrapped helper is ephemeral and
    // has nothing to relaunch it — so exiting there would just drop URLs.
    if privileged {
        tokio::spawn(watch_own_binary());
    }

    // Graceful shutdown on SIGTERM/Ctrl-C (e.g. `launchctl bootout`). Caddy is
    // intentionally left running so URLs stay up while launchd relaunches us.
    let mut term = signal_stream();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        // Peer-credential gate, applied to the *privileged* system
                        // daemon only. The socket is world-writable (0o777) so the
                        // unprivileged CLI can connect, which means *any* local process
                        // could otherwise drive a root helper — including `shutdown`,
                        // which stops Caddy and drops every live URL. The kernel
                        // attests the connecting process's uid at connect time, so this
                        // cannot be spoofed by holding the socket open. Only the
                        // installing user (and root, which privileged setup connects as
                        // to verify the socket) is allowed; `gate` decides which uid
                        // that is, from the service definition or from the filesystem.
                        if let Some(allowed) = gate.uid() {
                            match peer_uid(&stream) {
                                Some(uid) if peer_allowed(uid, allowed) => {}
                                Some(uid) => {
                                    warn!(
                                        peer_uid = uid,
                                        allowed_uid = allowed,
                                        "rejecting connection from untrusted uid on privileged helper socket"
                                    );
                                    reject_connection(stream).await;
                                    continue;
                                }
                                None => {
                                    warn!("could not read peer uid; rejecting connection on privileged helper socket");
                                    reject_connection(stream).await;
                                    continue;
                                }
                            }
                        }
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, state).await {
                                error!("connection handler error: {e:#}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("failed to accept connection: {e}");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("shutdown signal received, exiting");
                    break;
                }
            }
            _ = term.recv() => {
                info!("received termination signal, exiting (leaving caddy running)");
                break;
            }
        }
    }

    // Caddy is left running on purpose; a sleep setting veld took is not. Caddy
    // serving URLs across a helper restart is the desired behaviour, whereas a
    // durable `disablesleep` with nothing left watching its lease is the exact
    // failure this mechanism is built to avoid — including on the exit path that
    // has no relaunch after it, which is what `veld uninstall` produces. A
    // setting veld did not take is left alone; that is `release`'s own rule.
    state.release_sleep_on_exit().await;

    Ok(())
}

/// Future that resolves when the process receives SIGTERM or Ctrl-C.
struct SignalStream {
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
}

fn signal_stream() -> SignalStream {
    #[cfg(unix)]
    {
        let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        SignalStream { sigterm }
    }
    #[cfg(not(unix))]
    {
        SignalStream {}
    }
}

impl SignalStream {
    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.sigterm.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// The effective uid of the process that opened `stream`. On macOS this is
/// `getpeereid`; on Linux, `SO_PEERCRED`. The kernel fills these from the
/// connecting process's identity at connect time, so a peer cannot fake them
/// by arranging to hold the socket open. `None` when the platform has no such
/// primitive or the call fails — the caller must treat that as a rejection,
/// never as "allow".
fn peer_uid(stream: &tokio::net::UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    peer_uid_fd(stream.as_raw_fd())
}

/// Whether a peer with `peer` uid may drive a helper whose `--allow-uid` is
/// `allowed`. Root (uid 0) is always permitted: privileged setup connects as
/// root to verify the socket, and a process that is already root needs no
/// further privilege from this helper.
fn peer_allowed(peer: u32, allowed: u32) -> bool {
    peer == 0 || peer == allowed
}

/// The effective uid of the process that opened the socket on `fd`. On macOS
/// this is `getpeereid`; on Linux, `SO_PEERCRED`. The kernel fills these from
/// the connecting process's identity at connect time, so a peer cannot fake
/// them by arranging to hold the socket open. `None` when the platform has no
/// such primitive or the call fails — the caller must treat that as a
/// rejection, never as "allow".
fn peer_uid_fd(fd: i32) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // Safe: getpeereid writes into the two out-params and returns 0 on
        // success; the raw fd is owned by the caller, which outlives this call.
        if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == 0 {
            Some(uid)
        } else {
            None
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // Safe: getsockopt fills `cred` (a properly sized out-param) and the
        // raw fd is owned by the caller, which outlives this call.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc == 0 { Some(cred.uid) } else { None }
    }
    // The helper only targets macOS and Linux; on any other unix there is no
    // kernel-attested peer identity we can read, so report "unknown" and let
    // the caller reject rather than open the door.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = fd;
        None
    }
}

/// Best-effort refusal on a rejected connection, then drop it. The peer is not
/// trusted, so this is only for diagnosis (e.g. a legitimately misconfigured
/// setup whose uid no longer matches the plist) — it must never be relied on by
/// the client, which treats a dropped connection as a failure.
async fn reject_connection(stream: tokio::net::UnixStream) {
    use tokio::io::AsyncWriteExt;
    let mut stream = stream;
    let _ = stream
        .write_all(
            format!(
                "{}\n",
                serde_json::json!({
                    "ok": false,
                    "error": veld_core::helper_gate::REJECTED_PEER_ERROR,
                })
            )
            .as_bytes(),
        )
        .await;
}

/// Poll the helper's own executable; when its size/mtime changes and settles,
/// exit(0) so launchd's KeepAlive relaunches the freshly installed binary.
async fn watch_own_binary() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "could not resolve own executable path; binary self-restart disabled");
            return;
        }
    };
    let baseline = binary_signature(&exe);
    let mut interval = tokio::time::interval(BINARY_WATCH_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Consume the immediate first tick so we don't compare against ourselves at t=0.
    interval.tick().await;
    // Re-warn periodically (not once, not every 10s tick): an operator who
    // starts tailing the log later must still see the unmanaged-stale state.
    const REWARN_TICKS: u32 = 60; // ~10 minutes at BINARY_WATCH_INTERVAL
    let mut ticks_since_warn: u32 = 0;
    loop {
        interval.tick().await;
        let current = binary_signature(&exe);
        if current.is_some() && current != baseline {
            // Debounce: `veld update` does cp + chmod + xattr + codesign — several
            // writes. Wait for the signature to settle before relaunching so we
            // don't exit mid-swap.
            tokio::time::sleep(Duration::from_secs(2)).await;
            if binary_signature(&exe) == current {
                // Keep polling when something blocks the exit: the checks can
                // fail transiently (a bounded service query timing out, a write
                // still in flight), and the binary still differs from baseline,
                // so a later tick gets another chance to exit.
                // Spawned only for the system socket (see `main`), so this
                // watcher is the privileged helper by construction.
                match restart_blocker(true).await {
                    // Re-stat *after* the gate, not only before it. The gate
                    // takes real time (a service query plus an exec), and the
                    // write sequence this debounce exists for is cp + chmod +
                    // xattr + codesign — so a 2s lull before `codesign` can let
                    // a valid-but-unsigned file pass the exec check and be
                    // rewritten underneath us. Requiring the signature to be
                    // unchanged across the whole gate closes that window: if it
                    // moved, this tick's evidence is stale and the next one
                    // starts over.
                    None if binary_signature(&exe) == current => {
                        info!(
                            "helper binary changed on disk — exiting so launchd relaunches the new version"
                        );
                        std::process::exit(0);
                    }
                    None => {
                        debug!("binary changed again while checking it — re-checking next tick");
                    }
                    Some(reason) => {
                        if ticks_since_warn == 0 {
                            warn!(
                                reason,
                                "helper binary changed on disk, but restarting onto it is unsafe \
                                 — staying alive on the old binary. Run `veld setup` if this \
                                 persists."
                            );
                        }
                        ticks_since_warn = (ticks_since_warn + 1) % REWARN_TICKS;
                    }
                }
            }
        }
    }
}

/// Why exiting for a relaunch would be unsafe right now, or `None` when it is
/// safe. Shared by the binary watcher and the `restart` command so both stop for
/// the same reasons — the `restart` caller gets the string back and can fall
/// back instead of guessing why nothing happened.
///
/// Every check here is "refuse unless proven safe": a query that fails or times
/// out blocks the exit, because staying on an old binary is recoverable and
/// exiting into a hole is not.
///
/// `privileged` selects whether the org signing gate applies. It is the whole
/// point for the system helper, which relaunches as root; an unprivileged
/// helper relaunches as its own user, so requiring a signature there would buy
/// no privilege boundary and would refuse `veld update` for anyone running a
/// locally built (unsigned) helper.
pub(crate) async fn restart_blocker(privileged: bool) -> Option<String> {
    if !service_manager_owns_us().await {
        return Some(
            "this helper is not managed by a service manager, so nothing would relaunch it".into(),
        );
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Some(format!("could not resolve own executable path: {e}")),
    };
    if !binary_executes(&exe).await {
        return Some(format!(
            "the binary at {} does not execute yet",
            exe.display()
        ));
    }
    // The on-disk binary must carry a valid org signature (the fail-closed
    // signing gate from #261): relaunching onto a swapped, unsigned binary is
    // the #247 escalation. Shared by the watcher and the `restart` command so
    // neither can exit onto a binary the other refuses.
    if privileged {
        if let Some(reason) = signing::relaunch_guard(&exe) {
            return Some(reason);
        }
    }
    None
}

/// Whether `path` runs — checked by executing it with `--version`, which prints
/// and exits without binding the socket or touching Caddy.
///
/// This is the guard the size/mtime debounce could not provide. `veld update`
/// writes the binary with cp + chmod + xattr + codesign, and the signature can
/// go quiet *between* those steps; a watcher that trusted it exited onto a file
/// launchd then failed to exec, leaving it to crash-loop against `KeepAlive`
/// with no helper running at all (observed in the field: one such episode
/// produced 2432 consecutive `cannot execute binary file` lines). Asking the
/// kernel to actually exec the thing is the only check that cannot be fooled by
/// a plausible-looking stat.
async fn binary_executes(path: &Path) -> bool {
    let run = tokio::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match tokio::time::timeout(BINARY_EXEC_CHECK_TIMEOUT, run).await {
        Ok(Ok(status)) => status.success(),
        // Spawn failed (ENOEXEC on a half-written file is exactly this) or the
        // check hung — either way, not proven good.
        Ok(Err(_)) | Err(_) => false,
    }
}

/// Whether the SYSTEM-DOMAIN service manager reports *this process* as the
/// running instance of the veld-helper service. Distinguishes a
/// launchd/systemd-managed helper (safe to exit — it gets relaunched) from a
/// directly-spawned orphan that merely bound the same socket (exiting would
/// leave nothing behind). Queries are bounded inside veld-core
/// ([`veld_core::setup::SERVICE_QUERY_TIMEOUT`]) and degrade to "not owned" —
/// the safe direction. NOTE: system domain only (`system/…`, root systemd);
/// do not copy this for user-domain agents like veld-daemon.
async fn service_manager_owns_us() -> bool {
    let own_pid = std::process::id();
    if cfg!(target_os = "macos") {
        veld_core::setup::launchd_job_pid("system", veld_core::setup::HELPER_LABEL_MACOS).await
            == Some(own_pid)
    } else {
        veld_core::setup::systemd_main_pid(veld_core::setup::HELPER_SERVICE_LINUX).await
            == Some(own_pid)
    }
}

/// Whether this helper is listening on the privileged system-domain socket
/// (`/var/run` on macOS, `/run` on Linux), i.e. it is the root LaunchDaemon /
/// systemd service rather than an unprivileged/auto helper.
fn is_system_socket(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/var/run") || s.starts_with("/run")
}

/// Say what the peer-uid gate resolved to, once, at startup.
///
/// An ungated *privileged* helper warns rather than informs: it is the state
/// #337 exists to remove, and the helper's own log is where a support
/// transcript looks first. See [`veld_core::helper_gate::Gate::resolve`].
fn log_gate(gate: &Gate) {
    match gate.uid() {
        Some(uid) => info!(
            allow_uid = uid,
            source = gate.source().as_str(),
            "privileged helper socket gated to a single uid"
        ),
        None if gate.source() == GateSource::Unprivileged => {}
        None => warn!(
            source = gate.source().as_str(),
            "privileged helper socket is NOT uid-gated — any local process can drive it; \
             run `veld setup privileged` to write the gate explicitly"
        ),
    }
}

/// A cheap change signature for a file: (size, mtime-seconds).
fn binary_signature(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    state: Arc<handler::State>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let handled = state.handle_request(&line).await;
        let mut response_json = serde_json::to_string(&handled.response)
            .unwrap_or_else(|e| format!(r#"{{"ok":false,"error":"serialization error: {e}"}}"#));
        response_json.push('\n');
        let written = writer.write_all(response_json.as_bytes()).await;

        if handled.exit_after_reply {
            // The decision was made before this write and does not depend on it.
            // Propagating a write error here with `?` would skip `signal_exit`
            // entirely — and the caller most likely to drop the socket is one
            // whose own send timeout expired while the safety gate ran, so the
            // helper would have passed every check, decided to restart, and then
            // silently not. The reply is best-effort; the exit is not.
            //
            // Flush before signalling, never after: the signal ends the accept
            // loop and the process, and anything still buffered here would die
            // with it. Once these bytes are in the kernel's socket buffer the
            // peer reads them whether or not we are still alive.
            let _ = writer.flush().await;
            state.signal_exit();
            return Ok(());
        }
        written?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::peer_allowed;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    /// The peer-credential gate's policy, as pure logic.
    #[test]
    fn gate_allows_only_the_installing_user_and_root() {
        const INSTALLING: u32 = 501;
        // The installing user is allowed.
        assert!(peer_allowed(INSTALLING, INSTALLING));
        // Root is always allowed — privileged setup connects as root to verify
        // the socket, and a root process needs nothing this helper could give it.
        assert!(peer_allowed(0, INSTALLING));
        // Any other uid is rejected.
        assert!(!peer_allowed(502, INSTALLING));
        // A root helper whose --allow-uid is root-only still allows root.
        assert!(peer_allowed(0, 0));
    }

    /// `is_system_socket` is the switch deciding whether a root helper gates its
    /// socket *at all*, so the two real socket paths must land on the right side
    /// of it — including on Linux, where the user socket must not be mistaken
    /// for a system one.
    #[test]
    fn only_the_system_domain_socket_reads_as_privileged() {
        use super::is_system_socket;
        use std::path::Path;

        // The paths `veld-core` actually hands out.
        assert!(is_system_socket(&veld_core::helper::system_socket_path()));
        assert!(!is_system_socket(&veld_core::helper::user_socket_path()));

        // Both platforms' system paths read as privileged wherever the suite
        // runs, because the plist/unit pins an absolute path, not a cfg.
        assert!(is_system_socket(Path::new("/var/run/veld-helper.sock")));
        assert!(is_system_socket(Path::new("/run/veld-helper.sock")));

        // The user helper's own path, and the `/tmp` fallback `user_socket_path`
        // degrades to when there is no home directory, are NOT privileged —
        // they rely on the 0o700 owner-only socket instead.
        assert!(!is_system_socket(Path::new("/Users/x/.veld/helper.sock")));
        assert!(!is_system_socket(Path::new("/home/x/.veld/helper.sock")));
        assert!(!is_system_socket(Path::new("/tmp/veld-helper.sock")));
    }

    /// The kernel-attested peer uid of a same-process peer is this process's
    /// own effective uid — proving the primitive reads a real identity rather
    /// than a constant.
    #[test]
    fn peer_uid_reads_the_connecting_process_identity() {
        let (a, b) = UnixStream::pair().unwrap();
        let me = unsafe { libc::geteuid() };
        // Each end's peer is this same process.
        assert_eq!(super::peer_uid_fd(a.as_raw_fd()), Some(me));
        assert_eq!(super::peer_uid_fd(b.as_raw_fd()), Some(me));
    }
}
