mod caddy;
mod dns;
mod handler;
mod protocol;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{error, info, warn};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often the watchdog checks that Caddy is alive and serving.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(15);

/// How often to check whether the helper's own binary changed on disk.
const BINARY_WATCH_INTERVAL: Duration = Duration::from_secs(10);

struct HelperConfig {
    socket_path: PathBuf,
    https_port: u16,
    http_port: u16,
    /// Override the Caddy binary path (avoids lib_dir() resolution issues under sudo).
    caddy_bin: Option<PathBuf>,
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
            other => anyhow::bail!("unknown argument: {other}"),
        }
        i += 1;
    }

    Ok(HelperConfig {
        socket_path,
        https_port,
        http_port,
        caddy_bin,
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

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let state = Arc::new(handler::State::new(
        config.https_port,
        config.http_port,
        config.caddy_bin,
        shutdown_tx,
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
    if is_system_socket(&config.socket_path) {
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
                // Binding the system socket does not prove launchd/systemd is
                // behind us: a helper spawned directly (e.g. by setup's
                // fallback path) also binds it, and if that one exits nothing
                // relaunches it — every URL goes dark until the next
                // `veld setup`. Only exit when the service manager reports
                // *this* pid as the managed instance. Keep polling on failure:
                // the query can fail transiently, and the binary still differs
                // from baseline, so a later tick gets another chance to exit.
                if service_manager_owns_us().await {
                    info!(
                        "helper binary changed on disk — exiting so launchd relaunches the new version"
                    );
                    std::process::exit(0);
                }
                if ticks_since_warn == 0 {
                    warn!(
                        "helper binary changed on disk, but this helper is not managed by a \
                         service manager — staying alive on the old binary. Run `veld setup` \
                         to restart onto the new version."
                    );
                }
                ticks_since_warn = (ticks_since_warn + 1) % REWARN_TICKS;
            }
        }
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

        let response = state.handle_request(&line).await;
        let mut response_json = serde_json::to_string(&response)
            .unwrap_or_else(|e| format!(r#"{{"ok":false,"error":"serialization error: {e}"}}"#));
        response_json.push('\n');
        writer.write_all(response_json.as_bytes()).await?;
    }

    Ok(())
}
