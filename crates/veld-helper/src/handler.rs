use serde_json::Value;
use tracing::{info, warn};

use crate::caddy::CaddyManager;
use crate::dns::{self, DnsManager};
use crate::protocol::{Handled, Request, Response};
use crate::sleep::SleepManager;

/// Shared state for all connection handlers.
pub struct State {
    dns: DnsManager,
    caddy: CaddyManager,
    https_port: u16,
    http_port: u16,
    /// Root's half of the keep-awake switch. See [`crate::sleep`].
    sleep: SleepManager,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl State {
    pub fn new(
        https_port: u16,
        http_port: u16,
        caddy_bin: Option<std::path::PathBuf>,
        shutdown_tx: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        // Beside the helper's other durable state, and derived from the same
        // `--caddy-bin` override the plist already passes — never from
        // `lib_dir()`, which resolves against `$HOME` and lands a root daemon's
        // marker in `/var/root`, divorced from the tree it runs out of.
        let marker_path = crate::caddy::caddy_data_dir(&caddy_bin).join("sleep-lease.json");
        Self {
            dns: DnsManager::new(),
            caddy: CaddyManager::new(https_port, http_port, caddy_bin),
            https_port,
            http_port,
            sleep: SleepManager::new(marker_path),
            shutdown_tx,
        }
    }

    /// Startup reconcile: re-adopt + reload an already-running Caddy (e.g. one
    /// orphaned across our own self-restart) so an updated binary/config takes
    /// effect and the watchdog can supervise it. No-op if Caddy isn't running.
    pub async fn reconcile_caddy_on_startup(&self) {
        self.caddy.reconcile_on_startup().await;
    }

    /// One watchdog iteration: ensure Caddy is alive and serving the persisted
    /// routes. Only supervises once Caddy is meant to be running — either it has
    /// been started this session, or there are persisted routes to serve (e.g.
    /// after a reboot/update). This avoids spawning Caddy on a fresh install
    /// that has never started a run.
    pub async fn caddy_watchdog_tick(&self) {
        let should_run =
            self.caddy.pid().await.is_some() || self.caddy.stored_route_count().await > 0;
        if !should_run {
            return;
        }
        match self.caddy.ensure_healthy().await {
            Ok(true) => info!("watchdog restarted caddy and replayed routes"),
            Ok(false) => {}
            Err(e) => warn!(error = %format!("{e:#}"), "watchdog caddy recovery failed"),
        }
    }

    /// Parse and dispatch a single JSON request line.
    pub async fn handle_request(&self, line: &str) -> Handled {
        let request: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => return Handled::reply(Response::err(format!("invalid request JSON: {e}"))),
        };

        // Both exiting commands are handled ahead of the table, because their
        // replies must be flushed before the process goes away and the table's
        // arms all return a plain `Response`.
        if request.command == veld_core::helper::RESTART {
            return self.handle_restart().await;
        }

        // The other one: `shutdown` ends the process too, and additionally stops
        // Caddy on the way out.
        if request.command == "shutdown" {
            return self.handle_shutdown().await;
        }

        Handled::reply(match request.command.as_str() {
            "add_host" => self.handle_add_host(&request.args).await,
            "remove_host" => self.handle_remove_host(&request.args).await,
            "add_route" => self.handle_add_route(&request.args).await,
            "remove_route" => self.handle_remove_route(&request.args).await,
            "remove_routes_by_prefix" => self.handle_remove_routes_by_prefix(&request.args).await,
            "reload_dns" => self.handle_reload_dns().await,
            "caddy_start" => self.handle_caddy_start().await,
            "caddy_stop" => self.handle_caddy_stop().await,
            "caddy_reload" => self.handle_caddy_reload().await,
            "status" => self.handle_status().await,
            veld_core::helper::HOLD_SLEEP_DISABLED => {
                self.handle_hold_sleep_disabled(&request.args).await
            }
            veld_core::helper::RELEASE_SLEEP_DISABLED => self.handle_release_sleep_disabled().await,
            other => {
                warn!(command = other, "unknown command");
                Response::err(format!("unknown command: {other}"))
            }
        })
    }

    /// Exit so the service manager relaunches us onto the binary now on disk,
    /// leaving Caddy running so every live URL stays up across the swap.
    ///
    /// This is what `veld update` calls instead of waiting out the binary
    /// watcher's poll. The difference that matters is not speed: the CLI *knows*
    /// when the installer finished writing, where the watcher can only infer it
    /// from a settling size/mtime — an inference that has lost the race against
    /// the installer's own multi-step write in the field. Same safety gate as
    /// the watcher ([`crate::restart_blocker`]), so neither path can exit into a
    /// hole the other refuses.
    async fn handle_restart(&self) -> Handled {
        if let Some(reason) = crate::restart_blocker().await {
            warn!(reason, "refusing restart request");
            return Handled::reply(Response::err(reason));
        }
        info!("restart requested — exiting so the service manager relaunches the new binary");
        Handled {
            response: Response::ok(),
            exit_after_reply: true,
        }
    }

