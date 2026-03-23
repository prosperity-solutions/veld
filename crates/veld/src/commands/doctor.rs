use crate::output;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// `veld doctor` — comprehensive system diagnostics.
pub async fn run(json: bool) -> i32 {
    let mut diag = Diagnostics::default();
    diag.gather().await;

    if json {
        println!("{}", serde_json::to_string_pretty(&diag.to_json()).unwrap());
    } else {
        diag.print();
    }

    if diag.checks.iter().any(|c| !c.pass) {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Diagnostics {
    // Installation
    binary_path: String,
    binary_version: String,
    helper_path: String,
    helper_version: String,
    daemon_path: String,
    daemon_version: String,
    caddy_path: String,
    caddy_exists: bool,
    lib_dir: String,
    config_path: String,
    config_mode: String,

    // Services
    helper_status: String,
    daemon_status: String,
    caddy_status: String,
    ca_status: String,

    // Extensions
    extensions: Vec<Extension>,

    // Checks
    checks: Vec<Check>,

    // Tip
    tip: String,
}

struct Check {
    pass: bool,
    label: String,
}

struct Extension {
    name: String,
    status: String,
    hint: String,
}

impl Diagnostics {
    async fn gather(&mut self) {
        self.gather_installation();
        self.gather_services().await;
        self.gather_extensions();
        self.gather_checks().await;
        self.gather_tip();
    }

    // -- Installation --------------------------------------------------------

    fn gather_installation(&mut self) {
        let cli_version = env!("CARGO_PKG_VERSION").to_string();

        // Binary
        self.binary_path = std::env::current_exe()
            .map(|p| tilde_path(&p))
            .unwrap_or_else(|_| "unknown".to_string());
        self.binary_version = cli_version.clone();

        // Lib dir
        let lib = veld_core::paths::lib_dir();
        self.lib_dir = tilde_path(&lib);

        // Helper
        let helper_bin = lib.join("veld-helper");
        self.helper_path = tilde_path(&helper_bin);
        self.helper_version =
            query_binary_version(&helper_bin).unwrap_or_else(|| "not found".into());

        // Daemon
        let daemon_bin = lib.join("veld-daemon");
        self.daemon_path = tilde_path(&daemon_bin);
        self.daemon_version =
            query_binary_version(&daemon_bin).unwrap_or_else(|| "not found".into());

        // Caddy
        let caddy = veld_core::paths::caddy_bin();
        self.caddy_path = tilde_path(&caddy);
        self.caddy_exists = caddy.exists();

        // Config
        let config_path = dirs::home_dir()
            .map(|h| h.join(".veld").join("setup.json"))
            .unwrap_or_else(|| PathBuf::from("~/.veld/setup.json"));
        self.config_path = tilde_path(&config_path);
        self.config_mode = read_mode(&config_path);
    }

    // -- Services ------------------------------------------------------------

    async fn gather_services(&mut self) {
        // Helper
        match veld_core::helper::HelperClient::connect().await {
            Ok(client) => {
                let status_data = client.status().await.ok().and_then(|r| r.data);

                let port_info = status_data
                    .as_ref()
                    .and_then(|d| d.get("https_port"))
                    .and_then(|v| v.as_u64())
                    .map(|https| {
                        let http = if https == 443 { 80 } else { 18080 };
                        format!("port {https}/{http}")
                    })
                    .unwrap_or_default();

                let helper_pid = status_data
                    .as_ref()
                    .and_then(|d| d.get("helper_pid"))
                    .and_then(|v| v.as_u64())
                    .map(|p| format!("pid {p}"))
                    .unwrap_or_default();

                let caddy_pid = status_data
                    .as_ref()
                    .and_then(|d| d.get("caddy_pid"))
                    .and_then(|v| v.as_u64())
                    .map(|p| format!("pid {p}"))
                    .unwrap_or_default();

                let parts: Vec<&str> = [helper_pid.as_str(), port_info.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect();
                if parts.is_empty() {
                    self.helper_status = "running".to_string();
                } else {
                    self.helper_status = format!("running ({})", parts.join(", "));
                }

                // Caddy status from helper's perspective
                let caddy_running = status_data
                    .as_ref()
                    .and_then(|d| d.get("caddy"))
                    .and_then(|v| v.as_str())
                    == Some("running");
                if caddy_running {
                    let caddy_parts: Vec<&str> = [caddy_pid.as_str()]
                        .iter()
                        .filter(|s| !s.is_empty())
                        .copied()
                        .collect();
                    if caddy_parts.is_empty() {
                        self.caddy_status = "running (admin API on 2019, sentinel OK)".to_string();
                    } else {
                        self.caddy_status = format!(
                            "running ({}, admin API on 2019, sentinel OK)",
                            caddy_parts.join(", ")
                        );
                    }
                }
            }
            Err(_) => {
                self.helper_status = "not running".to_string();
            }
        }

        // Daemon
        self.daemon_status = check_daemon_status().await;

        // Caddy (only check independently if helper didn't report it)
        if self.caddy_status.is_empty() || self.caddy_status == "not running" {
            self.caddy_status = check_caddy_status().await;
        }

        // CA
        self.ca_status = check_ca_status();
    }

    // -- Extensions ----------------------------------------------------------

    fn gather_extensions(&mut self) {
        // Hammerspoon (macOS only)
        if cfg!(target_os = "macos") {
            let app_installed = Path::new("/Applications/Hammerspoon.app").exists();
            let spoon_installed = dirs::home_dir()
                .map(|h| h.join(".hammerspoon/Spoons/Veld.spoon/init.lua").exists())
                .unwrap_or(false);

            let (status, hint) = if !app_installed {
                (
                    "not installed".to_string(),
                    "Install Hammerspoon from https://www.hammerspoon.org/".to_string(),
                )
            } else if !spoon_installed {
                (
                    "app installed, Veld.spoon missing".to_string(),
                    "Run `veld setup hammerspoon` to install the menu bar widget".to_string(),
                )
            } else {
                ("installed".to_string(), String::new())
            };

            self.extensions.push(Extension {
                name: "Hammerspoon".to_string(),
                status,
                hint,
            });
        }
    }

    // -- Checks --------------------------------------------------------------

    async fn gather_checks(&mut self) {
        // 1. Helper socket reachable
        let helper_ok = veld_core::helper::HelperClient::connect().await.is_ok();
        self.checks.push(Check {
            pass: helper_ok,
            label: if helper_ok {
                "Helper socket reachable".into()
            } else {
                "Helper socket not reachable".into()
            },
        });

        // Determine HTTPS port for later checks
        let https_port: u16 = if let Ok(client) = veld_core::helper::HelperClient::connect().await {
            client.https_port().await.unwrap_or(18443)
        } else {
            18443
        };

        // 2. Caddy admin API responds
        let caddy_api = http_get_ok("http://localhost:2019/config/").await;
        self.checks.push(Check {
            pass: caddy_api,
            label: if caddy_api {
                "Caddy admin API responds".into()
            } else {
                "Caddy admin API not responding".into()
            },
        });

        // 3. Caddy sentinel verified
        let sentinel = http_get_ok("http://localhost:2019/id/veld-sentinel").await;
        self.checks.push(Check {
            pass: sentinel,
            label: if sentinel {
                "Caddy sentinel verified".into()
            } else {
                "Caddy sentinel not found".into()
            },
        });

        // 4. HTTPS port listening
        let https_ok = tcp_connect_ok("127.0.0.1", https_port).await;
        self.checks.push(Check {
            pass: https_ok,
            label: if https_ok {
                format!("HTTPS port listening ({})", https_port)
            } else {
                format!("HTTPS port not listening ({})", https_port)
            },
        });

        // 5. Feedback server responding
        let feedback_ok = tcp_connect_ok("127.0.0.1", 19899).await;
        self.checks.push(Check {
            pass: feedback_ok,
            label: if feedback_ok {
                "Feedback server responding".into()
            } else {
                "Feedback server not responding".into()
            },
        });

        // 6. .localhost DNS resolves
        let dns_ok = resolve_localhost_dns();
        self.checks.push(Check {
            pass: dns_ok,
            label: if dns_ok {
                ".localhost DNS resolves".into()
            } else {
                ".localhost DNS does not resolve".into()
            },
        });

        // 7. No stale system install
        let stale_path = Path::new("/usr/local/lib/veld");
        let lib = veld_core::paths::lib_dir();
        // Only warn if the system dir exists AND it's not the active lib dir
        let has_stale = stale_path.exists() && lib != stale_path;
        self.checks.push(Check {
            pass: !has_stale,
            label: if has_stale {
                format!("Stale system install at {}", stale_path.display())
            } else {
                "No stale system install".into()
            },
        });

        // 8. No stale binaries next to CLI (e.g. ~/.local/bin/veld-daemon
        //    left over from manual testing while lib dir has the real copy)
        if let Ok(cli_path) = std::env::current_exe() {
            if let Some(cli_dir) = cli_path.parent() {
                for name in ["veld-daemon", "veld-helper"] {
                    let sibling = cli_dir.join(name);
                    let canonical = lib.join(name);
                    // Only flag if both exist and they're different files
                    if sibling.exists() && canonical.exists() && sibling != canonical {
                        let sib_ver =
                            query_binary_version(&sibling).unwrap_or_else(|| "unknown".into());
                        let lib_ver =
                            query_binary_version(&canonical).unwrap_or_else(|| "unknown".into());
                        let stale = sib_ver != lib_ver;
                        self.checks.push(Check {
                            pass: !stale,
                            label: if stale {
                                format!(
                                    "Stale {} at {} ({}) — lib has {}. Remove with: rm {}",
                                    name,
                                    tilde_path(&sibling),
                                    sib_ver,
                                    lib_ver,
                                    tilde_path(&sibling),
                                )
                            } else {
                                format!("No stale {} next to CLI", name)
                            },
                        });
                    }
                }
            }
        }
    }

    // -- Tip -----------------------------------------------------------------

    fn gather_tip(&mut self) {
        let all_pass = self.checks.iter().all(|c| c.pass);
        if self.config_mode == "privileged" && all_pass {
            self.tip = "All checks passed.".to_string();
        } else if !all_pass {
            self.tip = "Some checks failed — see above for details.".to_string();
        } else {
            self.tip = String::new(); // Mode section already shows the upgrade hint
        }
    }

    // -- Output --------------------------------------------------------------

    fn print(&self) {
        println!("{}", output::bold("Veld Doctor"));
        println!();

        // Installation
        println!("  {}", output::bold("Installation"));
        println!(
            "    {:<14}{} (v{})",
            "Binary:", self.binary_path, self.binary_version
        );
        println!(
            "    {:<14}{} ({})",
            "Helper:", self.helper_path, self.helper_version
        );
        println!(
            "    {:<14}{} ({})",
            "Daemon:", self.daemon_path, self.daemon_version
        );
        if self.caddy_exists {
            println!("    {:<14}{}", "Caddy:", self.caddy_path);
        } else {
            println!("    {:<14}{} (not found)", "Caddy:", self.caddy_path);
        }
        println!("    {:<14}{}", "Lib dir:", self.lib_dir);
        println!("    {:<14}{}", "Config:", self.config_path);
        println!();

        // Mode (prominent)
        println!("  {}", output::bold("Mode"));
        match self.config_mode.as_str() {
            "privileged" => {
                println!(
                    "    {} {}",
                    output::checkmark(),
                    output::green("Privileged — clean URLs on ports 80/443")
                );
            }
            "unprivileged" => {
                println!(
                    "    {} Unprivileged — HTTPS on port 18443",
                    output::cyan("●")
                );
                println!(
                    "      {}",
                    output::dim("Run `veld setup privileged` for clean URLs without :18443")
                );
            }
            "auto" => {
                println!(
                    "    {} Auto-bootstrapped — HTTPS on port 18443",
                    output::cyan("●")
                );
                println!(
                    "      {}",
                    output::dim("Run `veld setup privileged` for clean URLs without :18443")
                );
            }
            _ => {
                println!(
                    "    {} {}",
                    output::cross(),
                    output::red(
                        "Not configured — run `veld setup unprivileged` or `veld setup privileged`"
                    )
                );
            }
        }
        println!();

        // Services
        println!("  {}", output::bold("Services"));
        println!(
            "    {:<14}{}",
            "Helper:",
            colorize_status(&self.helper_status)
        );
        println!(
            "    {:<14}{}",
            "Daemon:",
            colorize_status(&self.daemon_status)
        );
        println!(
            "    {:<14}{}",
            "Caddy:",
            colorize_status(&self.caddy_status)
        );
        println!("    {:<14}{}", "CA:", colorize_status(&self.ca_status));
        println!();

        // Checks
        println!("  {}", output::bold("Checks"));
        for check in &self.checks {
            if check.pass {
                println!("    {} {}", output::checkmark(), check.label);
            } else {
                println!("    {} {}", output::cross(), output::red(&check.label));
            }
        }
        println!();

        // Extensions
        if !self.extensions.is_empty() {
            println!("  {}", output::bold("Extensions"));
            for ext in &self.extensions {
                println!(
                    "    {:<18}{}",
                    format!("{}:", ext.name),
                    colorize_status(&ext.status)
                );
                if !ext.hint.is_empty() {
                    println!("    {:<18}{}", "", output::dim(&ext.hint));
                }
            }
            println!();
        }

        // Tip (only if there's something to say)
        if !self.tip.is_empty() {
            println!("  {}", output::dim(&self.tip));
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let checks: Vec<serde_json::Value> = self
            .checks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "pass": c.pass,
                    "label": c.label,
                })
            })
            .collect();

        let extensions: Vec<serde_json::Value> = self
            .extensions
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "status": e.status,
                    "hint": e.hint,
                })
            })
            .collect();

        serde_json::json!({
            "installation": {
                "binary_path": self.binary_path,
                "binary_version": self.binary_version,
                "helper_path": self.helper_path,
                "helper_version": self.helper_version,
                "daemon_path": self.daemon_path,
                "daemon_version": self.daemon_version,
                "caddy_path": self.caddy_path,
                "caddy_exists": self.caddy_exists,
                "lib_dir": self.lib_dir,
                "config_path": self.config_path,
                "config_mode": self.config_mode,
            },
            "services": {
                "helper": self.helper_status,
                "daemon": self.daemon_status,
                "caddy": self.caddy_status,
                "ca": self.ca_status,
            },
            "checks": checks,
            "extensions": extensions,
            "tip": self.tip,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Replace the home directory prefix with `~`.
fn tilde_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(suffix) = path.strip_prefix(&home) {
            return format!("~/{}", suffix.display());
        }
    }
    path.display().to_string()
}

