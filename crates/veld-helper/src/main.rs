mod caddy;
mod dns;
mod handler;
mod protocol;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{error, info};

const VERSION: &str = env!("CARGO_PKG_VERSION");

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
        }
    }

    Ok(())
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
