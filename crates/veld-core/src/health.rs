use std::path::Path;
use std::time::Duration;

use thiserror::Error;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

use crate::config::HealthCheck;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("health check timed out after {timeout_seconds}s{hint}")]
    Timeout { timeout_seconds: u64, hint: String },

    #[error("port check failed: {0}")]
    PortCheckFailed(String),

    #[error("HTTPS check failed: {0}")]
    HttpsCheckFailed(String),

    #[error("bash health check failed with exit code {0}")]
    BashCheckFailed(i32),
}

// ---------------------------------------------------------------------------
// Phase 1: TCP port check
// ---------------------------------------------------------------------------

/// Repeatedly try to connect to `port` on localhost until success or timeout.
///
/// Tries both IPv4 (127.0.0.1) and IPv6 (::1) on each attempt since modern
/// runtimes (Node.js 18+, Next.js, etc.) may bind to either address family.
pub async fn wait_for_port(port: u16, hc: &HealthCheck) -> Result<(), HealthError> {
    let deadline = Duration::from_secs(hc.timeout_seconds);
    let interval = Duration::from_millis(hc.interval_ms);

    let ipv4: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let ipv6: std::net::SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], port).into();

    let result = timeout(deadline, async {
        loop {
            // Accept either IPv4 or IPv6 — whichever the process bound to.
            if TcpStream::connect(ipv4).await.is_ok() || TcpStream::connect(ipv6).await.is_ok() {
                return Ok(());
            }
            sleep(interval).await;
        }
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => {
            // Before returning timeout, check if the port is in use by
            // something other than the expected process (stale process hint).
            let hint = if !crate::port::is_port_available(port) {
                format!(
                    " (note: port {port} is currently in use — \
                     a stale process may be occupying it)"
                )
            } else {
                String::new()
            };
            Err(HealthError::Timeout {
                timeout_seconds: hc.timeout_seconds,
                hint,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 2: HTTP endpoint check
// ---------------------------------------------------------------------------

/// Repeatedly GET an HTTP URL until success or timeout.
pub async fn wait_for_http(url: &str, hc: &HealthCheck) -> Result<(), HealthError> {
    let deadline = Duration::from_secs(hc.timeout_seconds);
    let interval = Duration::from_millis(hc.interval_ms);

    let full_url = if let Some(path) = &hc.path {
        let trimmed = url.trim_end_matches('/');
        let path = if path.starts_with('/') {
            path.clone()
        } else {
            format!("/{path}")
        };
        format!("{trimmed}{path}")
    } else {
        url.to_owned()
    };

    let expected_status = hc.expect_status.unwrap_or(200);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| HealthError::HttpsCheckFailed(e.to_string()))?;

    let result = timeout(deadline, async {
        loop {
            match client.get(&full_url).send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if status == expected_status {
                        return Ok(());
                    }
                    tracing::debug!(
                        url = full_url,
                        status,
                        expected_status,
                        "HTTP health check: unexpected status"
                    );
                }
                Err(e) => {
                    tracing::debug!(url = full_url, error = %e, "HTTP health check: request failed");
                }
            }
            sleep(interval).await;
        }
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(HealthError::Timeout {
            timeout_seconds: hc.timeout_seconds,
            hint: String::new(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Bash health check
// ---------------------------------------------------------------------------

/// Run a bash command as a health check. Exit 0 = healthy.
pub async fn wait_for_bash_check(
    command: &str,
    working_dir: &Path,
    hc: &HealthCheck,
) -> Result<(), HealthError> {
    let deadline = Duration::from_secs(hc.timeout_seconds);
    let interval = Duration::from_millis(hc.interval_ms);

    let cmd = command.to_owned();
    let dir = working_dir.to_path_buf();

    let result = timeout(deadline, async {
        loop {
            let status = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;

            match status {
                Ok(s) if s.success() => return Ok(()),
                Ok(s) => {
                    tracing::debug!(
                        command = cmd,
                        exit_code = s.code().unwrap_or(-1),
                        "bash health check: not yet healthy"
                    );
                }
                Err(e) => {
                    tracing::debug!(command = cmd, error = %e, "bash health check: command error");
                }
            }
            sleep(interval).await;
        }
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(HealthError::Timeout {
            timeout_seconds: hc.timeout_seconds,
            hint: String::new(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Two-phase health check runner
// ---------------------------------------------------------------------------

/// Run the complete two-phase health check for a `start_server` node.
///
/// Phase 1: TCP port check.
/// Phase 2: HTTP endpoint check directly on the port (not through Caddy).
pub async fn run_health_check(
    port: u16,
    _url: Option<&str>,
    working_dir: &Path,
    hc: &HealthCheck,
) -> Result<(), HealthError> {
    // Phase 1: always check the port is bound.
    tracing::info!(port, "health check phase 1: waiting for port");
    wait_for_port(port, hc).await.map_err(|e| {
        HealthError::PortCheckFailed(format!("process did not bind to port {port}: {e}"))
    })?;
    tracing::info!(port, "health check phase 1: port is open");

    // Phase 2: depends on check type.
    match hc.check_type.as_str() {
        "http" => {
            // Check the service directly on its port rather than going through
            // Caddy's HTTPS reverse proxy — this avoids DNS resolution issues
            // for multi-level .localhost subdomains.
            let direct_url = format!("http://127.0.0.1:{port}");
            tracing::info!(url = direct_url, "health check phase 2: waiting for HTTP");
            wait_for_http(&direct_url, hc).await?;
            tracing::info!(url = direct_url, "health check phase 2: HTTP check passed");
        }
        "bash" => {
            if let Some(cmd) = &hc.command {
                tracing::info!(command = cmd, "health check phase 2: running bash check");
                wait_for_bash_check(cmd, working_dir, hc).await?;
                tracing::info!("health check phase 2: bash check passed");
            }
        }
        "port" => {
            // Phase 1 already covers this; phase 2 is a no-op for type "port".
        }
        other => {
            tracing::warn!(
                check_type = other,
                "unknown health check type, skipping phase 2"
            );
        }
    }

    Ok(())
}