/// Query a binary's version by running `<path> --version`.
fn query_binary_version(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let out = Command::new(path).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let version = stdout.split_whitespace().last()?.to_string();
    if version.contains('.') {
        Some(format!("v{version}"))
    } else {
        None
    }
}

/// Read the mode from `~/.veld/setup.json`.
fn read_mode(path: &Path) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return "not configured".to_string(),
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return "not configured".to_string(),
    };
    value
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("not configured")
        .to_string()
}

/// Check daemon status via launchctl (macOS) or socket existence.
async fn check_daemon_status() -> String {
    // Try launchctl on macOS
    if cfg!(target_os = "macos") {
        if let Ok(out) = Command::new("launchctl").arg("list").output() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if line.contains("veld.daemon") || line.contains("veld-daemon") {
                    // Format: PID\tStatus\tLabel
                    let parts: Vec<&str> = line.split('\t').collect();
                    if let Some(pid_str) = parts.first() {
                        if let Ok(pid) = pid_str.trim().parse::<u32>() {
                            return format!("running (pid {pid})");
                        }
                    }
                    return "loaded (not running)".to_string();
                }
            }
        }
    }

    // Try daemon socket
    let daemon_sock = dirs::home_dir().map(|h| h.join(".veld").join("daemon.sock"));
    if let Some(ref sock) = daemon_sock {
        if sock.exists() {
            if tokio::net::UnixStream::connect(sock).await.is_ok() {
                return "running".to_string();
            }
            return "socket exists (not responding)".to_string();
        }
    }

    "not running".to_string()
}

