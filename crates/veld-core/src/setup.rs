use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;

use crate::helper::{self, HelperClient};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum SetupError {
    #[error("veld setup has not been completed")]
    Incomplete { missing: Vec<String> },
}

// ---------------------------------------------------------------------------
// Setup status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStatus {
    pub helper_running: bool,
    pub caddy_present: bool,
}

impl SetupStatus {
    /// Return a list of components that are missing / not running.
    pub fn missing(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if !self.helper_running {
            missing.push("veld-helper".to_owned());
        }
        if !self.caddy_present {
            missing.push("caddy".to_owned());
        }
        missing
    }

    pub fn is_complete(&self) -> bool {
        self.helper_running && self.caddy_present
    }
}

// ---------------------------------------------------------------------------
// Check functions
// ---------------------------------------------------------------------------

/// Probe the system to determine setup status.
pub async fn check_setup() -> SetupStatus {
    let helper_running = check_helper_running().await;
    let caddy_present = crate::paths::caddy_bin().exists();

    SetupStatus {
        helper_running,
        caddy_present,
    }
}

/// Try to contact veld-helper via its socket.
async fn check_helper_running() -> bool {
    let client = HelperClient::new(&helper::default_socket_path());
    (client.status().await).is_ok()
}

/// Enforce that setup is complete. Returns an error with structured info
/// if any component is missing.
pub async fn require_setup() -> Result<SetupStatus, SetupError> {
    let status = check_setup().await;
    if status.is_complete() {
        Ok(status)
    } else {
        Err(SetupError::Incomplete {
            missing: status.missing(),
        })
    }
}

/// Structured JSON representation of the setup-required error.
pub fn setup_required_json(missing: &[String]) -> serde_json::Value {
    serde_json::json!({
        "error": "setup_required",
        "message": "Run `veld setup` to complete one-time system setup.",
        "missing": missing,
    })
}

// ---------------------------------------------------------------------------
// Setup step results (used by `veld setup` command)
// ---------------------------------------------------------------------------

/// Short result message from a setup step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub message: String,
}

