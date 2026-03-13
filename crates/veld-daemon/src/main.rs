mod broadcaster;
mod feedback_server;
mod gc;
mod monitor;

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
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut socket_path: Option<PathBuf> = None;

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
                println!("  --version, -V         Print version and exit");
                println!("  --help, -h            Print help and exit");
                std::process::exit(0);
            }
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

    let socket_path = socket_path.unwrap_or_else(|| {
        let home = dirs::home_dir().expect("could not determine home directory");
        home.join(".veld").join("daemon.sock")
    });

    Args { socket_path }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = parse_args();

    info!("veld-daemon {VERSION} starting");
    info!("socket path: {}", args.socket_path.display());

    // Ensure the parent directory exists.
    if let Some(parent) = args.socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("failed to create socket parent directory")?;
    }

    // Remove stale socket file if present.
    if args.socket_path.exists() {
        tokio::fs::remove_file(&args.socket_path)
            .await
            .context("failed to remove stale socket")?;
    }

    // Bind the Unix socket listener.
    let listener = UnixListener::bind(&args.socket_path).context("failed to bind Unix socket")?;

    info!("listening on {}", args.socket_path.display());

    // Shared broadcaster for connected CLI clients.
    let broadcaster = broadcaster::Broadcaster::new();

    // Spawn background tasks.
    let monitor_broadcaster = broadcaster.clone();
    let monitor_handle = tokio::spawn(async move {
        monitor::run_health_monitor(monitor_broadcaster).await;
    });

    let gc_handle = tokio::spawn(async move {
        gc::run_gc_scheduler().await;
    });

    let feedback_handle = tokio::spawn(async move {
        feedback_server::run_feedback_server().await;
    });

    let accept_broadcaster = broadcaster.clone();
    let accept_handle = tokio::spawn(async move {
        accept_connections(listener, accept_broadcaster).await;
    });

    // Wait for shutdown signal.
    shutdown_signal().await;
    info!("shutdown signal received, cleaning up");

    // Abort background tasks.
    monitor_handle.abort();
    gc_handle.abort();
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
