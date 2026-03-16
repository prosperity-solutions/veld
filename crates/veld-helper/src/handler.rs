use serde_json::Value;
use tracing::{info, warn};

use crate::caddy::CaddyManager;
use crate::dns::{self, DnsManager};
use crate::protocol::{Request, Response};

/// Shared state for all connection handlers.
pub struct State {
    dns: DnsManager,
    caddy: CaddyManager,
    https_port: u16,
    http_port: u16,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl State {
    pub fn new(
        https_port: u16,
        http_port: u16,
        shutdown_tx: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        Self {
            dns: DnsManager::new(),
            caddy: CaddyManager::new(https_port, http_port),
            https_port,
            http_port,
            shutdown_tx,
        }
    }

    /// Parse and dispatch a single JSON request line, returning a `Response`.
    pub async fn handle_request(&self, line: &str) -> Response {
        let request: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => return Response::err(format!("invalid request JSON: {e}")),
        };

        match request.command.as_str() {
            "add_host" => self.handle_add_host(&request.args).await,
            "remove_host" => self.handle_remove_host(&request.args).await,
            "add_route" => self.handle_add_route(&request.args).await,
            "remove_route" => self.handle_remove_route(&request.args).await,
            "reload_dns" => self.handle_reload_dns().await,
            "caddy_start" => self.handle_caddy_start().await,
            "caddy_stop" => self.handle_caddy_stop().await,
            "caddy_reload" => self.handle_caddy_reload().await,
            "status" => self.handle_status().await,
            "shutdown" => self.handle_shutdown().await,
            other => {
                warn!(command = other, "unknown command");
                Response::err(format!("unknown command: {other}"))
            }
        }
    }

    async fn handle_add_host(&self, args: &Value) -> Response {
        let hostname = match args.get("hostname").and_then(Value::as_str) {
            Some(h) => h,
            None => return Response::err("missing 'hostname' in args"),
        };
        let ip = args
            .get("ip")
            .and_then(Value::as_str)
            .unwrap_or("127.0.0.1");

        match self.dns.add_host(hostname, ip).await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    async fn handle_remove_host(&self, args: &Value) -> Response {
        let hostname = match args.get("hostname").and_then(Value::as_str) {
            Some(h) => h,
            None => return Response::err("missing 'hostname' in args"),
        };

        match self.dns.remove_host(hostname).await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    async fn handle_add_route(&self, args: &Value) -> Response {
        let route_id = match args.get("route_id").and_then(Value::as_str) {
            Some(v) => v,
            None => return Response::err("missing 'route_id' in args"),
        };
        let hostname = match args.get("hostname").and_then(Value::as_str) {
            Some(v) => v,
            None => return Response::err("missing 'hostname' in args"),
        };
        let upstream = match args.get("upstream").and_then(Value::as_str) {
            Some(v) => v,
            None => return Response::err("missing 'upstream' in args"),
        };

        // Build feedback config if the orchestrator included feedback fields.
        let feedback = match (
            args.get("feedback_upstream").and_then(Value::as_str),
            args.get("run_name").and_then(Value::as_str),
            args.get("project_root").and_then(Value::as_str),
        ) {
            (Some(fb_upstream), Some(run_name), Some(project_root)) => {
                Some(crate::caddy::FeedbackConfig {
                    upstream: fb_upstream,
                    run_name,
                    project_root,
                })
            }
            (None, None, None) => None,
            _ => {
                warn!("partial feedback config in add_route args — disabling feedback overlay");
                None
            }
        };

        match self
            .caddy
            .add_route(route_id, hostname, upstream, feedback)
            .await
        {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    async fn handle_remove_route(&self, args: &Value) -> Response {
        let route_id = match args.get("route_id").and_then(Value::as_str) {
            Some(v) => v,
            None => return Response::err("missing 'route_id' in args"),
        };

        match self.caddy.remove_route(route_id).await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    async fn handle_reload_dns(&self) -> Response {
        match dns::reload_dns().await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    async fn handle_caddy_start(&self) -> Response {
        match self.caddy.start().await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    async fn handle_caddy_stop(&self) -> Response {
        match self.caddy.stop().await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    async fn handle_caddy_reload(&self) -> Response {
        match self.caddy.reload().await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    async fn handle_status(&self) -> Response {
        let caddy_running = self.caddy.is_running().await;
        let dns_entries = self.dns.entry_count().await;

        Response::ok_with_data(serde_json::json!({
            "caddy": if caddy_running { "running" } else { "stopped" },
            "dns_entries": dns_entries,
            "https_port": self.https_port,
            "http_port": self.http_port,
        }))
    }

    async fn handle_shutdown(&self) -> Response {
        info!("shutdown command received, stopping caddy and signalling exit");
        if let Err(e) = self.caddy.stop().await {
            warn!("error stopping caddy during shutdown: {e:#}");
        }
        let _ = self.shutdown_tx.send(true);
        Response::ok()
    }
}