impl StepResult {
    pub fn success(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Setup steps
// ---------------------------------------------------------------------------

/// Check whether a port has something listening on it.
fn is_port_in_use(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// Check that the required ports (80, 443, 2019) are free.
///
/// If Caddy is already running (admin API responds on 2019), all three ports
/// are considered owned by Veld and the check passes — this makes `veld setup`
/// idempotent.
pub async fn check_ports() -> Result<StepResult, anyhow::Error> {
    // If our own Caddy is already running, ports are ours — skip the check.
    if is_caddy_running().await {
        return Ok(StepResult::success(
            "Ports in use by Veld's own Caddy (already set up)",
        ));
    }

    let ports = [80u16, 443, 2019];
    let mut in_use = Vec::new();

    for port in ports {
        if is_port_in_use(port) {
            in_use.push(port);
        }
    }

    if in_use.is_empty() {
        Ok(StepResult::success("Ports 80, 443, and 2019 are available"))
    } else {
        let list: Vec<String> = in_use.iter().map(|p| p.to_string()).collect();
        anyhow::bail!(
            "The following ports are already in use: {}",
            list.join(", ")
        )
    }
}

/// Check if our Caddy instance is responding on the admin API.
async fn is_caddy_running() -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_default();
    client
        .get("http://localhost:2019/config/")
        .send()
        .await
        .is_ok()
}

/// Install (or verify) the Caddy web server.
pub async fn install_caddy() -> Result<StepResult, anyhow::Error> {
    let lib_dir = crate::paths::lib_dir();
    let caddy = lib_dir.join("caddy");
    if caddy.exists() {
        return Ok(StepResult::success("Caddy is already installed"));
    }

    std::fs::create_dir_all(&lib_dir).context(format!("failed to create {}", lib_dir.display()))?;

    let (_, arch) = platform_pair()?;
    // Caddy uses "mac" for macOS (not "darwin")
    let caddy_os = match std::env::consts::OS {
        "macos" => "mac",
        "linux" => "linux",
        other => anyhow::bail!("unsupported OS: {other}"),
    };
    let version = "2.11.2";
    let url = format!(
        "https://github.com/caddyserver/caddy/releases/download/v{version}/caddy_{version}_{caddy_os}_{arch}.tar.gz"
    );

    // Download to temp file
    let tmp_dir = std::env::temp_dir().join("veld-setup");
    std::fs::create_dir_all(&tmp_dir)?;
    let tarball = tmp_dir.join("caddy.tar.gz");

    download_binary(&url, &tarball)
        .await
        .context("failed to download Caddy tarball")?;

    // Extract caddy binary from tarball
    let status = tokio::process::Command::new("tar")
        .args(["xzf"])
        .arg(&tarball)
        .arg("-C")
        .arg(&tmp_dir)
        .arg("caddy")
        .status()
        .await
        .context("failed to extract Caddy tarball")?;

    if !status.success() {
        anyhow::bail!("tar extraction failed");
    }

    let extracted = tmp_dir.join("caddy");
    std::fs::rename(&extracted, &caddy)
        .or_else(|_| {
            // rename fails across filesystems, fall back to copy
            std::fs::copy(&extracted, &caddy)?;
            std::fs::remove_file(&extracted)?;
            Ok::<(), std::io::Error>(())
        })
        .context("failed to install Caddy binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&caddy, std::fs::Permissions::from_mode(0o755))?;
    }

    // Clean up
    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(StepResult::success(format!("Caddy {version} installed")))
}

/// Trust Caddy's internal CA root certificate in the system trust store.
///
/// Caddy generates its own internal CA when configured with `tls internal`.
/// The root cert is stored at `{caddy_data_dir}/pki/authorities/local/root.crt`.
/// This step adds that cert to the OS trust store so browsers accept HTTPS
/// connections to `.localhost` domains without warnings.
pub async fn trust_caddy_ca() -> Result<StepResult, anyhow::Error> {
    let root_cert = crate::paths::caddy_data_dir()
        .join("pki")
        .join("authorities")
        .join("local")
        .join("root.crt");

    if !root_cert.exists() {
        // Caddy generates its CA at startup when the PKI app is configured.
        // Give it a moment to initialize.
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if root_cert.exists() {
                break;
            }
        }
        if !root_cert.exists() {
            anyhow::bail!(
                "Caddy CA not generated at {}. Is Caddy running?",
                root_cert.display()
            );
        }
    }

    // In CI environments, skip CA trust — it can't work (no keychain access,
    // no GUI prompts) and tests use curl -k anyway.
    if std::env::var("CI").is_ok() {
        return Ok(StepResult::success(
            "Caddy CA generated (skipping trust in CI environment)",
        ));
    }

    match std::env::consts::OS {
        "macos" => {
            // Add to the user login keychain with SSL trust policy.
            // Use a timeout and pipe stdin from /dev/null to prevent interactive
            // password prompts from hanging in headless environments.
            let keychain = dirs::home_dir()
                .context("could not determine home directory")?
                .join("Library/Keychains/login.keychain-db");

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                Command::new("security")
                    .args(["add-trusted-cert", "-p", "ssl", "-k"])
                    .arg(&keychain)
                    .arg(&root_cert)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
            )
            .await;

            match result {
                Ok(Ok(status)) if status.success() => {}
                Ok(Ok(_)) => {
                    return Ok(StepResult::success(
                        "Caddy CA generated (could not add to keychain — run with sudo or add manually)",
                    ));
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "failed to run security add-trusted-cert");
                    return Ok(StepResult::success(
                        "Caddy CA generated (could not add to keychain — add manually)",
                    ));
                }
                Err(_) => {
                    // Timeout — likely an interactive password prompt.
                    tracing::warn!("security add-trusted-cert timed out (interactive prompt?)");
                    return Ok(StepResult::success(
                        "Caddy CA generated (trust command timed out — add manually if needed)",
                    ));
                }
            }
        }
        "linux" => {
            let ca_dir = PathBuf::from("/usr/local/share/ca-certificates");
            let dest = ca_dir.join("veld-caddy-ca.crt");
            if std::fs::create_dir_all(&ca_dir)
                .and_then(|_| std::fs::copy(&root_cert, &dest).map(|_| ()))
                .is_err()
            {
                return Ok(StepResult::success(
                    "Caddy CA generated (could not copy to ca-certificates — run with sudo or add manually)",
                ));
            }
            let _ = Command::new("update-ca-certificates").status().await;
        }
        other => {
            return Ok(StepResult::success(format!(
                "Caddy CA generated (automatic trust not supported on {other} — add manually)"
            )));
        }
    }

    Ok(StepResult::success(
        "Caddy CA trusted in system store (browsers will accept HTTPS)",
    ))
}

