use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Caddy admin API base URL.
const CADDY_ADMIN_API: &str = "http://localhost:2019";

/// Reserved hostname for the browser management UI.
const MANAGEMENT_HOST: &str = "veld.localhost";

/// Port the daemon's HTTP server listens on (feedback + management).
const DAEMON_HTTP_PORT: u16 = 19899;

/// Manages the Caddy process and its routes.
#[derive(Debug)]
pub struct CaddyManager {
    inner: Arc<Mutex<CaddyState>>,
    client: reqwest::Client,
    https_port: u16,
    http_port: u16,
    /// Override for the Caddy binary path (avoids lib_dir() issues under sudo).
    caddy_bin_override: Option<std::path::PathBuf>,
}

#[derive(Debug)]
struct CaddyState {
    /// PID of the managed Caddy process, if running.
    child_pid: Option<u32>,
}

impl CaddyManager {
    pub fn new(https_port: u16, http_port: u16, caddy_bin: Option<std::path::PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaddyState { child_pid: None })),
            client: reqwest::Client::new(),
            https_port,
            http_port,
            caddy_bin_override: caddy_bin,
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
        // helper instance). If it is, reload the base config to ensure any new
        // built-in routes (e.g. management UI) are registered.
        drop(state);
        if self.is_running().await {
            info!("caddy admin API already reachable, reloading base config");
            self.reload()
                .await
                .context("failed to reload caddy base config on existing instance")?;
            return Ok(());
        }

        let mut state = self.inner.lock().await;
        let caddy_bin = self
            .caddy_bin_override
            .clone()
            .unwrap_or_else(veld_core::paths::caddy_bin);
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
        let config = build_base_config(self.https_port, self.http_port, &self.caddy_bin_override);

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

    /// Check whether caddy is running and reachable by querying the veld
    /// sentinel route. Returns `true` only when our Caddy instance is running
    /// (i.e. the sentinel route exists), not an unrelated Caddy process.
    pub async fn is_running(&self) -> bool {
        let url = format!("{CADDY_ADMIN_API}/id/veld-sentinel");
        matches!(self.client.get(&url).send().await, Ok(r) if r.status().is_success())
    }

    /// Return the stored Caddy child PID, if known.
    pub async fn pid(&self) -> Option<u32> {
        self.inner.lock().await.child_pid
    }
}

// ---------------------------------------------------------------------------
// Feedback config
// ---------------------------------------------------------------------------

/// Configuration for feedback overlay / client-side injection on a route.
pub struct FeedbackConfig<'a> {
    pub upstream: &'a str,
    pub run_name: &'a str,
    pub project_root: &'a str,
    /// Comma-separated client log levels (e.g. "log,warn,error").
    pub client_log_levels: &'a str,
    /// Whether to inject the feedback overlay toolbar.
    pub inject_feedback_overlay: bool,
    /// Whether to inject the client-side log collector.
    pub inject_client_logs: bool,
}

// ---------------------------------------------------------------------------
// Caddy JSON config builders
// ---------------------------------------------------------------------------

