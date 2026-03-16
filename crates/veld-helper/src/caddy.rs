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
                info!(pid, "caddy is already running");
                return Ok(());
            }
            // Stale PID.
            state.child_pid = None;
        }

        // Check if Caddy is already running externally (e.g. from a previous
        // helper instance). If the admin API responds, nothing to do — routes
        // are added individually via add_route(), not via base config reload.
        drop(state);
        if self.is_running().await {
            info!("caddy admin API already reachable, skipping startup");
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
    pub async fn add_route(
        &self,
        route_id: &str,
        hostname: &str,
        upstream: &str,
        feedback: Option<FeedbackConfig<'_>>,
    ) -> Result<()> {
        let route = build_route_json(route_id, hostname, upstream, feedback);

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
// Feedback config
// ---------------------------------------------------------------------------

/// Configuration for feedback overlay injection on a route.
pub struct FeedbackConfig<'a> {
    pub upstream: &'a str,
    pub run_name: &'a str,
    pub project_root: &'a str,
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
///
/// When feedback is configured:
/// 1. `/__veld__/*` routes to the daemon's feedback HTTP server (API + assets)
///    with `X-Veld-Run` and `X-Veld-Project` headers injected by Caddy.
/// 2. The main app proxy uses Caddy's `replace` handler to inject the feedback
///    overlay `<script>` tag into HTML responses automatically — no Service
///    Worker, no manual activation, it just works.
fn build_route_json(
    route_id: &str,
    hostname: &str,
    upstream: &str,
    feedback: Option<FeedbackConfig<'_>>,
) -> serde_json::Value {
    let mut subroutes = Vec::new();

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

        // Main app proxy with HTML injection via the replace-response plugin.
        // We set Accept-Encoding: identity so the upstream sends uncompressed
        // responses — replace_response cannot match inside gzip'd bytes.
        //
        // Two replacements:
        // 1. </head> → inject @font-face for JetBrains Mono + CSS link
        // 2. </body> → inject overlay script + CSS link
        subroutes.push(serde_json::json!({
            "handle": [
                {
                    "handler": "replace_response",
                    "match": {
                        "headers": {
                            "Content-Type": ["text/html*"]
                        }
                    },
                    "replacements": [
                        {
                            "search": "</head>",
                            "replace": "<style>@font-face{font-family:'JetBrains Mono';font-style:normal;font-weight:400;font-display:swap;src:local('JetBrains Mono Regular'),local('JetBrainsMono-Regular');}</style><link rel=\"stylesheet\" href=\"/__veld__/feedback/style.css\"></head>"
                        },
                        {
                            "search": "</body>",
                            "replace": "<script src=\"/__veld__/feedback/script.js\"></script></body>"
                        }
                    ]
                },
                {
                    "handler": "reverse_proxy",
                    "headers": {
                        "request": {
                            "set": {
                                "Accept-Encoding": ["identity"]
                            }
                        }
                    },
                    "upstreams": [{ "dial": upstream }]
                }
            ]
        }));
    } else {
        // No feedback — plain reverse proxy.
        subroutes.push(serde_json::json!({
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{
                    "dial": upstream
                }]
            }]
        }));
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
        let route = build_route_json("test-route", "app.test.localhost", "localhost:3000", None);
        assert_eq!(route["@id"], "test-route");
        assert_eq!(route["match"][0]["host"][0], "app.test.localhost");
        // Without feedback, only one subroute (the main proxy).
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        assert_eq!(subroutes.len(), 1);
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
            }),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        assert_eq!(subroutes.len(), 2);
        // First subroute matches /__veld__/*
        assert_eq!(subroutes[0]["match"][0]["path"][0], "/__veld__/*");
        // Verify headers are set on the feedback proxy.
        let fb_proxy = &subroutes[0]["handle"][1];
        assert_eq!(
            fb_proxy["headers"]["request"]["set"]["X-Veld-Run"][0],
            "my-run"
        );
        assert_eq!(
            fb_proxy["headers"]["request"]["set"]["X-Veld-Project"][0],
            "/tmp/project"
        );
        // Main app proxy has two replacements: </head> for font, </body> for script+CSS.
        let replace_handler = &subroutes[1]["handle"][0];
        let replacements = replace_handler["replacements"].as_array().unwrap();
        assert_eq!(replacements.len(), 2);
        assert_eq!(replacements[0]["search"], "</head>");
        assert!(replacements[0]["replace"].as_str().unwrap().contains("@font-face"));
        assert!(replacements[0]["replace"].as_str().unwrap().contains("style.css"));
        assert_eq!(replacements[1]["search"], "</body>");
        assert!(replacements[1]["replace"].as_str().unwrap().contains("script.js"));
        // Main app proxy strips compression so replace_response can work.
        let main_proxy = &subroutes[1]["handle"][1];
        assert_eq!(
            main_proxy["headers"]["request"]["set"]["Accept-Encoding"][0],
            "identity"
        );
    }

    #[test]
    fn test_build_base_config() {
        let config = build_base_config();
        assert!(config["apps"]["http"]["servers"]["veld"].is_object());
    }
}