/// Install (or verify) the Veld daemon.
///
/// The daemon is a user-level LaunchAgent, so on macOS it must be loaded
/// by the real user — not root. When running under `sudo`, we use
/// `SUDO_USER` / `SUDO_UID` to target the correct user and home directory,
/// and `launchctl asuser <uid>` to load the agent in their session.
pub async fn install_daemon() -> Result<StepResult, anyhow::Error> {
    let veld_daemon_bin = which_self("veld-daemon")?;

    match std::env::consts::OS {
        "macos" => {
            // Resolve the real (non-root) user's home and UID. When running
            // under sudo, HOME and `id -u` reflect root — use SUDO_USER instead.
            let (real_user, real_uid, real_home) = resolve_real_user_macos()?;

            let plist_dir = real_home.join("Library/LaunchAgents");
            std::fs::create_dir_all(&plist_dir)
                .context("failed to create LaunchAgents directory")?;
            let plist_path = plist_dir.join("dev.veld.daemon.plist");

            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.veld.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#,
                veld_daemon_bin.display()
            );
            let label = "dev.veld.daemon";
            let domain_target = format!("gui/{real_uid}/{label}");
            let domain = format!("gui/{real_uid}");

            // Stop the running service first (required for upgrades).
            let _ = Command::new("launchctl")
                .args(["bootout", &domain_target])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;

            std::fs::write(&plist_path, &plist)
                .context("failed to write daemon LaunchAgent plist")?;

            // Fix ownership so the user (not root) owns the plist.
            let _ = Command::new("chown")
                .args([
                    format!("{real_user}:staff"),
                    plist_path.to_string_lossy().to_string(),
                ])
                .status()
                .await;

            // Load the agent as the real user via `launchctl asuser <uid>`.
            // This works even when the current process is root.
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                Command::new("launchctl")
                    .args([
                        "asuser",
                        &real_uid,
                        "launchctl",
                        "bootstrap",
                        &domain,
                        &plist_path.to_string_lossy(),
                    ])
                    .stdin(std::process::Stdio::null())
                    .status(),
            )
            .await;
            match result {
                Ok(Ok(status)) if status.success() => {}
                Ok(Ok(status)) if status.code() == Some(37) => {
                    // Already loaded — kickstart to restart with new binary.
                    let _ = Command::new("launchctl")
                        .args(["kickstart", "-k", &domain_target])
                        .stdin(std::process::Stdio::null())
                        .status()
                        .await;
                }
                _ => {
                    // bootstrap failed (e.g. no GUI domain in CI/SSH).
                    // Fall back to `launchctl asuser <uid> launchctl load`.
                    let _ = Command::new("launchctl")
                        .args([
                            "asuser",
                            &real_uid,
                            "launchctl",
                            "load",
                            "-w",
                            &plist_path.to_string_lossy(),
                        ])
                        .stdin(std::process::Stdio::null())
                        .status()
                        .await;
                }
            }
        }
        "linux" => {
            let unit_dir = dirs::home_dir()
                .context("could not determine home directory")?
                .join(".config/systemd/user");
            std::fs::create_dir_all(&unit_dir).context("failed to create systemd user unit dir")?;

            let unit_path = unit_dir.join("veld-daemon.service");
            let unit = format!(
                "[Unit]\nDescription=Veld Daemon\n\n[Service]\nExecStart={}\nRestart=always\n\n[Install]\nWantedBy=default.target\n",
                veld_daemon_bin.display()
            );
            std::fs::write(&unit_path, unit).context("failed to write daemon systemd unit")?;

            run_cmd("systemctl", &["--user", "daemon-reload"]).await?;
            // restart to pick up new binary on upgrades.
            let _ = run_cmd("systemctl", &["--user", "restart", "veld-daemon"]).await;
            run_cmd("systemctl", &["--user", "enable", "--now", "veld-daemon"]).await?;
        }
        other => anyhow::bail!("unsupported OS: {other}"),
    }

    Ok(StepResult::success(
        "veld-daemon service installed and started",
    ))
}

