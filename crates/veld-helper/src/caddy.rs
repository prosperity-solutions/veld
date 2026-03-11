use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Caddy admin API base URL.
const CADDY_ADMIN_API: &str = "http://localhost:2019";

/// Manages the Caddy process and its routes.
#[derive(Debug)]
pub struct CaddyManager {
    inner: Arc<Mutex<CaddyState>>,
    client: reqwest::Client,
}

#[derive(Debug)]
struct CaddyState {
    /// PID of the managed Caddy process, if running.
    child_pid: Option<u32>,
}

impl CaddyManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaddyState { child_pid: None })),
            client: reqwest::Client::new(),
        }
    }

    /// Start the Caddy process if it is not already running, and ensure the
    /// base config is loaded.
    pub async fn start(&self) -> Result<()> {
        let mut state = self.inner.lock().await;

        if let Some(pid) = state.child_pid {
            if is_process_alive(pid) {
                info!(pid, "caddy is already running, ensuring base config");
                drop(state);
                self.reload()
                    .await
                    .context("failed to reload caddy config")?;
                return Ok(());
            }
            // Stale PID.
            state.child_pid = None;
        }

        // Check if Caddy is already running externally (e.g. from a previous
        // helper instance). If the admin API responds, just reload the config.
        drop(state);
        if self.is_running().await {
            info!("caddy admin API already reachable, loading base config");
            self.reload()
                .await
                .context("failed to reload caddy config")?;
            return Ok(());
        }

        let mut state = self.inner.lock().await;
        let caddy_bin = veld_core::paths::caddy_bin();
        if !caddy_bin.exists() {
            anyhow::bail!("caddy not found at {}", caddy_bin.display());
        }

        let child = tokio::process::Command::new(&caddy_bin)
            .arg("run")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("spawning caddy at {}", caddy_bin.display()))?;

        let pid = child.id().context("failed to get caddy PID")?;
        state.child_pid = Some(pid);
        drop(state); // release lock before HTTP call

        // Wait for the admin API to become available, then load the base config
        // (TLS internal issuer + HTTP/HTTPS listeners).
        info!(pid, "caddy process started, loading base config...");
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
        self.reload()
            .await
            .context("failed to load caddy base config")?;
        info!(pid, "caddy started with base config");
        Ok(())
    }

    /// Stop the Caddy process.
    pub async fn stop(&self) -> Result<()> {
        let mut state = self.inner.lock().await;

        // Try graceful shutdown via the admin API first.
        let stop_url = format!("{CADDY_ADMIN_API}/stop");
        match self.client.post(&stop_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("caddy stopped via admin API");
                state.child_pid = None;
                return Ok(());
            }
            Ok(resp) => {
                debug!(
                    status = %resp.status(),
                    "caddy admin API /stop returned non-success; falling back to signal"
                );
            }
            Err(e) => {
                debug!("caddy admin API not reachable for /stop: {e}; falling back to signal");
            }
        }

        // Fall back to SIGTERM.
        if let Some(pid) = state.child_pid.take() {
            let pid = nix::unistd::Pid::from_raw(pid as i32);
            if let Err(e) = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM) {
                warn!(%e, "failed to send SIGTERM to caddy");
            } else {
                info!(?pid, "sent SIGTERM to caddy");
            }
        }

        Ok(())
    }

    /// Reload caddy configuration via the admin API.
    pub async fn reload(&self) -> Result<()> {
        let load_url = format!("{CADDY_ADMIN_API}/load");
        let config = build_base_config();

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
    pub async fn add_route(&self, route_id: &str, hostname: &str, upstream: &str) -> Result<()> {
        let route = build_route_json(route_id, hostname, upstream);

        let url = format!("{CADDY_ADMIN_API}/config/apps/http/servers/veld/routes",);

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&route)?)
            .send()
            .await
            .context("adding route to caddy")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("caddy add route returned error: {body}");
        }

        info!(route_id, hostname, upstream, "caddy route added");
        Ok(())
    }

    /// Remove a route by its `@id` via the Caddy admin API.
    pub async fn remove_route(&self, route_id: &str) -> Result<()> {
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

    /// Check whether caddy is running and reachable.
    pub async fn is_running(&self) -> bool {
        let url = format!("{CADDY_ADMIN_API}/config/");
        matches!(self.client.get(&url).send().await, Ok(r) if r.status().is_success())
    }
}

// ---------------------------------------------------------------------------
// Caddy JSON config builders
// ---------------------------------------------------------------------------

/// Build a minimal base Caddy config with a server block for Veld.
fn build_base_config() -> serde_json::Value {
    let data_dir = veld_core::paths::caddy_data_dir();
    // Ensure the data directory exists so Caddy can write PKI data.
    let _ = std::fs::create_dir_all(&data_dir);

    serde_json::json!({
        "storage": {
            "module": "file_system",
            "root": data_dir.to_string_lossy()
        },
        "apps": {
            "http": {
                "servers": {
                    "veld": {
                        "listen": [":443", ":80"],
                        "routes": []
                    }
                }
            },
            "pki": {
                "certificate_authorities": {
                    "local": {
                        "name": "Veld Local CA"
                    }
                }
            },
            "tls": {
                "automation": {
                    "policies": [{
                        "issuers": [{
                            "module": "internal"
                        }]
                    }]
                }
            }
        }
    })
}

/// Build a single route entry with hostname matching, TLS, and reverse proxy.
fn build_route_json(route_id: &str, hostname: &str, upstream: &str) -> serde_json::Value {
    serde_json::json!({
        "@id": route_id,
        "match": [{
            "host": [hostname]
        }],
        "handle": [
            {
                "handler": "subroute",
                "routes": [{
                    "handle": [{
                        "handler": "reverse_proxy",
                        "upstreams": [{
                            "dial": upstream
                        }]
                    }]
                }]
            }
        ],
        "terminal": true
    })
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
    fn test_build_route_json() {
        let route = build_route_json("test-route", "app.test.localhost", "localhost:3000");
        assert_eq!(route["@id"], "test-route");
        assert_eq!(route["match"][0]["host"][0], "app.test.localhost");
    }

    #[test]
    fn test_build_base_config() {
        let config = build_base_config();
        assert!(config["apps"]["http"]["servers"]["veld"].is_object());
    }
}