/// Build a minimal base Caddy config with a server block for Veld.
fn build_base_config(
    https_port: u16,
    http_port: u16,
    caddy_bin_override: &Option<std::path::PathBuf>,
) -> serde_json::Value {
    // If caddy_bin was overridden, derive data_dir from its parent (sibling "caddy-data").
    let data_dir = caddy_bin_override
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.join("caddy-data"))
        .unwrap_or_else(veld_core::paths::caddy_data_dir);
    // Ensure the data directory exists so Caddy can write PKI data.
    let _ = std::fs::create_dir_all(&data_dir);

    let https_listen = format!(":{https_port}");
    let http_listen = format!(":{http_port}");
    let management_upstream = format!("127.0.0.1:{DAEMON_HTTP_PORT}");

    serde_json::json!({
        "storage": {
            "module": "file_system",
            "root": data_dir.to_string_lossy()
        },
        "apps": {
            "http": {
                "servers": {
                    "veld": {
                        "listen": [https_listen, http_listen],
                        "routes": [
                            {
                                "@id": "veld-management",
                                "match": [{"host": [MANAGEMENT_HOST]}],
                                "handle": [{
                                    "handler": "reverse_proxy",
                                    "upstreams": [{"dial": management_upstream}]
                                }],
                                "terminal": true
                            },
                            {
                                "@id": "veld-sentinel",
                                "match": [{"path": ["/__veld_sentinel__"]}],
                                "handle": [{"handler": "static_response", "body": "veld"}],
                                "terminal": true
                            }
                        ]
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
/// 2. The main app proxy uses the `veld_inject` Caddy handler to prepend a
///    bootstrap `<script>` tag to HTML responses. The handler streams the
///    response without buffering — it writes the prefix before the first body
///    chunk and passes the rest through. This enables streaming SSR, WebSocket
///    upgrades, and SSE without any bypass routes (the handler properly
///    delegates Flusher and Hijacker).
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

        let bootstrap = build_bootstrap_script(&fb);

        if bootstrap.is_empty() {
            // Both features disabled — plain reverse proxy for the main app.
            subroutes.push(serde_json::json!({
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": upstream }]
                }]
            }));
        } else {
            // veld_inject prepends the bootstrap script to text/html responses
            // without buffering. Accept-Encoding: identity ensures the upstream
            // sends uncompressed HTML (can't prepend to gzipped bytes).
            subroutes.push(serde_json::json!({
                "handle": [
                    {
                        "handler": "veld_inject",
                        "prefix": bootstrap
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
        }
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

/// Build the inline bootstrap `<script>` tag that is prepended to HTML
/// responses by the `veld_inject` Caddy handler.
///
/// The script runs before any app code (it is prepended before `<!DOCTYPE>`).
/// It immediately intercepts console methods to capture early logs, then
/// dynamically loads the full client-log collector and/or feedback overlay
/// assets once the DOM is ready.
fn build_bootstrap_script(fb: &FeedbackConfig<'_>) -> String {
    if !fb.inject_client_logs && !fb.inject_feedback_overlay {
        return String::new();
    }

    let mut js = String::from(
        "(function(){\"use strict\";\
         if(window.__veld_cl)return;\
         window.__veld_cl=1;",
    );

    // --- Immediate console interception (before any app code) ---
    if fb.inject_client_logs {
        // Escape levels for safe embedding in JS string.
        let levels = escape_js_string(fb.client_log_levels);
        js.push_str(&format!(
            "var V={levels}.split(','),B=window.__veld_early_logs=[],O={{}};\
             V.forEach(function(n){{\
             var o=console[n];if(typeof o!=='function')return;\
             O[n]=o;\
             console[n]=function(){{\
             B.push({{l:n,a:Array.from(arguments),t:Date.now()}});\
             o.apply(console,arguments);\
             }};}});\
             window.__veld_early_originals=O;\
             window.addEventListener('error',function(e){{\
             try{{B.push({{l:'exception',m:e.message||String(e),\
             s:e.error&&e.error.stack?e.error.stack:'',t:Date.now()}});\
             }}catch(_){{}}\
             }});\
             window.addEventListener('unhandledrejection',function(e){{\
             try{{var r=e.reason;\
             B.push({{l:'exception',m:'Unhandled Promise rejection: '+(r instanceof Error?r.message:String(r||'')),\
             s:r instanceof Error&&r.stack?r.stack:'',t:Date.now()}});\
             }}catch(_){{}}\
             }});",
            levels = levels,
        ));
    }

    // --- Dynamic asset loading ---
    js.push_str(
        "function E(t,a){var e=document.createElement(t);\
         for(var k in a)e.setAttribute(k,a[k]);\
         (document.head||document.documentElement).appendChild(e);return e;}\
         function R(fn){document.readyState==='loading'?\
         document.addEventListener('DOMContentLoaded',fn):fn();}R(function(){",
    );

    if fb.inject_client_logs {
        let levels = escape_js_string_bare(fb.client_log_levels);
        js.push_str(&format!(
            "E('script',{{'src':'/__veld__/api/client-log.js','data-veld-levels':'{levels}'}});",
            levels = levels,
        ));
    }

    if fb.inject_feedback_overlay {
        js.push_str(
            "E('link',{'rel':'stylesheet','href':'/__veld__/feedback/style.css'});\
             var s=document.createElement('style');\
             s.textContent=\"@font-face{font-family:'JetBrains Mono';font-style:normal;\
             font-weight:400;font-display:swap;\
             src:local('JetBrains Mono Regular'),local('JetBrainsMono-Regular');}\";\
             (document.head||document.documentElement).appendChild(s);\
             E('script',{'src':'/__veld__/feedback/script.js'});",
        );
    }

    js.push_str("});})();");

    format!("<script>{js}</script>")
}

/// Escape a string for safe embedding inside a JavaScript single-quoted string.
/// Returns the value wrapped in single quotes: `'escaped'`.
fn escape_js_string(s: &str) -> String {
    format!("'{}'", escape_js_string_bare(s))
}

/// Escape a string for safe embedding in JS without adding outer quotes.
/// Use this when the string will be placed inside an already-quoted context.
fn escape_js_string_bare(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\x3c"), // prevent </script> injection
            '>' => out.push_str("\\x3e"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
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

    // -----------------------------------------------------------------------
    // Route structure tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_route_json() {
        let route = build_route_json("test-route", "app.test.localhost", "localhost:3000", None);
        assert_eq!(route["@id"], "test-route");
        assert_eq!(route["match"][0]["host"][0], "app.test.localhost");
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        assert_eq!(subroutes.len(), 1);
        assert_eq!(subroutes[0]["handle"][0]["handler"], "reverse_proxy");
        assert!(
            subroutes[0]["match"].is_null(),
            "catch-all route has no matcher"
        );
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
                client_log_levels: "log,warn,error",
                inject_feedback_overlay: true,
                inject_client_logs: true,
            }),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        // /__veld__/* + veld_inject catch-all (no bypass routes needed).
        assert_eq!(subroutes.len(), 2);

        // First subroute: feedback API.
        assert_eq!(subroutes[0]["match"][0]["path"][0], "/__veld__/*");
        let fb_proxy = &subroutes[0]["handle"][1];
        assert_eq!(
            fb_proxy["headers"]["request"]["set"]["X-Veld-Run"][0],
            "my-run"
        );
        assert_eq!(
            fb_proxy["headers"]["request"]["set"]["X-Veld-Project"][0],
            "/tmp/project"
        );

        // Second subroute: veld_inject + reverse_proxy.
        let handlers = subroutes[1]["handle"].as_array().unwrap();
        assert_eq!(handlers.len(), 2);
        assert_eq!(handlers[0]["handler"], "veld_inject");
        assert!(handlers[0]["prefix"].as_str().unwrap().contains("<script>"));
        assert_eq!(handlers[1]["handler"], "reverse_proxy");
        assert_eq!(
            handlers[1]["headers"]["request"]["set"]["Accept-Encoding"][0],
            "identity"
        );
        assert_eq!(handlers[1]["upstreams"][0]["dial"], "localhost:3000");

        // Verify bootstrap script contains both features.
        let prefix = handlers[0]["prefix"].as_str().unwrap();
        assert!(prefix.contains("client-log.js"), "should load client-log");
        assert!(
            prefix.contains("__veld_early_logs"),
            "should buffer early logs"
        );
        assert!(prefix.contains("style.css"), "should load overlay CSS");
        assert!(
            prefix.contains("feedback/script.js"),
            "should load overlay JS"
        );
        assert!(prefix.contains("@font-face"), "should include font-face");
    }

    #[test]
    fn test_build_route_json_feedback_overlay_only() {
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "my-run",
                project_root: "/tmp/project",
                client_log_levels: "log,warn,error",
                inject_feedback_overlay: true,
                inject_client_logs: false,
            }),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        assert_eq!(subroutes.len(), 2);
        let prefix = subroutes[1]["handle"][0]["prefix"].as_str().unwrap();
        assert!(prefix.contains("style.css"), "should load overlay CSS");
        assert!(
            prefix.contains("feedback/script.js"),
            "should load overlay JS"
        );
        assert!(
            !prefix.contains("client-log.js"),
            "should NOT load client-log"
        );
        assert!(
            !prefix.contains("__veld_early_logs"),
            "should NOT intercept console"
        );
    }

    #[test]
    fn test_build_route_json_client_logs_only() {
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "my-run",
                project_root: "/tmp/project",
                client_log_levels: "warn,error",
                inject_feedback_overlay: false,
                inject_client_logs: true,
            }),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        assert_eq!(subroutes.len(), 2);
        let prefix = subroutes[1]["handle"][0]["prefix"].as_str().unwrap();
        assert!(prefix.contains("client-log.js"), "should load client-log");
        assert!(
            prefix.contains("__veld_early_logs"),
            "should intercept console"
        );
        assert!(!prefix.contains("style.css"), "should NOT load overlay CSS");
        assert!(
            !prefix.contains("feedback/script.js"),
            "should NOT load overlay JS"
        );
    }

    #[test]
    fn test_build_route_json_all_features_disabled() {
        let route = build_route_json(
            "test-route",
            "app.test.localhost",
            "localhost:3000",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "my-run",
                project_root: "/tmp/project",
                client_log_levels: "log,warn,error",
                inject_feedback_overlay: false,
                inject_client_logs: false,
            }),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();
        // /__veld__/* + plain proxy (no veld_inject).
        assert_eq!(subroutes.len(), 2);
        assert_eq!(subroutes[0]["match"][0]["path"][0], "/__veld__/*");
        let handlers = subroutes[1]["handle"].as_array().unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["handler"], "reverse_proxy");
        assert!(
            subroutes[1]["match"].is_null(),
            "catch-all route has no matcher"
        );
    }

    /// Verify the veld_inject route is structurally correct: it uses
    /// veld_inject + reverse_proxy (no bypass routes), proxies to the app
    /// upstream, and sets Accept-Encoding: identity.
    #[test]
    fn test_veld_inject_route_structure() {
        let route = build_route_json(
            "inject-test",
            "app.test.localhost",
            "localhost:5555",
            Some(FeedbackConfig {
                upstream: "localhost:19899",
                run_name: "run",
                project_root: "/tmp",
                client_log_levels: "log",
                inject_feedback_overlay: true,
                inject_client_logs: true,
            }),
        );
        let subroutes = route["handle"][0]["routes"].as_array().unwrap();

        // Only 2 subroutes: /__veld__/* and catch-all. No bypass routes.
        assert_eq!(subroutes.len(), 2);
        assert_eq!(subroutes[0]["match"][0]["path"][0], "/__veld__/*");

        // Catch-all has exactly 2 handlers: veld_inject + reverse_proxy.
        let handlers = subroutes[1]["handle"].as_array().unwrap();
        assert_eq!(handlers.len(), 2);
        assert_eq!(handlers[0]["handler"], "veld_inject");
        assert!(!handlers[0]["prefix"].as_str().unwrap().is_empty());
        assert_eq!(handlers[1]["handler"], "reverse_proxy");
        assert_eq!(handlers[1]["upstreams"][0]["dial"], "localhost:5555");
        assert_eq!(
            handlers[1]["headers"]["request"]["set"]["Accept-Encoding"][0],
            "identity"
        );

        // No matcher on the catch-all route.
        assert!(
            subroutes[1]["match"].is_null(),
            "catch-all route has no matcher"
        );
    }

    // -----------------------------------------------------------------------
    // Base config tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_base_config() {
        let config = build_base_config(443, 80, &None);
        assert!(config["apps"]["http"]["servers"]["veld"].is_object());
        let listen = config["apps"]["http"]["servers"]["veld"]["listen"]
            .as_array()
            .unwrap();
        assert_eq!(listen[0], ":443");
        assert_eq!(listen[1], ":80");
        let routes = config["apps"]["http"]["servers"]["veld"]["routes"]
            .as_array()
            .unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0]["@id"], "veld-management");
        assert_eq!(routes[0]["match"][0]["host"][0], MANAGEMENT_HOST);
        assert_eq!(routes[1]["@id"], "veld-sentinel");
    }

    #[test]
    fn test_build_base_config_custom_ports() {
        let config = build_base_config(18443, 18080, &None);
        let listen = config["apps"]["http"]["servers"]["veld"]["listen"]
            .as_array()
            .unwrap();
        assert_eq!(listen[0], ":18443");
        assert_eq!(listen[1], ":18080");
    }

    // -----------------------------------------------------------------------
    // Bootstrap script tests
    // -----------------------------------------------------------------------

    fn make_fb<'a>(overlay: bool, logs: bool, levels: &'a str) -> FeedbackConfig<'a> {
        FeedbackConfig {
            upstream: "localhost:19899",
            run_name: "run",
            project_root: "/tmp",
            client_log_levels: levels,
            inject_feedback_overlay: overlay,
            inject_client_logs: logs,
        }
    }

    #[test]
    fn test_bootstrap_script_both_features() {
        let script = build_bootstrap_script(&make_fb(true, true, "log,warn,error"));
        assert!(script.starts_with("<script>"));
        assert!(script.ends_with("</script>"));
        // Console interception.
        assert!(script.contains("__veld_early_logs"));
        assert!(script.contains("__veld_cl"));
        // Dynamic asset loading.
        assert!(script.contains("client-log.js"));
        assert!(script.contains("style.css"));
        assert!(script.contains("feedback/script.js"));
        assert!(script.contains("@font-face"));
    }

    #[test]
    fn test_bootstrap_script_overlay_only() {
        let script = build_bootstrap_script(&make_fb(true, false, "log,warn,error"));
        assert!(script.contains("style.css"));
        assert!(script.contains("feedback/script.js"));
        assert!(script.contains("@font-face"));
        assert!(!script.contains("client-log.js"));
        assert!(!script.contains("__veld_early_logs"));
    }

    #[test]
    fn test_bootstrap_script_logs_only() {
        let script = build_bootstrap_script(&make_fb(false, true, "warn,error"));
        assert!(script.contains("client-log.js"));
        assert!(script.contains("__veld_early_logs"));
        assert!(!script.contains("style.css"));
        assert!(!script.contains("feedback/script.js"));
    }

    #[test]
    fn test_bootstrap_script_neither_feature() {
        let script = build_bootstrap_script(&make_fb(false, false, "log"));
        assert!(script.is_empty());
    }

    #[test]
    fn test_bootstrap_script_custom_levels() {
        let script = build_bootstrap_script(&make_fb(false, true, "debug,info"));
        assert!(script.contains("debug,info"));
    }

    #[test]
    fn test_bootstrap_script_escaping() {
        // Levels with special chars should be escaped safely.
        let script = build_bootstrap_script(&make_fb(false, true, "log'</script>"));
        assert!(!script.contains("'</script>'"));
        assert!(script.contains("\\x3c/script\\x3e"));
        assert!(script.contains("\\'"));
    }

    #[test]
    fn test_escape_js_string() {
        assert_eq!(escape_js_string("hello"), "'hello'");
        assert_eq!(escape_js_string("it's"), "'it\\'s'");
        assert_eq!(escape_js_string("a\\b"), "'a\\\\b'");
        assert_eq!(escape_js_string("<script>"), "'\\x3cscript\\x3e'");
        assert_eq!(escape_js_string("a\nb"), "'a\\nb'");
        assert_eq!(escape_js_string("a\rb"), "'a\\rb'");
    }

    #[test]
    fn test_bootstrap_script_is_valid_html() {
        let script = build_bootstrap_script(&make_fb(true, true, "log"));
        // Must be a single script tag.
        assert_eq!(
            script.matches("<script>").count(),
            1,
            "should have exactly one opening script tag"
        );
        assert_eq!(
            script.matches("</script>").count(),
            1,
            "should have exactly one closing script tag"
        );
    }

    #[test]
    fn test_bootstrap_script_dedup_guard() {
        let script = build_bootstrap_script(&make_fb(true, true, "log"));
        // Guard prevents double execution.
        assert!(script.contains("if(window.__veld_cl)return"));
        assert!(script.contains("window.__veld_cl=1"));
    }

    #[test]
    fn test_bootstrap_script_error_handlers() {
        let script = build_bootstrap_script(&make_fb(false, true, "log"));
        // Should capture unhandled errors and promise rejections.
        assert!(script.contains("addEventListener('error'"));
        assert!(script.contains("addEventListener('unhandledrejection'"));
    }

    /// Regression: the bootstrap script must not have duplicate variable/function
    /// names. Previously `L` was used for both the levels array and the DOM
    /// element helper, causing `Uncaught SyntaxError: Unexpected identifier`.
    #[test]
    fn test_bootstrap_script_no_duplicate_identifiers() {
        let script = build_bootstrap_script(&make_fb(true, true, "log,warn,error"));
        let js = script
            .strip_prefix("<script>")
            .unwrap()
            .strip_suffix("</script>")
            .unwrap();

        // Collect all single-letter `var X=` and `function X(` declarations.
        let mut decls: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
        for pattern in ["var ", "function "] {
            let mut search_from = 0;
            while let Some(pos) = js[search_from..].find(pattern) {
                let abs = search_from + pos + pattern.len();
                if let Some(ch) = js[abs..].chars().next() {
                    if ch.is_ascii_uppercase() {
                        *decls.entry(ch).or_default() += 1;
                    }
                }
                search_from = abs + 1;
            }
        }
        for (name, count) in &decls {
            assert_eq!(
                *count, 1,
                "identifier '{name}' declared {count} times — would cause SyntaxError"
            );
        }
    }

    /// Regression: the data-veld-levels attribute value must not have nested
    /// quotes. Previously `escape_js_string` wrapped the value in quotes,
    /// then it was placed inside an already-quoted JS object literal property,
    /// producing `'data-veld-levels':''log,warn,error''`.
    #[test]
    fn test_bootstrap_script_no_nested_quotes_in_attributes() {
        let script = build_bootstrap_script(&make_fb(true, true, "log,warn,error"));
        // The attribute value should be 'log,warn,error' not ''log,warn,error''
        assert!(
            !script.contains("':''"),
            "attribute value has nested quotes — would produce invalid JS"
        );
        assert!(
            script.contains("'data-veld-levels':'log,warn,error'"),
            "attribute value should be properly single-quoted"
        );
    }
}