/// Install (or verify) the Veld helper, then verify it is reachable and
/// start Caddy through it.
pub async fn install_helper() -> Result<StepResult, anyhow::Error> {
    let veld_helper_bin = which_self("veld-helper")?;
    let socket = crate::helper::default_socket_path();

    // Try to register as a system service. If launchctl/systemctl fails
    // (e.g. in CI), fall back to starting the helper directly.
    let service_ok = match std::env::consts::OS {
        "macos" => install_helper_macos(&veld_helper_bin).await.is_ok(),
        "linux" => install_helper_linux(&veld_helper_bin).await.is_ok(),
        other => anyhow::bail!("unsupported OS: {other}"),
    };

    // Give the service a moment to start.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Check if the helper is reachable.
    let client = HelperClient::new(&socket);
    let helper_up = client.status().await.is_ok();

    if !helper_up {
        // Service registration may have failed or the daemon hasn't started
        // yet. Start the helper directly as a background process.
        tracing::info!("helper not reachable via service manager, starting directly");
        let _child = tokio::process::Command::new(&veld_helper_bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("failed to spawn veld-helper directly")?;

        // Wait for the socket to appear.
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if client.status().await.is_ok() {
                break;
            }
        }

        if client.status().await.is_err() {
            anyhow::bail!("veld-helper failed to start — socket not reachable");
        }
    }

    // Start Caddy via the helper (with timeout — Caddy startup waits for
    // the admin API internally, so give it a generous window).
    match tokio::time::timeout(std::time::Duration::from_secs(30), client.caddy_start()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "could not start Caddy via helper (may already be running)");
        }
        Err(_) => {
            tracing::warn!("caddy_start RPC timed out (Caddy may still be starting)");
        }
    }

    let via = if service_ok {
        "service registered and running"
    } else {
        "started directly (service registration skipped)"
    };
    Ok(StepResult::success(format!(
        "veld-helper {via}, Caddy started"
    )))
}

async fn install_helper_macos(bin: &Path) -> Result<(), anyhow::Error> {
    let plist_path = Path::new("/Library/LaunchDaemons/dev.veld.helper.plist");
    let label = "dev.veld.helper";
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#,
        bin.display()
    );

    // Stop the running service first (required for upgrades). Use the modern
    // `bootout` API — the legacy `unload` is deprecated and unreliable for
    // system-domain LaunchDaemons.
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("system/{label}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    std::fs::write(plist_path, plist).context("failed to write helper LaunchDaemon plist")?;

    // Register and start via the modern `bootstrap` API.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new("launchctl")
            .args(["bootstrap", "system", &plist_path.to_string_lossy()])
            .stdin(std::process::Stdio::null())
            .status(),
    )
    .await;

    match result {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => {
            // bootstrap returns 37 if already loaded — try kickstart instead.
            if status.code() == Some(37) {
                let _ = Command::new("launchctl")
                    .args(["kickstart", "-k", &format!("system/{label}")])
                    .stdin(std::process::Stdio::null())
                    .status()
                    .await;
                Ok(())
            } else {
                anyhow::bail!(
                    "launchctl bootstrap failed for veld-helper (exit {})",
                    status.code().unwrap_or(-1)
                )
            }
        }
        Ok(Err(e)) => Err(e.into()),
        Err(_) => anyhow::bail!("launchctl bootstrap timed out for veld-helper"),
    }
}