    /// Signal the accept loop to exit. Caddy is deliberately left running — see
    /// [`Self::handle_restart`].
    pub fn signal_exit(&self) {
        let _ = self.shutdown_tx.send(true);
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
        let client_log_levels = args
            .get("client_log_levels")
            .and_then(Value::as_str)
            .unwrap_or("log,warn,error");
        let inject = args.get("inject").and_then(Value::as_bool).unwrap_or(true);
        let inject_feedback_overlay = args
            .get("inject_feedback_overlay")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let inject_client_logs = args
            .get("inject_client_logs")
            .and_then(Value::as_bool)
            .unwrap_or(true);
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
                    client_log_levels,
                    inject,
                    inject_feedback_overlay,
                    inject_client_logs,
                })
            }
            (None, None, None) => None,
            _ => {
                warn!("partial feedback config in add_route args — disabling injection");
                None
            }
        };

        // Reverse-proxy header rules, if the orchestrator resolved any.
        let proxy = parse_proxy_arg(args);

        match self
            .caddy
            .add_route(route_id, hostname, upstream, feedback, &proxy)
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

    async fn handle_remove_routes_by_prefix(&self, args: &Value) -> Response {
        let prefix = match args.get("prefix").and_then(Value::as_str) {
            Some(v) => v,
            None => return Response::err("missing 'prefix' in args"),
        };

        match self.caddy.remove_routes_by_prefix(prefix).await {
            Ok(_) => Response::ok(),
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
        let caddy_pid = self.caddy.pid().await;
        let dns_entries = self.dns.entry_count().await;
        let helper_pid = std::process::id();

        Response::ok_with_data(serde_json::json!({
            "caddy": if caddy_running { "running" } else { "stopped" },
            "caddy_pid": caddy_pid,
            "dns_entries": dns_entries,
            "https_port": self.https_port,
            "http_port": self.http_port,
            "helper_pid": helper_pid,
            "version": env!("CARGO_PKG_VERSION"),
            "stored_routes": self.caddy.stored_route_count().await,
            // Whether a keep-awake lease is armed right now. Read by `veld
            // doctor` (`crates/veld/src/commands/doctor.rs`, the helper row), so a
            // support transcript can answer "why is this Mac not sleeping"
            // without anyone having to run `pmset -g` — including for a lease
            // taken straight on this socket, which the IDE never shows.
            "sleep_disabled": self.sleep.held().await,
        }))
    }

    /// Disable battery sleep, or renew an existing hold. See [`crate::sleep`].
    ///
    /// `lease_secs` is required rather than defaulted: the lease is the entire
    /// safety property, and a caller that forgot the field would otherwise get
    /// some silently-chosen window. An old daemon never sends this command at
    /// all, so there is no compatibility case to default for.
    async fn handle_hold_sleep_disabled(&self, args: &Value) -> Response {
        let lease_secs = match args.get("lease_secs").and_then(Value::as_u64) {
            Some(s) if s > 0 => s,
            _ => return Response::err("missing or invalid 'lease_secs' in args"),
        };
        match self.sleep.hold(lease_secs).await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    /// Re-enable battery sleep now, rather than waiting out the lease.
    async fn handle_release_sleep_disabled(&self) -> Response {
        match self.sleep.release().await {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(format!("{e:#}")),
        }
    }

    /// One watchdog iteration for the sleep lease. Separate from
    /// [`Self::caddy_watchdog_tick`] and driven by its own task: a Caddy
    /// recovery can take seconds, and the lease must not be held past its
    /// deadline because something unrelated was slow.
    pub async fn sleep_watchdog_tick(&self) {
        self.sleep.watchdog_tick().await;
    }

    /// Startup reconcile for the sleep lease — see
    /// [`crate::sleep::SleepManager::reconcile_on_startup`].
    pub async fn reconcile_sleep_on_startup(&self) {
        self.sleep.reconcile_on_startup().await;
    }

    /// Re-enable battery sleep on the way out.
    ///
    /// Unlike Caddy — deliberately left running so URLs survive a restart — this
    /// is always reverted. A helper that exits is a helper that has stopped
    /// watching the lease, and an unwatched `disablesleep` is the failure this
    /// whole mechanism exists to prevent. It also covers the case with no other
    /// answer: `veld uninstall` stops the service, and nothing would ever come
    /// back to clear the setting.
    pub async fn release_sleep_on_exit(&self) {
        if let Err(e) = self.sleep.release().await {
            warn!(error = %format!("{e:#}"), "could not re-enable battery sleep while exiting");
        }
    }

    /// Stop Caddy and exit. Unlike [`Self::handle_restart`] this takes the URLs
    /// down with it, which is why an update does not use it.
    ///
    /// Routed through `exit_after_reply` rather than signalling inline: it had the
    /// same write-vs-exit race `restart` was built to avoid — the process could be
    /// gone before its own `{"ok":true}` left the socket, so a caller saw a dropped
    /// connection and could not tell "shutting down" from "died". Leaving one of the
    /// two exiting commands on the old path would also have been a trap for whoever
    /// adds the third.
    async fn handle_shutdown(&self) -> Handled {
        info!("shutdown command received, stopping caddy and signalling exit");
        if let Err(e) = self.caddy.stop().await {
            warn!("error stopping caddy during shutdown: {e:#}");
        }
        Handled {
            response: Response::ok(),
            exit_after_reply: true,
        }
    }
}

/// Extract the resolved proxy header rules from add_route args. Absent → default
/// (no manipulation). A malformed value is logged and treated as absent rather
/// than failing the whole route — but the log makes a daemon↔helper version-skew
/// serialization mismatch diagnosable instead of silently dropping the rules.
fn parse_proxy_arg(args: &Value) -> veld_core::config::ResolvedProxy {
    match args.get("proxy") {
        None => veld_core::config::ResolvedProxy::default(),
        Some(v) => match serde_json::from_value::<veld_core::config::ResolvedProxy>(v.clone()) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "ignoring malformed proxy config in add_route args");
                veld_core::config::ResolvedProxy::default()
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proxy_arg_absent_is_default() {
        let args = serde_json::json!({ "route_id": "r" });
        assert!(parse_proxy_arg(&args).is_empty());
    }

    #[test]
    fn parse_proxy_arg_reads_valid_rules() {
        let args = serde_json::json!({
            "proxy": { "request": { "remove": ["Origin"] } }
        });
        let p = parse_proxy_arg(&args);
        assert_eq!(p.request.remove, vec!["Origin"]);
    }

    #[test]
    fn parse_proxy_arg_malformed_falls_back_to_default() {
        // Wrong shape (a string where an object is expected) → default, no panic.
        let args = serde_json::json!({ "proxy": "not-an-object" });
        assert!(parse_proxy_arg(&args).is_empty());
    }

    /// A `State` whose sleep marker lives in a tempdir.
    ///
    /// Load-bearing, not tidiness. With the production path these tests would
    /// read the **installed privileged helper's** real marker, and `release`
    /// would then execute `/usr/bin/pmset disablesleep 0` for real — the exact
    /// hazard `sleep.rs`'s `SleepSetter` seam exists to prevent, reintroduced one
    /// layer up. A tempdir marker never exists, so `release` returns before it
    /// reaches `pmset` at all.
    fn test_state() -> (State, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::watch::channel(false);
        let caddy_bin = Some(dir.path().join("caddy"));
        (State::new(443, 80, caddy_bin, tx), dir)
    }

    /// A lease request with no usable `lease_secs` is refused *before* anything
    /// touches `pmset`.
    ///
    /// Two things at once, and both matter. The refusal is the safety property:
    /// the lease is the only thing that ever gives the setting back, so a
    /// request that does not state one must not be honoured with some default.
    /// And reaching a refusal at all proves the command is wired into the
    /// dispatch table — a name that did not match would answer
    /// `unknown command`, which the daemon is required to treat as "no battery
    /// coverage here" and carry on, so the whole feature would be silently off
    /// with every other test still green.
    ///
    /// Note what this deliberately does not do: send a *valid* lease. That would
    /// execute `pmset -b disablesleep 1` for real, and run as root it would
    /// leave a developer's Mac unable to sleep — the setting is durable.
    #[tokio::test]
    async fn a_lease_request_without_a_duration_is_refused_before_pmset_runs() {
        let (state, _dir) = test_state();
        for args in [
            serde_json::json!({}),
            serde_json::json!({ "lease_secs": 0 }),
            serde_json::json!({ "lease_secs": "600" }),
            serde_json::json!({ "lease_secs": -1 }),
        ] {
            let line = serde_json::json!({
                "command": veld_core::helper::HOLD_SLEEP_DISABLED,
                "args": args,
            })
            .to_string();
            let handled = state.handle_request(&line).await;
            assert!(!handled.response.ok, "{args} should be refused");
            assert!(
                handled
                    .response
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("lease_secs")),
                "the refusal must name the field, not read as `unknown command`: {:?}",
                handled.response.error
            );
            assert!(!handled.exit_after_reply);
        }
    }

    /// Releasing is safe to reach in a test — on a non-macOS builder it is a
    /// no-op, and on macOS `pmset -b disablesleep 0` only ever moves the machine
    /// toward being *able* to sleep, which is the direction that cannot hurt.
    #[tokio::test]
    async fn releasing_a_lease_that_was_never_taken_succeeds() {
        let (state, _dir) = test_state();
        let line = serde_json::json!({
            "command": veld_core::helper::RELEASE_SLEEP_DISABLED,
            "args": {},
        })
        .to_string();
        let handled = state.handle_request(&line).await;
        // Idempotent: the daemon releases on every teardown without checking
        // whether it ever held one, and a stop must not surface an error for it.
        assert!(handled.response.ok, "{:?}", handled.response.error);
        assert!(!state.sleep.held().await);
    }
}
