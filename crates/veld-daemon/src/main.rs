mod backup;
mod broadcaster;
mod dbhealth;
mod feedback_server;
mod gc;
mod monitor;
mod share;
mod stats;

use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::net::UnixListener;
use tokio::signal;
use tracing::{info, warn};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_SOCKET: &str = "~/.veld/daemon.sock";

// ---------------------------------------------------------------------------
// CLI argument parsing (minimal, no clap dependency needed)
// ---------------------------------------------------------------------------

struct Args {
    socket_path: PathBuf,
    /// True for `--pty-holder`: serve one terminal session and nothing else.
    pty_holder: bool,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut socket_path: Option<PathBuf> = None;
    let mut pty_holder = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("veld-daemon {VERSION}");
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("Usage: veld-daemon [OPTIONS]");
                println!();
                println!("Options:");
                println!("  --socket-path <PATH>  Path to Unix socket (default: {DEFAULT_SOCKET})");
                println!(
                    "  --pty-holder          Serve one terminal session (spawned by the daemon)"
                );
                println!("  --version, -V         Print version and exit");
                println!("  --help, -h            Print help and exit");
                std::process::exit(0);
            }
            // Not a user-facing mode: the daemon spawns itself with this to put
            // a terminal's PTY in a process that outlives the daemon, and hands
            // it its configuration on stdin.
            "--pty-holder" => pty_holder = true,
            "--socket-path" => {
                socket_path = Some(PathBuf::from(
                    args.next().expect("--socket-path requires a value"),
                ));
            }
            other => {
                eprintln!("Unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }

    // Precedence: --socket-path flag, then VELD_DAEMON_SOCK, then the
    // installed default (~/.veld/daemon.sock).
    let socket_path = socket_path.unwrap_or_else(veld_core::instance::daemon_socket);

    Args {
        socket_path,
        pty_holder,
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // Parsed before tracing is initialised, because the mode decides where the
    // logs go — and parsing writes nothing.
    let args = parse_args();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    if args.pty_holder {
        // A holder logs to **stderr**, and the daemon spawns it with stderr
        // inherited, so its lines land in the daemon's log beside the session
        // they belong to. Not stdout: that is nulled on the spawn (a pipe nobody
        // drains would block the holder the first time it filled), so a
        // stdout-bound subscriber would discard every diagnostic this process
        // ever produces — including why it failed to start.
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }

    // A holder is not a daemon: it binds no port and no socket of the daemon's,
    // opens no database, and starts no background task. Dispatched before any of
    // that so nothing below can mistake it for a second instance.
    if args.pty_holder {
        return feedback_server::run_pty_holder().await;
    }

    info!("veld-daemon {VERSION} starting");
    info!("socket path: {}", args.socket_path.display());

    // Ensure the parent directory exists.
    if let Some(parent) = args.socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("failed to create socket parent directory")?;
    }

    // NOTE: no unconditional unlink here — removing the socket file blindly
    // would break a LIVE daemon's socket and make the AddrInUse arm below
    // (the "another instance is already running" abort) unreachable. Stale
    // files are removed only after a connect() probe proves nobody answers.

    // Warned about before binding, because `bind`'s own error for an over-long path is
    // "path must be shorter than SUN_LEN" — true, and it names neither the path nor
    // the way out. It reaches the user as a daemon that will not start. The holder
    // sockets already get this treatment (`veld_core::instance::pty_dir`); the
    // control socket is reachable the same way, since `VELD_DAEMON_SOCK` can point
    // anywhere and a checkout under `~/git/_worktrees/<long-branch-name>/` is over
    // the bound on its own.
    //
    // **A warning, not a refusal**, and the difference is a real one: `MAX_SOCKET_PATH`
    // is 104 because that is the *safe floor* across platforms (macOS `sun_path` is
    // 104, Linux's is 108), so refusing here would stop a Linux daemon that has always
    // bound a 104..=107-byte path perfectly well — a regression handed to someone who
    // would then read nothing, since under systemd this goes to a journal. Let the
    // kernel be the authority on its own limit; this only makes its verdict legible.
    if let Some(len) = veld_core::instance::socket_path_over_limit(&args.socket_path) {
        warn!(
            "the daemon socket path is {len} bytes, at or over the {}-byte floor a unix \
             socket allows on the strictest supported platform ({}). If the bind below \
             fails, set VELD_DAEMON_SOCK to a shorter path — somewhere under $HOME \
             rather than inside a deep checkout.",
            veld_core::instance::MAX_SOCKET_PATH,
            args.socket_path.display()
        );
    }

    // Bind the Unix socket listener. A leftover socket FILE from a crashed
    // daemon must not block startup — but a LIVE socket means another
    // instance of this daemon is already running, which must be a loud,
    // immediate error (two half-alive daemons fighting over one port was a
    // miserable thing to debug).
    let listener = match UnixListener::bind(&args.socket_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            match tokio::net::UnixStream::connect(&args.socket_path).await {
                Ok(_) => anyhow::bail!(
                    "another veld-daemon is already running on {} — stop it first \
                     (two instances cannot share a socket/port)",
                    args.socket_path.display()
                ),
                Err(_) => {
                    info!(
                        "removing stale socket {} (no daemon answering)",
                        args.socket_path.display()
                    );
                    std::fs::remove_file(&args.socket_path)
                        .context("failed to remove stale socket")?;
                    UnixListener::bind(&args.socket_path).context("failed to bind Unix socket")?
                }
            }
        }
        Err(e) => return Err(e).context("failed to bind Unix socket"),
    };

    info!("listening on {}", args.socket_path.display());

    // Shared broadcaster for connected CLI clients.
    let broadcaster = broadcaster::Broadcaster::new();

    // Share manager owns the iroh endpoint and all live shares/joins. Its node
    // key persists so the node identity is stable across restarts. Created early
    // because GC and the startup route purge both need it.
    let share_manager = {
        let key_path =
            share::endpoint::key_path().context("could not determine data dir for node key")?;
        let secret =
            share::endpoint::load_or_create_secret_key(&key_path).context("loading node key")?;
        std::sync::Arc::new(share::manager::ShareManager::new(secret))
    };

    // Purge orphaned `veld-join-*` Caddy routes left by a previous daemon that
    // crashed while a join was active. In-memory join state is empty at boot, so
    // every such route is stale. Best-effort, retried with backoff because on a
    // cold boot the helper/Caddy may not be reachable yet when the daemon starts.
    tokio::spawn(async {
        for attempt in 0..5u64 {
            if let Ok(helper) = veld_core::helper::HelperClient::connect().await {
                if helper.remove_routes_by_prefix("veld-join-").await.is_ok() {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2 * (attempt + 1))).await;
        }
    });

    // Dev-instance dashboard: when VELD_MANAGEMENT_HOST is set (e.g.
    // `veld-dev.localhost`), self-register a Caddy route for it pointing at
    // THIS daemon's HTTP port. The installed instance never sets this — its
    // `veld.localhost` route ships in the helper's base config. Best-effort
    // with backoff (helper may still be booting); `.localhost` names need no
    // DNS entry (RFC 6761).
    if let Some(host) = veld_core::instance::management_host() {
        let upstream = veld_core::instance::daemon_upstream();
        tokio::spawn(async move {
            let route = serde_json::json!({
                "route_id": format!("veld-mgmt-{host}"),
                "hostname": host,
                "upstream": upstream,
            });
            for attempt in 0..5u64 {
                if let Ok(helper) = veld_core::helper::HelperClient::connect().await {
                    if helper.add_route(route.clone()).await.is_ok() {
                        info!("management route registered: https://{host}");
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2 * (attempt + 1))).await;
            }
            warn!("could not register management route for {host} (helper unreachable)");
        });
    }

    // Local files, on their own listener and their own origin — see
    // `feedback_server::files`. Spawned rather than awaited: registering its Caddy
    // route retries with backoff, and nothing else here waits on the helper.
    tokio::spawn(feedback_server::files::start());

    // Collect stderr captures left by a previous daemon that was killed between
    // spawning a `veld` command and reaping it.
    feedback_server::management::sweep_spawn_logs();

    // Spawn background tasks.
    let monitor_broadcaster = broadcaster.clone();
    let monitor_handle = tokio::spawn(async move {
        monitor::run_health_monitor(monitor_broadcaster).await;
    });

    let gc_manager = std::sync::Arc::clone(&share_manager);
    let gc_handle = tokio::spawn(async move {
        gc::run_gc_scheduler(gc_manager).await;
    });

    // Copies of the central database, on the user's interval. Its own task rather
    // than a step inside the GC pass: the GC runs every 10 minutes and this is
    // configurable down to 5, and the two failure modes are unrelated — a backup
    // must keep happening on a machine where route cleanup is failing, and vice
    // versa.
    let backup_handle = tokio::spawn(backup::run_backup_scheduler());

    // Whether the database is still intact, and whether those backups are
    // actually happening. Its own task for the same reason the backup scheduler
    // is: the interval is unrelated (a `quick_check` is cheap enough to run far
    // more often than a backup is worth taking), and a probe that stopped
    // because the *backup* path failed would be a health check that goes quiet
    // exactly when it matters. See `dbhealth`'s module docs for the incident.
    // **Every `Db::open()` failure in this process now reports itself.** The
    // daemon opens the database from more than two dozen places and several of
    // them map the error straight to a status; installing one observer covers all
    // of them, where wiring each call site missed sites in three review rounds
    // running. Per-query failures still report at their own funnels.
    veld_core::db::observe_open_failures(dbhealth::note_error);
    dbhealth::mark_start();
    let dbhealth_handle = tokio::spawn(dbhealth::run_probe());

    // Which shell that resolution — and every terminal — uses. Published from
    // here because `veld_core::user_path` is linked into the gateway and the CLI
    // too, neither of which has a database to read a setting from; a failure to
    // open ours leaves it unset, which means the user's login shell, i.e. the
    // behaviour before the setting existed. Re-published by the settings handler
    // on every patch.
    veld_core::user_path::set_preferred_shell(
        veld_core::db::Db::open().ok().map(|db| db.terminal_shell()),
    );

    // The user's login-shell PATH, kept warm on its own timer for the same
    // reason stats is: resolving it spawns a login shell (up to 10s on a stalled
    // rc file), and the request handlers that need it — stop/restart/action,
    // Desktop's start, share start — must never pay that on a click.
    let user_path_handle = tokio::spawn(veld_core::user_path::warm_user_path_cache());

    // Same reasoning, for the directory-scoped sibling: prime every already-
    // registered project's entry so the FIRST top-bar start/stop/restart/
    // action click after this boot (including every `veld update`, which
    // restarts the daemon) doesn't pay a synchronous login-shell resolution
    // on a UI `fetch` with no timeout. A project registered after this runs
    // still pays that cost once on its own first use — the same one-time
    // shape `cached_user_path`'s first call already has. Resolved
    // concurrently, not sequentially: one project's slow `.zshrc` must not
    // delay every other project's warm-up.
    // `join_all` inside this one task, not a `Vec` of separately spawned
    // ones: aborting `project_path_warm_handle` at shutdown must actually
    // stop every in-flight login shell it started, and dropping a
    // `JoinHandle` detaches rather than cancels — a `Vec<JoinHandle<_>>`
    // would leave every still-running resolution to finish on its own.
    let project_path_warm_handle = tokio::spawn(async move {
        let Ok(db) = veld_core::db::Db::open() else {
            return;
        };
        let Ok(registry) = db.registry() else {
            return;
        };
        let warms = registry.projects.into_values().map(|entry| async move {
            veld_core::user_path::cached_user_path_for(&entry.project_root).await;
        });
        futures_util::future::join_all(warms).await;
    });

    // Which ports the dashboard is actually served on, learned from the helper and
    // kept current, because the `Origin` gate on the terminal and IDE-channel
    // upgrades is synchronous and cannot ask — and the setup mode file it would
    // otherwise infer from can disagree with the helper in front.
    let dashboard_ports_handle = tokio::spawn(feedback_server::track_dashboard_ports());

    // Resource-stats sampling runs on its own timer, deliberately separate from
    // the health monitor: liveness probes there can block for tens of seconds,
    // which would stretch the sampling gap and make live stats read as stale.
    let stats_handle = tokio::spawn(async move {
        stats::run_stats_sampler().await;
    });

    let feedback_manager = std::sync::Arc::clone(&share_manager);
    let feedback_handle = tokio::spawn(async move {
        feedback_server::run_feedback_server(feedback_manager).await;
    });

    let accept_broadcaster = broadcaster.clone();
    let accept_handle = tokio::spawn(async move {
        accept_connections(listener, accept_broadcaster).await;
    });

    // Wait for shutdown signal.
    shutdown_signal().await;
    info!("shutdown signal received, cleaning up");

    // Deregister the dev-instance management route (best-effort — a stale
    // route only 502s until the next dev daemon re-registers it).
    if let Some(host) = veld_core::instance::management_host() {
        if let Ok(helper) = veld_core::helper::HelperClient::connect().await {
            let _ = helper.remove_route(&format!("veld-mgmt-{host}")).await;
        }
    }

    // And the file origin's route, for a stronger reason than the one above. The
    // helper persists routes and replays them after a Caddy restart or a reboot, so
    // one left behind keeps `files.veld.localhost` pointing at a port this process no
    // longer holds. The listener's port is fixed and reserved
    // (`instance::files_port`), which is what makes the leftover benign rather than a
    // hostname another process can inherit — this removes it anyway, because the
    // cheapest time to not need that argument is now.
    // Gated on having actually registered one, like the management route above is
    // gated on having a management host. Ungated, a daemon that never registered —
    // not an addressable instance, or the port was squatted — still connects to the
    // helper and issues a DELETE that Caddy refuses, taking its reload lock and
    // logging an error for a route nobody made.
    if feedback_server::files::is_ready()
        && let Ok(helper) = veld_core::helper::HelperClient::connect().await
    {
        let _ = helper
            .remove_route(&veld_core::instance::files_route_id())
            .await;
    }

    // Terminal shells are deliberately **left running**. Their PTYs belong to
    // holder processes rather than to this one, so a shutdown is invisible to them
    // and the next daemon adopts them — which is what makes `veld update` safe to
    // run with terminals open. This call only records how many were left; what
    // still ends a shell is an explicit `DELETE`, the detach reaper, or the
    // holder's own orphan grace. Ordered before the aborts because it needs the
    // runtime to still be turning.
    feedback_server::shutdown_terminal_sessions().await;

    // Abort background tasks.
    monitor_handle.abort();
    gc_handle.abort();
    backup_handle.abort();
    dbhealth_handle.abort();
    stats_handle.abort();
    dashboard_ports_handle.abort();
    user_path_handle.abort();
    project_path_warm_handle.abort();
    accept_handle.abort();
    feedback_handle.abort();

    // Clean up the socket file.
    let _ = tokio::fs::remove_file(&args.socket_path).await;

    info!("veld-daemon stopped");
    Ok(())
}

// ---------------------------------------------------------------------------
// Connection acceptor
// ---------------------------------------------------------------------------

async fn accept_connections(listener: UnixListener, broadcaster: broadcaster::Broadcaster) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                info!("client connected");
                let bc = broadcaster.clone();
                tokio::spawn(async move {
                    bc.handle_client(stream).await;
                });
            }
            Err(e) => {
                warn!("failed to accept connection: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Graceful shutdown
// ---------------------------------------------------------------------------

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