async fn install_helper_linux(bin: &Path) -> Result<(), anyhow::Error> {
    let unit_path = Path::new("/etc/systemd/system/veld-helper.service");
    let unit = format!(
        "[Unit]\nDescription=Veld Helper\n\n[Service]\nExecStart={}\nRestart=always\n\n[Install]\nWantedBy=multi-user.target\n",
        bin.display()
    );
    std::fs::write(unit_path, unit).context("failed to write helper systemd unit")?;

    run_cmd("systemctl", &["daemon-reload"]).await?;
    // restart (not just enable) to pick up new binary on upgrades.
    let _ = run_cmd("systemctl", &["restart", "veld-helper"]).await;
    run_cmd("systemctl", &["enable", "--now", "veld-helper"]).await?;
    Ok(())
}

const GITHUB_REPO: &str = "prosperity-solutions/veld";

/// Check for available updates. Returns `Some(version)` if a newer version
/// exists on GitHub releases, or `None` if we're already up to date.
pub async fn check_update() -> Result<Option<String>, anyhow::Error> {
    let current = env!("CARGO_PKG_VERSION");

    let client = reqwest::Client::builder()
        .user_agent(format!("veld/{current}"))
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;

    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("failed to fetch latest release from GitHub")?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "GitHub API returned status {} when checking for updates",
            resp.status()
        );
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse GitHub release response")?;

    let tag = body["tag_name"]
        .as_str()
        .context("GitHub release missing tag_name")?;

    let latest = tag.strip_prefix('v').unwrap_or(tag);

    if is_newer(latest, current) {
        Ok(Some(latest.to_owned()))
    } else {
        Ok(None)
    }
}

/// Compare two semver-like version strings. Returns true if `latest` is
/// newer than `current`.
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> (u64, u64, u64) {
        let mut parts = v.split('.');
        let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    };
    parse(latest) > parse(current)
}

/// Download and run the install script to update to the given version.
pub async fn perform_update(version: &str) -> Result<(), anyhow::Error> {
    let install_url = format!("https://raw.githubusercontent.com/{GITHUB_REPO}/main/install.sh");

    let client = reqwest::Client::builder()
        .user_agent(format!("veld/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")?;

    let script = client
        .get(&install_url)
        .send()
        .await
        .context("failed to download install script")?
        .text()
        .await
        .context("failed to read install script")?;

    // Run the install script with the target version pinned, in
    // non-interactive mode (skip the `veld setup` prompt at the end).
    let status = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("VELD_VERSION", version)
        .env("VELD_NON_INTERACTIVE", "1")
        .status()
        .await
        .context("failed to execute install script")?;

    if !status.success() {
        anyhow::bail!(
            "install script exited with code {}",
            status.code().unwrap_or(-1)
        );
    }

    Ok(())
}

/// Uninstall Veld from this machine.
pub async fn uninstall() -> Result<(), anyhow::Error> {
    match std::env::consts::OS {
        "macos" => {
            // Stop and remove helper (system daemon).
            let helper_plist = "/Library/LaunchDaemons/dev.veld.helper.plist";
            let _ = Command::new("launchctl")
                .args(["bootout", "system/dev.veld.helper"])
                .status()
                .await;
            let _ = std::fs::remove_file(helper_plist);

            // Stop and remove daemon (user agent). Use resolve_real_user_macos
            // so uninstall works correctly when running under sudo.
            if let Ok((_user, uid, home)) = resolve_real_user_macos() {
                let _ = Command::new("launchctl")
                    .args(["bootout", &format!("gui/{uid}/dev.veld.daemon")])
                    .status()
                    .await;
                let daemon_plist = home.join("Library/LaunchAgents/dev.veld.daemon.plist");
                let _ = std::fs::remove_file(&daemon_plist);
            }
        }
        "linux" => {
            // Stop and disable helper (system service).
            let _ = Command::new("systemctl")
                .args(["stop", "veld-helper"])
                .status()
                .await;
            let _ = Command::new("systemctl")
                .args(["disable", "veld-helper"])
                .status()
                .await;
            let _ = std::fs::remove_file("/etc/systemd/system/veld-helper.service");

            // Stop and disable daemon (user service).
            let _ = Command::new("systemctl")
                .args(["--user", "stop", "veld-daemon"])
                .status()
                .await;
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "veld-daemon"])
                .status()
                .await;
            if let Some(home) = dirs::home_dir() {
                let _ = std::fs::remove_file(home.join(".config/systemd/user/veld-daemon.service"));
            }
        }
        _ => {}
    }

    // Remove Caddy CA from system trust store.
    remove_caddy_ca_trust().await;

    // Remove veld library directory (check both possible locations).
    for lib_dir in &[
        PathBuf::from("/usr/local/lib/veld"),
        dirs::home_dir()
            .map(|h| h.join(".local").join("lib").join("veld"))
            .unwrap_or_default(),
    ] {
        if lib_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(lib_dir) {
                tracing::warn!(path = %lib_dir.display(), error = %e, "failed to remove lib dir");
            }
        }
    }

    // Remove daemon socket.
    let socket = helper::default_socket_path();
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }

    // Remove Hammerspoon integration (best-effort).
    uninstall_hammerspoon().await;

    Ok(())
}

