use serde_json::Value;
use tracing::warn;

use crate::caddy::CaddyManager;
use crate::dns::{self, DnsManager};
use crate::protocol::{Request, Response};

/// Shared state for all connection handlers.
pub struct State {
    dns: DnsManager,
    caddy: CaddyManager,
}

impl State {
    pub fn new() -> Self {
        Self {
            dns: DnsManager::new(),
            caddy: CaddyManager::new(),
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

        match self.caddy.add_route(route_id, hostname, upstream).await {
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
        }))
    }
}
