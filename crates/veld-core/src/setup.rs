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
    pub mkcert_present: bool,
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
        if !self.mkcert_present {
            missing.push("mkcert".to_owned());
        }
        missing
    }

    pub fn is_complete(&self) -> bool {
        self.helper_running && self.caddy_present && self.mkcert_present
    }
}

// ---------------------------------------------------------------------------
// Check functions
// ---------------------------------------------------------------------------

const CADDY_PATH: &str = "/usr/local/lib/veld/caddy";
const MKCERT_PATH: &str = "/usr/local/lib/veld/mkcert";

/// Probe the system to determine setup status.
pub async fn check_setup() -> SetupStatus {
    let helper_running = check_helper_running().await;
    let caddy_present = PathBuf::from(CADDY_PATH).exists();
    let mkcert_present = PathBuf::from(MKCERT_PATH).exists();

    SetupStatus {
        helper_running,
        caddy_present,
        mkcert_present,
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
// Constants
// ---------------------------------------------------------------------------

const VELD_LIB_DIR: &str = "/usr/local/lib/veld";
const CERTS_DIR: &str = "/usr/local/lib/veld/certs";

// ---------------------------------------------------------------------------
// Setup steps
// ---------------------------------------------------------------------------

/// Check whether a port has something listening on it.
fn is_port_in_use(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// Check that the required ports (80, 443, 2019) are free.
pub async fn check_ports() -> Result<StepResult, anyhow::Error> {
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

/// Install (or verify) the Caddy web server.
pub async fn install_caddy() -> Result<StepResult, anyhow::Error> {
    let caddy = PathBuf::from(CADDY_PATH);
    if caddy.exists() {
        return Ok(StepResult::success("Caddy is already installed"));
    }

    std::fs::create_dir_all(VELD_LIB_DIR).context("failed to create /usr/local/lib/veld")?;

    let (os, arch) = platform_pair()?;
    let version = "2.9.1";
    let url = format!(
        "https://github.com/caddyserver/caddy/releases/download/v{version}/caddy_{version}_{os}_{arch}.tar.gz"
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

/// Install (or verify) mkcert for local TLS certificates.
pub async fn install_mkcert() -> Result<StepResult, anyhow::Error> {
    let mkcert = PathBuf::from(MKCERT_PATH);
    if mkcert.exists() {
        return Ok(StepResult::success("mkcert is already installed"));
    }

    std::fs::create_dir_all(VELD_LIB_DIR).context("failed to create /usr/local/lib/veld")?;

    let (os, arch) = platform_pair()?;
    let version = "1.4.4";
    let url = format!(
        "https://github.com/FiloSottile/mkcert/releases/download/v{version}/mkcert-v{version}-{os}-{arch}"
    );

    download_binary(&url, &mkcert)
        .await
        .context("failed to download mkcert")?;

    Ok(StepResult::success("mkcert downloaded and installed"))
}

/// Generate local TLS certificates via mkcert.
pub async fn generate_certs() -> Result<StepResult, anyhow::Error> {
    let mkcert = MKCERT_PATH;

    // Install the local CA.
    let status = Command::new(mkcert)
        .arg("-install")
        .status()
        .await
        .context("failed to run mkcert -install")?;
    if !status.success() {
        anyhow::bail!(
            "mkcert -install exited with code {}",
            status.code().unwrap_or(-1)
        );
    }

    // Ensure certs directory exists.
    std::fs::create_dir_all(CERTS_DIR).context("failed to create certs directory")?;

    // Generate wildcard certs.
    let cert_file = format!("{CERTS_DIR}/cert.pem");
    let key_file = format!("{CERTS_DIR}/key.pem");

    let status = Command::new(mkcert)
        .args([
            "-cert-file",
            &cert_file,
            "-key-file",
            &key_file,
            "*.localhost",
            "*.*.localhost",
            "*.*.*.localhost",
        ])
        .status()
        .await
        .context("failed to run mkcert for cert generation")?;
    if !status.success() {
        anyhow::bail!(
            "mkcert cert generation exited with code {}",
            status.code().unwrap_or(-1)
        );
    }

    Ok(StepResult::success("TLS certificates generated"))
}

/// Install (or verify) the Veld daemon.
pub async fn install_daemon() -> Result<StepResult, anyhow::Error> {
    let veld_daemon_bin = which_self("veld-daemon")?;

    match std::env::consts::OS {
        "macos" => {
            let plist_path = dirs::home_dir()
                .context("could not determine home directory")?
                .join("Library/LaunchAgents/dev.veld.daemon.plist");
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
            std::fs::write(&plist_path, plist)
                .context("failed to write daemon LaunchAgent plist")?;

            let status = Command::new("launchctl")
                .args(["load", "-w"])
                .arg(&plist_path)
                .status()
                .await
                .context("failed to load daemon LaunchAgent")?;
            if !status.success() {
                anyhow::bail!("launchctl load failed for veld-daemon");
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
            run_cmd("systemctl", &["--user", "enable", "--now", "veld-daemon"]).await?;
        }
        other => anyhow::bail!("unsupported OS: {other}"),
    }

    Ok(StepResult::success(
        "veld-daemon service installed and started",
    ))
}

/// Install (or verify) the Veld helper.
pub async fn install_helper() -> Result<StepResult, anyhow::Error> {
    let veld_helper_bin = which_self("veld-helper")?;

    match std::env::consts::OS {
        "macos" => {
            let plist_path = Path::new("/Library/LaunchDaemons/dev.veld.helper.plist");
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.veld.helper</string>
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
                veld_helper_bin.display()
            );
            std::fs::write(plist_path, plist)
                .context("failed to write helper LaunchDaemon plist")?;

            let status = Command::new("launchctl")
                .args(["load", "-w"])
                .arg(plist_path)
                .status()
                .await
                .context("failed to load helper LaunchDaemon")?;
            if !status.success() {
                anyhow::bail!("launchctl load failed for veld-helper");
            }
        }
        "linux" => {
            let unit_path = Path::new("/etc/systemd/system/veld-helper.service");
            let unit = format!(
                "[Unit]\nDescription=Veld Helper\n\n[Service]\nExecStart={}\nRestart=always\n\n[Install]\nWantedBy=multi-user.target\n",
                veld_helper_bin.display()
            );
            std::fs::write(unit_path, unit).context("failed to write helper systemd unit")?;

            run_cmd("systemctl", &["daemon-reload"]).await?;
            run_cmd("systemctl", &["enable", "--now", "veld-helper"]).await?;
        }
        other => anyhow::bail!("unsupported OS: {other}"),
    }

    Ok(StepResult::success(
        "veld-helper service installed and started",
    ))
}

/// Check for available updates. Returns `Some(version)` if an update exists.
pub async fn check_update() -> Result<Option<String>, anyhow::Error> {
    // Stub: no update mechanism in v0.1.
    Ok(None)
}

/// Download and install the given version.
pub async fn perform_update(_version: &str) -> Result<(), anyhow::Error> {
    // Stub: no update mechanism in v0.1.
    tracing::info!("No update available");
    Ok(())
}

/// Uninstall Veld from this machine.
pub async fn uninstall() -> Result<(), anyhow::Error> {
    match std::env::consts::OS {
        "macos" => {
            // Unload helper (system daemon).
            let helper_plist = "/Library/LaunchDaemons/dev.veld.helper.plist";
            if Path::new(helper_plist).exists() {
                let _ = Command::new("launchctl")
                    .args(["unload", "-w", helper_plist])
                    .status()
                    .await;
                let _ = std::fs::remove_file(helper_plist);
            }

            // Unload daemon (user agent).
            if let Some(home) = dirs::home_dir() {
                let daemon_plist = home.join("Library/LaunchAgents/dev.veld.daemon.plist");
                if daemon_plist.exists() {
                    let _ = Command::new("launchctl")
                        .args(["unload", "-w"])
                        .arg(&daemon_plist)
                        .status()
                        .await;
                    let _ = std::fs::remove_file(&daemon_plist);
                }
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

    // Remove veld library directory.
    let lib_dir = Path::new(VELD_LIB_DIR);
    if lib_dir.exists() {
        std::fs::remove_dir_all(lib_dir).context("failed to remove /usr/local/lib/veld")?;
    }

    // Remove daemon socket.
    let socket = helper::default_socket_path();
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }

    Ok(())
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

/// Locate a sibling binary (e.g. veld-helper) next to the current executable.
fn which_self(name: &str) -> Result<PathBuf, anyhow::Error> {
    let current = std::env::current_exe().context("cannot determine current executable path")?;
    let dir = current
        .parent()
        .context("executable has no parent directory")?;
    let candidate = dir.join(name);
    if candidate.exists() {
        Ok(candidate)
    } else {
        // Fall back to PATH lookup.
        Ok(PathBuf::from(name))
    }
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