/// Check Caddy status via its admin API.
async fn check_caddy_status() -> String {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return "unknown (HTTP client error)".to_string(),
    };

    // Check sentinel
    let sentinel_ok = client
        .get("http://localhost:2019/id/veld-sentinel")
        .send()
        .await
        .is_ok_and(|r| r.status().is_success());

    if sentinel_ok {
        "running (admin API on 2019, sentinel OK)".to_string()
    } else {
        // Maybe admin API is up but no sentinel
        match client.get("http://localhost:2019/config/").send().await {
            Ok(r) if r.status().is_success() => {
                "running (admin API on 2019, sentinel missing)".to_string()
            }
            _ => "not running".to_string(),
        }
    }
}

/// Check CA trust status.
///
/// In privileged mode, Caddy runs as root and its `caddy-data/pki/` directory
/// is root-owned with mode 700. This means `path.exists()` and `metadata()`
/// both return false/Err when run as the normal user. To handle this, we check
/// the macOS keychain directly (which doesn't need file access) before falling
/// back to the cert file on disk.
fn check_ca_status() -> String {
    let ca_cert = veld_core::paths::caddy_data_dir()
        .join("pki")
        .join("authorities")
        .join("local")
        .join("root.crt");

    if cfg!(target_os = "macos") {
        // Try verify-cert if the file is readable.
        if ca_cert.exists() {
            if let Ok(out) = Command::new("security")
                .args(["verify-cert", "-c"])
                .arg(&ca_cert)
                .output()
            {
                if out.status.success() {
                    return "trusted (login keychain)".to_string();
                }
            }
        }

        // Check the keychain directly by certificate name. This works even when
        // the cert file on disk is unreadable (root-owned in privileged mode).
        // The CA may be named "Veld Local CA" (custom) or "Caddy Local Authority"
        // (Caddy default).
        for name in ["Veld Local CA", "Caddy Local Authority"] {
            if let Ok(out) = Command::new("security")
                .args(["find-certificate", "-c", name, "-a"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if out.status.success() && !stdout.is_empty() {
                    // Found in keychain — verify trust by extracting and checking.
                    if is_ca_trusted_in_keychain(name) {
                        return "trusted (login keychain)".to_string();
                    }
                    return "installed (may not be trusted)".to_string();
                }
            }
        }

        if ca_cert.exists() {
            return "not trusted (cert exists but not in keychain)".to_string();
        }

        return "not found".to_string();
    }

    if !ca_cert.exists() {
        return "not found".to_string();
    }

    // Fallback for non-macOS: cert file exists
    "present (trust status unknown)".to_string()
}

/// Check whether a CA certificate is actually trusted (not just present) in
/// the macOS keychain by running `security verify-cert` against a temp copy
/// extracted from the keychain.
fn is_ca_trusted_in_keychain(name: &str) -> bool {
    // Export the cert from the keychain to a temp file, then verify it.
    let tmp = std::env::temp_dir().join("veld-doctor-ca-check.pem");
    let export_ok = Command::new("security")
        .args(["find-certificate", "-c", name, "-p"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() && !out.stdout.is_empty() {
                std::fs::write(&tmp, &out.stdout).ok()
            } else {
                None
            }
        });

    if export_ok.is_none() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }

    let trusted = Command::new("security")
        .args(["verify-cert", "-c"])
        .arg(&tmp)
        .output()
        .is_ok_and(|out| out.status.success());

    let _ = std::fs::remove_file(&tmp);
    trusted
}

/// Try an HTTP GET and return true if status is success.
async fn http_get_ok(url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Try a TCP connection to host:port.
async fn tcp_connect_ok(host: &str, port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

/// Check that `test.localhost` resolves to 127.0.0.1.
fn resolve_localhost_dns() -> bool {
    match ("test.localhost", 80u16).to_socket_addrs() {
        Ok(addrs) => addrs
            .into_iter()
            .any(|a| a.ip() == std::net::Ipv4Addr::LOCALHOST),
        Err(_) => false,
    }
}

/// Colorize service status strings.
fn colorize_status(status: &str) -> String {
    if status.starts_with("running") || status.starts_with("trusted") {
        output::green(status)
    } else if status.starts_with("not running")
        || status.starts_with("not found")
        || status.starts_with("not trusted")
    {
        output::red(status)
    } else {
        output::yellow(status)
    }
}