/// Remove the Hammerspoon integration (best-effort, never fails).
async fn uninstall_hammerspoon() {
    let home = match resolve_real_user_macos() {
        Ok((_, _, h)) => h,
        Err(_) => return,
    };

    let lua_path = home.join(".hammerspoon/menu/veld.lua");
    if lua_path.exists() {
        let _ = std::fs::remove_file(&lua_path);
    }

    // Remove require("menu.veld") from workspace-manager.lua or init.lua.
    for config_file in &[
        home.join(".hammerspoon/workspace-manager.lua"),
        home.join(".hammerspoon/init.lua"),
    ] {
        if let Ok(content) = std::fs::read_to_string(config_file) {
            if content.contains("menu.veld") {
                let cleaned: String = content
                    .lines()
                    .filter(|line| !line.contains("menu.veld"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = std::fs::write(config_file, cleaned + "\n");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return the (os, arch) pair for download URLs.
fn platform_pair() -> Result<(&'static str, &'static str), anyhow::Error> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        other => anyhow::bail!("unsupported OS: {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => anyhow::bail!("unsupported architecture: {other}"),
    };
    Ok((os, arch))
}

/// Download a binary from `url` to `dest` and make it executable.
async fn download_binary(url: &str, dest: &Path) -> Result<(), anyhow::Error> {
    let response = reqwest::get(url)
        .await
        .context("HTTP request failed")?
        .error_for_status()
        .context("download returned non-success status")?;

    let bytes = response
        .bytes()
        .await
        .context("failed to read response body")?;
    std::fs::write(dest, &bytes).context("failed to write binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
            .context("failed to set executable permissions")?;
    }

    Ok(())
}

/// Locate a sibling binary (e.g. veld-helper) next to the current executable,
/// or in the veld lib directory.
fn which_self(name: &str) -> Result<PathBuf, anyhow::Error> {
    let current = std::env::current_exe().context("cannot determine current executable path")?;
    let dir = current
        .parent()
        .context("executable has no parent directory")?;
    // Check next to the current binary (e.g. target/debug/).
    let candidate = dir.join(name);
    if candidate.exists() {
        return Ok(candidate);
    }
    // Check in the veld lib directory (install.sh puts helper/daemon there).
    let lib_candidate = crate::paths::lib_dir().join(name);
    if lib_candidate.exists() {
        return Ok(lib_candidate);
    }
    // Fall back to PATH lookup.
    Ok(PathBuf::from(name))
}

/// Remove the Caddy CA from the system trust store (best-effort).
async fn remove_caddy_ca_trust() {
    // Try both possible caddy-data locations.
    let candidates = [
        PathBuf::from("/usr/local/lib/veld/caddy-data"),
        dirs::home_dir()
            .map(|h| h.join(".local/lib/veld/caddy-data"))
            .unwrap_or_default(),
    ];

    for data_dir in &candidates {
        let root_cert = data_dir
            .join("pki")
            .join("authorities")
            .join("local")
            .join("root.crt");
        if !root_cert.exists() {
            continue;
        }

        match std::env::consts::OS {
            "macos" => {
                let _ = Command::new("security")
                    .args(["remove-trusted-cert"])
                    .arg(&root_cert)
                    .status()
                    .await;
            }
            "linux" => {
                let dest = Path::new("/usr/local/share/ca-certificates/veld-caddy-ca.crt");
                if dest.exists() {
                    let _ = std::fs::remove_file(dest);
                    let _ = Command::new("update-ca-certificates").status().await;
                }
            }
            _ => {}
        }
    }
}

/// Resolve the real (non-root) user when running under `sudo` on macOS.
///
/// Returns `(username, uid_string, home_dir)`. When not running as root,
/// simply returns the current user's info.
fn resolve_real_user_macos() -> Result<(String, String, PathBuf), anyhow::Error> {
    // If SUDO_USER is set, we're running under sudo — use the real user.
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        // Get UID via `id -u <username>`
        let uid_output = std::process::Command::new("id")
            .args(["-u", &sudo_user])
            .output()
            .context("failed to run `id -u` for SUDO_USER")?;
        let uid = String::from_utf8_lossy(&uid_output.stdout)
            .trim()
            .to_string();
        if uid.is_empty() || !uid_output.status.success() {
            anyhow::bail!("failed to resolve UID for SUDO_USER={sudo_user}");
        }

        // Get home directory via `dscl`
        let home_output = std::process::Command::new("dscl")
            .args([
                ".",
                "-read",
                &format!("/Users/{sudo_user}"),
                "NFSHomeDirectory",
            ])
            .output()
            .context("failed to run `dscl` for SUDO_USER home directory")?;
        let home_line = String::from_utf8_lossy(&home_output.stdout);
        let home = home_line
            .lines()
            .find_map(|line| {
                line.strip_prefix("NFSHomeDirectory:")
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| format!("/Users/{sudo_user}"));

        return Ok((sudo_user, uid, PathBuf::from(home)));
    }

    // Not running under sudo — use current user info.
    let uid_output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .context("failed to run `id -u`")?;
    let uid = String::from_utf8_lossy(&uid_output.stdout)
        .trim()
        .to_string();

    let user_output = std::process::Command::new("id")
        .arg("-un")
        .output()
        .context("failed to run `id -un`")?;
    let user = String::from_utf8_lossy(&user_output.stdout)
        .trim()
        .to_string();

    let home = dirs::home_dir().context("could not determine home directory")?;

    Ok((user, uid, home))
}

// ---------------------------------------------------------------------------
// Hammerspoon integration (macOS only, optional)
// ---------------------------------------------------------------------------

/// The embedded Lua module for the Hammerspoon menu bar integration.
const HAMMERSPOON_LUA: &str = include_str!("../../../integrations/hammerspoon/veld.lua");

/// Marker we look for / insert in workspace-manager.lua or init.lua.
const HS_REQUIRE: &str = "require(\"menu.veld\")";

/// Install the Hammerspoon menu bar integration if Hammerspoon is present.
///
/// This is best-effort and never fails setup. It:
/// 1. Writes `~/.hammerspoon/menu/veld.lua`
/// 2. Registers the module in workspace-manager.lua (if it exists) or init.lua
/// 3. Reloads Hammerspoon
pub async fn install_hammerspoon() -> Result<StepResult, anyhow::Error> {
    let (_user, _uid, home) = resolve_real_user_macos()?;
    let hs_dir = home.join(".hammerspoon");

    if !hs_dir.exists() {
        return Ok(StepResult::success(
            "Hammerspoon detected but not configured (~/.hammerspoon missing)",
        ));
    }

    // Step 1: Write the Lua module.
    let menu_dir = hs_dir.join("menu");
    std::fs::create_dir_all(&menu_dir).context("failed to create ~/.hammerspoon/menu")?;
    let lua_path = menu_dir.join("veld.lua");
    std::fs::write(&lua_path, HAMMERSPOON_LUA).context("failed to write veld.lua")?;

    // Fix ownership (setup runs as root via sudo).
    fix_owner(&lua_path, &_user);
    fix_owner(&menu_dir, &_user);

    // Step 2: Register the module.
    let ws_manager = hs_dir.join("workspace-manager.lua");
    let init_lua = hs_dir.join("init.lua");

    let registered = if ws_manager.exists() {
        register_in_workspace_manager(&ws_manager)?
    } else if init_lua.exists() {
        register_in_init_lua(&init_lua)?
    } else {
        false
    };

    // Step 3: Reload Hammerspoon.
    reload_hammerspoon(&_uid).await;

    let detail = if registered {
        "installed and registered"
    } else {
        "updated (already registered)"
    };

    Ok(StepResult::success(format!(
        "Hammerspoon menu bar integration {detail}"
    )))
}

/// Register `require("menu.veld")` in workspace-manager.lua's sections table.
fn register_in_workspace_manager(path: &Path) -> Result<bool, anyhow::Error> {
    let content = std::fs::read_to_string(path).context("failed to read workspace-manager.lua")?;

    if content.contains(HS_REQUIRE) {
        return Ok(false); // Already registered.
    }

    // Find the sections table closing brace and insert before it.
    // Pattern: look for the last `}` that closes the `local sections = {` table.
    if let Some(pos) = content.find("local sections = {") {
        // Find the matching closing `}`
        if let Some(close) = content[pos..].find('}') {
            let insert_at = pos + close;
            let new_content = format!(
                "{}    {},\n{}",
                &content[..insert_at],
                HS_REQUIRE,
                &content[insert_at..]
            );
            std::fs::write(path, new_content).context("failed to update workspace-manager.lua")?;
            return Ok(true);
        }
    }

    Ok(false)
}

/// Register a standalone menubar in init.lua as a fallback.
fn register_in_init_lua(path: &Path) -> Result<bool, anyhow::Error> {
    let content = std::fs::read_to_string(path).context("failed to read init.lua")?;

    if content.contains("menu.veld") {
        return Ok(false); // Already registered.
    }

    let snippet = format!(
        "\n-- Veld environment menu bar (installed by veld setup)\n\
         veldMenu = {}:start()\n",
        HS_REQUIRE
    );
    let new_content = format!("{content}{snippet}");
    std::fs::write(path, new_content).context("failed to update init.lua")?;
    Ok(true)
}

/// Fix file ownership to the real user (since setup runs as root).
fn fix_owner(path: &Path, user: &str) {
    let _ = std::process::Command::new("chown")
        .args([
            &format!("{user}:staff"),
            &path.to_string_lossy().to_string(),
        ])
        .output();
}

/// Reload Hammerspoon configuration via the `hs` CLI or AppleScript.
async fn reload_hammerspoon(uid: &str) {
    // Try the `hs` CLI tool first (if user has installed it).
    let result = Command::new("launchctl")
        .args(["asuser", uid, "hs", "-c", "hs.reload()"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    if result.is_ok_and(|s| s.success()) {
        return;
    }

    // Fall back to AppleScript.
    let _ = Command::new("launchctl")
        .args([
            "asuser",
            uid,
            "osascript",
            "-e",
            "tell application \"Hammerspoon\" to execute lua code \"hs.reload()\"",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}

/// Run a command and bail on failure.
async fn run_cmd(program: &str, args: &[&str]) -> Result<(), anyhow::Error> {
    let status = Command::new(program)
        .args(args)
        .status()
        .await
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        anyhow::bail!(
            "{program} {} exited with code {}",
            args.join(" "),
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}
