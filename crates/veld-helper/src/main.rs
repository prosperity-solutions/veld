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

fn default_socket_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/var/run/veld-helper.sock")
    } else {
        PathBuf::from("/run/veld-helper.sock")
    }
}

fn parse_args() -> Result<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    let mut socket_path = default_socket_path();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                println!("veld-helper {VERSION}");
                std::process::exit(0);
            }
            "--socket-path" => {
                i += 1;
                let path = args
                    .get(i)
                    .context("--socket-path requires a value")?;
                socket_path = PathBuf::from(path);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
        i += 1;
    }

    Ok(socket_path)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let socket_path = parse_args()?;

    // Remove stale socket if it exists.
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)
            .with_context(|| format!("failed to remove stale socket at {}", socket_path.display()))?;
    }

    // Ensure the parent directory exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind socket at {}", socket_path.display()))?;

    info!("veld-helper {VERSION} listening on {}", socket_path.display());

    let state = Arc::new(handler::State::new());

    loop {
        match listener.accept().await {
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
            .unwrap_or_else(|e| {
                format!(r#"{{"ok":false,"error":"serialization error: {e}"}}"#)
            });
        response_json.push('\n');
        writer.write_all(response_json.as_bytes()).await?;
    }

    Ok(())
}
