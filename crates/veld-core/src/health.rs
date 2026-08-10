use std::path::Path;
use std::time::Duration;

use thiserror::Error;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

use crate::config::{CommandSpec, HealthCheck};

/// Callback invoked on each health check retry with the attempt number.
pub type AttemptNotifier = Box<dyn Fn(u32) + Send + Sync>;

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

    #[error("command health check failed with exit code {0}")]
    CommandCheckFailed(i32),
}

// ---------------------------------------------------------------------------
// Phase 1: TCP port check
// ---------------------------------------------------------------------------

/// Repeatedly try to connect to `port` on localhost until success or timeout.
///
/// Tries both IPv4 (127.0.0.1) and IPv6 (::1) on each attempt since modern
/// runtimes (Node.js 18+, Next.js, etc.) may bind to either address family.
pub async fn wait_for_port(
    port: u16,
    hc: &HealthCheck,
    on_attempt: Option<&AttemptNotifier>,
) -> Result<(), HealthError> {
    let deadline = Duration::from_secs(hc.timeout_seconds);
    let interval = Duration::from_millis(hc.interval_ms);

    let ipv4: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let ipv6: std::net::SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], port).into();

    let result = timeout(deadline, async {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            if let Some(f) = &on_attempt {
                f(attempt);
            }
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
pub async fn wait_for_http(
    url: &str,
    hc: &HealthCheck,
    on_attempt: Option<&AttemptNotifier>,
) -> Result<(), HealthError> {
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
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            if let Some(f) = &on_attempt {
                f(attempt);
            }
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
// Command health check
// ---------------------------------------------------------------------------

/// Run a command as a health check. Exit 0 = healthy.
///
/// `env` is the environment the probe command runs with — the node's veld-owned
/// vars plus its declared `env`, so a probe can be parameterised the same way
/// the node it probes is. Pass an empty map for no extra environment.
pub async fn wait_for_command_check(
    command: &CommandSpec,
    working_dir: &Path,
    env: &std::collections::HashMap<String, String>,
    hc: &HealthCheck,
    on_attempt: Option<&AttemptNotifier>,
) -> Result<(), HealthError> {
    let deadline = Duration::from_secs(hc.timeout_seconds);
    let interval = Duration::from_millis(hc.interval_ms);

    let cmd = command.clone();
    let dir = working_dir.to_path_buf();

    // A probe's stderr is the *diagnosis* when it fails, so it cannot be thrown
    // at /dev/null: capture the last few lines of each failing attempt and the
    // most recent exit status, and surface them in the timeout error. Without
    // this a probe that prints "DATABASE_URL is not set" to stderr reports
    // only "health check timed out after 60s".
    const TAIL_LINES: usize = 5;
    let mut tail: Vec<String> = Vec::new();
    let mut last_exit: Option<i32> = None;

    let result = timeout(deadline, async {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            if let Some(f) = &on_attempt {
                f(attempt);
            }
            // Rebuilt each attempt: a `Command` is consumed by `output()`.
            let output = match crate::process::tokio_command(&cmd) {
                Ok(mut c) => {
                    c.current_dir(&dir)
                        .envs(env)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped())
                        .output()
                        .await
                }
                Err(e) => {
                    // An unrunnable probe (empty argv) can never become healthy;
                    // let the deadline report it rather than spinning silently.
                    tracing::debug!(error = %e, "command health check: unrunnable probe");
                    sleep(interval).await;
                    continue;
                }
            };

            match output {
                Ok(out) if out.status.success() => return Ok(()),
                Ok(out) => {
                    last_exit = Some(out.status.code().unwrap_or(-1));
                    if !out.stderr.is_empty() {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        for line in stderr.lines() {
                            let line = line.trim_end();
                            if !line.is_empty() {
                                tail.push(line.to_owned());
                            }
                        }
                        let drop = tail.len().saturating_sub(TAIL_LINES);
                        if drop > 0 {
                            tail.drain(..drop);
                        }
                    }
                    tracing::debug!(
                        command = cmd.display(),
                        exit_code = out.status.code().unwrap_or(-1),
                        "command health check: not yet healthy"
                    );
                }
                Err(e) => {
                    tracing::debug!(command = cmd.display(), error = %e, "command health check: command error");
                }
            }
            sleep(interval).await;
        }
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => {
            let mut hint = String::new();
            if let Some(code) = last_exit {
                hint.push_str(&format!(" — last attempt exited with code {code}"));
            }
            if !tail.is_empty() {
                hint.push_str(" — last stderr:\n");
                for line in &tail {
                    hint.push_str("    ");
                    hint.push_str(line);
                    hint.push('\n');
                }
            }
            Err(HealthError::Timeout {
                timeout_seconds: hc.timeout_seconds,
                hint,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Two-phase health check runner
// ---------------------------------------------------------------------------

/// Run the complete two-phase health check for a `start_server` node.
///
/// Phase 1: TCP port check.
/// Phase 2: HTTP endpoint check directly on the port (not through Caddy).
///
/// Note: The orchestrator inlines the two-phase logic for progress events,
/// but this function remains available for external callers and tests.
#[allow(dead_code)]
pub async fn run_health_check(
    port: u16,
    _url: Option<&str>,
    working_dir: &Path,
    hc: &HealthCheck,
) -> Result<(), HealthError> {
    // Phase 1: always check the port is bound.
    tracing::info!(port, "health check phase 1: waiting for port");
    wait_for_port(port, hc, None).await.map_err(|e| {
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
            wait_for_http(&direct_url, hc, None).await?;
            tracing::info!(url = direct_url, "health check phase 2: HTTP check passed");
        }
        "command" | "bash" => {
            if let Some(cmd) = hc.cmd.spec() {
                tracing::info!(
                    command = cmd.display(),
                    "health check phase 2: running command check"
                );
                wait_for_command_check(
                    &cmd,
                    working_dir,
                    &std::collections::HashMap::new(),
                    hc,
                    None,
                )
                .await?;
                tracing::info!("health check phase 2: command check passed");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CommandKeys, HealthCheck};

    fn failing_probe() -> HealthCheck {
        HealthCheck {
            check_type: "command".to_owned(),
            path: None,
            expect_status: None,
            cmd: CommandKeys {
                shell: Some("echo boom-to-stderr >&2; exit 3".to_owned()),
                ..Default::default()
            },
            port: None,
            seconds: None,
            timeout_seconds: 1,
            interval_ms: 100,
        }
    }

    /// A failing command probe used to report only "health check timed out
    /// after 1s" — its stderr went to /dev/null and its exit status was
    /// discarded. The whole diagnosis (an echo, a missing env var, a typo'd
    /// binary) had to be re-derived by hand. The timeout error must carry the
    /// exit code and the last few stderr lines.
    #[tokio::test]
    async fn a_failing_command_probe_reports_its_stderr_and_exit_code() {
        let hc = failing_probe();
        let cmd = hc.cmd.spec().expect("probe declares a shell command");
        let env = std::collections::HashMap::new();
        let err = wait_for_command_check(&cmd, std::path::Path::new("."), &env, &hc, None)
            .await
            .expect_err("a probe that always exits 3 must time out");
        let msg = err.to_string();
        assert!(
            msg.contains("code 3"),
            "timeout error should name the exit code, got: {msg}"
        );
        assert!(
            msg.contains("boom-to-stderr"),
            "timeout error should include the captured stderr, got: {msg}"
        );
    }

    /// A passing probe still succeeds, and the captured-output machinery must
    /// not change that.
    #[tokio::test]
    async fn a_passing_command_probe_succeeds() {
        let hc = HealthCheck {
            cmd: CommandKeys {
                shell: Some("exit 0".to_owned()),
                ..Default::default()
            },
            ..failing_probe()
        };
        let cmd = hc.cmd.spec().unwrap();
        let env = std::collections::HashMap::new();
        wait_for_command_check(&cmd, std::path::Path::new("."), &env, &hc, None)
            .await
            .expect("an exit-0 probe is healthy");
    }
}
