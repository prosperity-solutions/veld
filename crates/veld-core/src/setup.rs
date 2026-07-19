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
    // Try both system and user sockets.
    HelperClient::connect().await.is_ok()
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

/// Ensure a helper is running and reachable. Tries existing sockets first,
/// then auto-bootstraps an unprivileged helper if needed.
pub async fn ensure_helper() -> Result<crate::helper::HelperClient, anyhow::Error> {
    use crate::helper::{HelperClient, system_socket_path, user_socket_path};

    // Migrate caddy-data from system install if needed.
    if let Err(e) = migrate_from_system_install() {
        tracing::warn!(error = %e, "caddy-data migration failed (non-fatal)");
    }

    // If setup.json records an explicit mode, the helper is a managed
    // launchd/systemd service that should already be running. Connect to *that*
    // helper and never silently bootstrap a throwaway auto-helper — doing so
    // used to clobber the persisted mode to "auto" and move every URL to
    // :18443, which is exactly what forced users to re-run `veld setup
    // privileged` after an update. If the service is momentarily down (e.g.
    // launchd is relaunching it after `veld update` replaced the binary), wait
    // for it; if it stays down, surface a clear error instead of downgrading.
    match read_setup_mode().as_deref() {
        Some("privileged") => {
            return connect_managed_helper(&system_socket_path(), "privileged").await;
        }
        Some("unprivileged") => {
            return connect_managed_helper(&user_socket_path(), "unprivileged").await;
        }
        // "auto" or unset — fall through to the auto-bootstrap path below.
        _ => {}
    }

    // Try connecting to an existing helper (system or user socket).
    if let Ok(client) = HelperClient::connect().await {
        return Ok(client);
    }

    // Auto-bootstrap: start a user-level helper.
    eprintln!("Setting up Veld for first use...");

    // Ensure Caddy is installed.
    let caddy = crate::paths::caddy_bin();
    if !caddy.exists() {
        eprintln!("  Downloading Caddy...");
        install_caddy(false)
            .await
            .context("failed to install Caddy during auto-bootstrap")?;
    }

    // Ensure ~/.veld/ directory exists.
    let socket = user_socket_path();
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // Find the helper binary.
    let helper_bin = which_self("veld-helper")?;

    // Spawn the helper as a background process.
    eprintln!("  Starting helper...");
    let _child = std::process::Command::new(&helper_bin)
        .arg("--socket-path")
        .arg(&socket)
        .arg("--https-port")
        .arg("18443")
        .arg("--http-port")
        .arg("18080")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn veld-helper")?;

    // Wait for socket to become available.
    let client = HelperClient::new(&socket);
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if client.status().await.is_ok() {
            break;
        }
    }

    if client.status().await.is_err() {
        anyhow::bail!(
            "veld-helper failed to start — socket not reachable at {}",
            socket.display()
        );
    }

    // Start Caddy via the helper.
    eprintln!("  Starting Caddy...");
    match tokio::time::timeout(std::time::Duration::from_secs(30), client.caddy_start()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "could not start Caddy (may already be running)");
        }
        Err(_) => {
            tracing::warn!("caddy_start timed out");
        }
    }

    // Trust CA (best-effort, non-blocking).
    eprintln!("  Trusting development CA...");
    if let Err(e) = trust_caddy_ca().await {
        tracing::warn!(error = %e, "CA trust failed (HTTPS may show warnings)");
    }

    // Write mode file.
    let veld_dir = socket.parent().unwrap_or(std::path::Path::new("/tmp"));
    let setup_json = veld_dir.join("setup.json");
    let _ = std::fs::write(&setup_json, r#"{"mode":"auto"}"#);

    eprintln!("  Done!");
    eprintln!();

    Ok(client)
}

/// Read the persisted setup mode from `~/.veld/setup.json`, if present.
/// Returns `"privileged"`, `"unprivileged"`, `"auto"`, or `None` when unset.
pub fn read_setup_mode() -> Option<String> {
    let path = dirs::home_dir()?.join(".veld").join("setup.json");
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .get("mode")
        .and_then(|m| m.as_str())
        .map(str::to_owned)
}

/// Connect to a helper that `setup.json` says is a managed service on `socket`.
///
/// Retries for a short bounded window so a `veld start` issued while launchd is
/// relaunching the helper (e.g. right after `veld update` swapped the binary)
/// rides out the gap. Never falls back to bootstrapping an auto-helper — a
/// down managed helper is surfaced as an actionable error, not papered over
/// with a mode downgrade.
async fn connect_managed_helper(
    socket: &std::path::Path,
    mode: &str,
) -> Result<crate::helper::HelperClient, anyhow::Error> {
    use crate::helper::HelperClient;

    // Bounded by wall-clock rather than a fixed attempt count: each
    // `connect_to` is itself capped at 3s (its status check timeout), so a fixed
    // count could block `veld start` for over a minute on a wedged socket.
    // 20s is chosen to ride out a helper self-restart — the binary-change
    // watcher can take ~12s to trigger, plus the launchd/systemd relaunch — so a
    // `veld start` issued mid-restart waits it out instead of bailing early.
    // Usually succeeds on the first attempt.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut announced = false;
    loop {
        if let Ok(client) = HelperClient::connect_to(socket).await {
            if announced {
                eprintln!("  veld-helper is back up.");
            }
            return Ok(client);
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        if !announced {
            eprintln!("Waiting for the {mode} veld-helper to come back up...");
            announced = true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    anyhow::bail!(
        "the {mode} veld-helper is not responding at {}.\n\
         It is managed by launchd/systemd and should restart automatically — \
         check `veld doctor`. If it stays down, re-run `veld setup {mode}`.",
        socket.display()
    )
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

/// Result from the Hammerspoon install step — carries extra info so the CLI
/// can interactively offer to patch `init.lua`.
#[derive(Debug)]
pub struct HammerspoonResult {
    pub message: String,
    /// If `true`, `require("hs.ipc")` is missing from init.lua.
    pub needs_ipc: bool,
    /// If `true`, `hs.loadSpoon("Veld")` is missing from init.lua.
    pub needs_load_spoon: bool,
    /// Path to the user's init.lua (may not exist yet).
    pub init_lua_path: PathBuf,
    /// Real user name (for chown after editing).
    pub user: String,
}

// ---------------------------------------------------------------------------
// Setup steps
// ---------------------------------------------------------------------------

/// Check whether a port has something listening on it.
fn is_port_in_use(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// Check that the required ports (https, http, 2019) are free.
///
/// If Caddy is already running (admin API responds on 2019), all three ports
/// are considered owned by Veld and the check passes — this makes `veld setup`
/// idempotent.
pub async fn check_ports(https_port: u16, http_port: u16) -> Result<StepResult, anyhow::Error> {
    // If our own Caddy is already running, ports are ours — skip the check.
    if is_caddy_running().await {
        return Ok(StepResult::success(
            "Ports in use by Veld's own Caddy (already set up)",
        ));
    }

    let ports = [http_port, https_port, 2019];
    let mut in_use = Vec::new();

    for port in ports {
        if is_port_in_use(port) {
            in_use.push(port);
        }
    }

    if in_use.is_empty() {
        Ok(StepResult::success(format!(
            "Ports {http_port}, {https_port}, and 2019 are available"
        )))
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
    // Check for our sentinel route to verify it's Veld's Caddy, not a foreign one.
    client
        .get("http://localhost:2019/id/veld-sentinel")
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Install, upgrade, or verify the Caddy web server.
///
/// Verify that the Caddy binary is installed. The binary is bundled in the
/// release tarball and copied to `lib_dir()` by the installer — no network
/// download needed.
pub async fn install_caddy(_force: bool) -> Result<StepResult, anyhow::Error> {
    // Migrate caddy-data from system install if needed.
    if let Err(e) = migrate_from_system_install() {
        tracing::warn!(error = %e, "caddy-data migration failed (non-fatal)");
    }

    let caddy = crate::paths::caddy_bin();
    if caddy.exists() {
        return Ok(StepResult::success("Caddy is already installed"));
    }

    anyhow::bail!(
        "Caddy binary not found at {}. Re-run the installer or place the caddy binary at this path.",
        caddy.display()
    );
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
            // Add to the real user's login keychain as a trusted root CA.
            // - When running as root (privileged setup), use `-d` to add to
            //   the admin cert store (persists across sessions, needs root).
            // - When running as the user (unprivileged/auto), skip `-d` and
            //   add to the login keychain only (no sudo needed, browsers
            //   still trust it for the current user).
            // - `-r trustRoot` marks it as a trusted root (not just "present")
            // - We copy the cert to a temp file first because the caddy-data
            //   directory may be owned by root with mode 600, and `security`
            //   may not be able to read it directly.
            let (_, _, real_home) = resolve_real_user_macos()?;
            let keychain = real_home.join("Library/Keychains/login.keychain-db");

            // Check if the CA is already trusted — skip if so (prevents duplicates).
            let already_trusted = Command::new("security")
                .args(["verify-cert", "-c"])
                .arg(&root_cert)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .is_ok_and(|s| s.success());

            if already_trusted {
                return Ok(StepResult::success("Caddy CA already trusted in keychain"));
            }

            let tmp_cert = std::env::temp_dir().join("veld-ca.crt");
            std::fs::copy(&root_cert, &tmp_cert).context("failed to copy CA cert to temp file")?;

            let is_root = std::process::Command::new("id")
                .arg("-u")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
                .unwrap_or(false);
            let mut args = vec!["add-trusted-cert"];
            if is_root {
                // Admin cert store — persists across sessions, needs root.
                args.push("-d");
            }
            args.extend(["-r", "trustRoot", "-k"]);

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                Command::new("security")
                    .args(&args)
                    .arg(&keychain)
                    .arg(&tmp_cert)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
            )
            .await;

            let _ = std::fs::remove_file(&tmp_cert);

            match result {
                Ok(Ok(status)) if status.success() => {}
                Ok(Ok(_)) => {
                    return Ok(StepResult::success(
                        "Caddy CA generated (could not add to keychain — try `veld setup privileged` or add manually)",
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
                    "Caddy CA generated (could not copy to ca-certificates — try `veld setup privileged` or add manually)",
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

    // Make the CA certificate (but NOT the private key) readable by the
    // normal user so `veld doctor` can verify it. In privileged mode Caddy
    // runs as root and creates the pki/ tree with mode 700.
    let ca_dir = crate::paths::caddy_data_dir()
        .join("pki")
        .join("authorities")
        .join("local");
    if ca_dir.exists() {
        // Open up the directory chain so the user can traverse to root.crt.
        let _ = Command::new("chmod")
            .args(["a+x"])
            .arg(crate::paths::caddy_data_dir().join("pki"))
            .status()
            .await;
        let _ = Command::new("chmod")
            .args(["a+x"])
            .arg(
                crate::paths::caddy_data_dir()
                    .join("pki")
                    .join("authorities"),
            )
            .status()
            .await;
        let _ = Command::new("chmod")
            .args(["a+x"])
            .arg(&ca_dir)
            .status()
            .await;
        // Only the public cert — the private key stays root-only.
        let _ = Command::new("chmod")
            .args(["a+r"])
            .arg(ca_dir.join("root.crt"))
            .status()
            .await;
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
        <string>{bin_path}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>WatchPaths</key>
    <array>
        <string>{bin_path}</string>
    </array>
</dict>
</plist>
"#,
                bin_path = veld_daemon_bin.display()
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

            // Same bootout/bootstrap race as the helper: bootout returns
            // before the job is gone, and bootstrapping into that window
            // fails. Wait for the removal to drain.
            let _ =
                wait_for_launchd_job_removal(&domain, label, std::time::Duration::from_secs(10))
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

            // Load the agent as the real user via `launchctl asuser <uid>`
            // (works even when the current process is root), with the shared
            // race-safe choreography: retry-on-drain, kickstart last resort,
            // legacy `load -w` fallback for sessions without a GUI domain
            // (CI/SSH). Soft-fail: the daemon is non-critical for setup and
            // the verification below reports a dead one.
            match bootstrap_launchd_job(&domain, label, &plist_path, Some(&real_uid), true).await {
                Ok(BootstrapOutcome::Bootstrapped) | Ok(BootstrapOutcome::LegacyLoaded) => {}
                Ok(BootstrapOutcome::KickstartedStale) => {
                    eprintln!(
                        "  Warning: could not load the new veld-daemon service definition; \
                         restarted the existing registration instead."
                    );
                }
                Err(e) => {
                    eprintln!("  Warning: could not register veld-daemon with launchd: {e:#}");
                }
            }

            // Soft verification only: CI/SSH sessions have no GUI domain, so a
            // missing job is expected there — but on a real machine this makes
            // a silently-dead daemon visible instead of reporting success.
            if !wait_for_launchd_job_running(&domain, label, std::time::Duration::from_secs(10))
                .await
            {
                eprintln!(
                    "  Warning: launchd does not report veld-daemon running; run `veld doctor` to check."
                );
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

            // Soft verification, mirroring the macOS branch.
            if !wait_for_systemd_running("veld-daemon", true, std::time::Duration::from_secs(10))
                .await
            {
                eprintln!(
                    "  Warning: systemd does not report veld-daemon running; run `veld doctor` to check."
                );
            }
        }
        other => anyhow::bail!("unsupported OS: {other}"),
    }

    Ok(StepResult::success(
        "veld-daemon service installed and started",
    ))
}

/// Install (or verify) the Veld helper using an explicit binary path,
/// then verify it is reachable and start Caddy through it.
///
/// This variant is used by `veld setup privileged` where the binary path
/// was resolved before sudo escalation and passed as an argument.
pub async fn install_helper_with_bin(
    veld_helper_bin: &std::path::Path,
    caddy_bin: Option<&std::path::Path>,
) -> Result<StepResult, anyhow::Error> {
    install_helper_inner(
        veld_helper_bin.to_path_buf(),
        caddy_bin.map(|p| p.to_path_buf()),
    )
    .await
}

/// Install (or verify) the Veld helper, then verify it is reachable and
/// start Caddy through it.
pub async fn install_helper() -> Result<StepResult, anyhow::Error> {
    let veld_helper_bin = which_self("veld-helper")?;
    install_helper_inner(veld_helper_bin, None).await
}

/// Shared implementation for `install_helper` and `install_helper_with_bin`.
async fn install_helper_inner(
    veld_helper_bin: PathBuf,
    caddy_bin: Option<PathBuf>,
) -> Result<StepResult, anyhow::Error> {
    let socket = crate::helper::system_socket_path();

    // Register as a system service. No silent direct-spawn fallback here: a
    // directly-spawned root helper has no service manager behind it, so it
    // dies permanently on the next binary update or reboot — and it can
    // split-brain against a registered KeepAlive job that is still starting.
    // That orphan state is exactly the incident this code path used to cause.
    // `VELD_ALLOW_UNMANAGED_HELPER=1` restores the old fallback for
    // environments with no working service manager (e.g. containers).
    let service_result = match std::env::consts::OS {
        "macos" => install_helper_macos(&veld_helper_bin, caddy_bin.as_deref()).await,
        "linux" => install_helper_linux(&veld_helper_bin, caddy_bin.as_deref()).await,
        other => anyhow::bail!("unsupported OS: {other}"),
    };
    let allow_unmanaged = matches!(
        std::env::var("VELD_ALLOW_UNMANAGED_HELPER")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    );
    let service_ok = match service_result {
        Ok(()) => true,
        Err(e) if allow_unmanaged => {
            eprintln!("  Warning: service registration failed: {e:#}");
            eprintln!("  Starting unmanaged helper (VELD_ALLOW_UNMANAGED_HELPER=1).");
            let _child = std::process::Command::new(&veld_helper_bin)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .context("failed to spawn veld-helper directly")?;
            false
        }
        Err(e) => {
            return Err(e.context(
                "veld-helper service registration failed — an unmanaged helper would die \
                 permanently on the next update, so setup stops here. Re-run `veld setup`; \
                 set VELD_ALLOW_UNMANAGED_HELPER=1 to force a direct spawn anyway",
            ));
        }
    };

    // Wait for the helper (whether launchd/systemd just started it, or the
    // unmanaged spawn above) to serve its socket. 40×250ms = 10s: the service
    // manager already confirmed the process is up, so this only covers socket
    // bind + permission setup, which is near-instant when healthy.
    let client = HelperClient::new(&socket);
    let mut helper_up = false;
    for _ in 0..40 {
        if client.status().await.is_ok() {
            helper_up = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    if !helper_up {
        if service_ok {
            anyhow::bail!(
                "veld-helper service is registered but its socket at {} is not answering",
                socket.display()
            );
        }
        anyhow::bail!(
            "directly-spawned veld-helper did not answer on its socket at {}",
            socket.display()
        );
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
        "started directly (service registration FAILED — helper will not survive reboots or updates; re-run `veld setup`)"
    };
    Ok(StepResult::success(format!(
        "veld-helper {via}, Caddy started"
    )))
}

async fn install_helper_macos(bin: &Path, caddy_bin: Option<&Path>) -> Result<(), anyhow::Error> {
    let plist_path_buf = PathBuf::from(format!(
        "/Library/LaunchDaemons/{}",
        helper_plist_filename()
    ));
    let plist_path = plist_path_buf.as_path();
    let label = HELPER_LABEL_MACOS;

    // Build ProgramArguments with optional --caddy-bin.
    let mut program_args = format!("        <string>{}</string>", bin.display());
    if let Some(caddy) = caddy_bin {
        program_args.push_str(&format!(
            "\n        <string>--caddy-bin</string>\n        <string>{}</string>",
            caddy.display()
        ));
    }

    // Log to a file next to the binary so the self-healing story (watchdog
    // restarts, Caddy recovery, pid adoption) is observable — launchd otherwise
    // discards the helper's stderr, making a post-sleep recovery impossible to
    // diagnose.
    let log_path = bin
        .parent()
        .map(|p| p.join("veld-helper.log"))
        .unwrap_or_else(|| PathBuf::from("/tmp/veld-helper.log"));

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
{program_args}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <!-- KeepAlive must stay unconditionally true: the helper self-updates by
         exit(0) from its binary watcher and relies on launchd relaunching it.
         A SuccessfulExit=false variant would leave it dead after every update. -->
    <key>KeepAlive</key>
    <true/>
    <key>WatchPaths</key>
    <array>
        <string>{bin_path}</string>
    </array>
    <key>StandardOutPath</key>
    <string>{log_path}</string>
    <key>StandardErrorPath</key>
    <string>{log_path}</string>
</dict>
</plist>
"#,
        bin_path = bin.display(),
        log_path = log_path.display(),
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

    // `bootout` can return before launchd finishes tearing the job down.
    // Bootstrapping into that window fails with exit 5, and the kickstart
    // fallback then targets a registration that is being removed — leaving no
    // service loaded at all while every error is swallowed. Wait until the job
    // is actually gone before re-registering.
    let removed =
        wait_for_launchd_job_removal("system", label, std::time::Duration::from_secs(10)).await;
    if !removed {
        tracing::warn!("old {label} job still registered after bootout; bootstrap may conflict");
    }

    std::fs::write(plist_path, plist).context("failed to write helper LaunchDaemon plist")?;

    match bootstrap_launchd_job("system", label, plist_path, None, false).await? {
        BootstrapOutcome::Bootstrapped | BootstrapOutcome::LegacyLoaded => {}
        BootstrapOutcome::KickstartedStale => {
            // User-visible, not just a trace log: the running job uses the OLD
            // plist until the next successful setup.
            eprintln!(
                "  Warning: could not load the new veld-helper service definition; restarted \
                 the existing registration instead. Re-run `veld setup privileged` if helper \
                 settings changed."
            );
        }
    }

    // Don't trust bootstrap/kickstart exit codes alone: verify launchd actually
    // has the job registered and running before reporting success. 20s covers
    // a slow first launch (Gatekeeper verification of the freshly-signed
    // binary) without stalling setup badly when genuinely broken.
    if !wait_for_launchd_job_running("system", label, std::time::Duration::from_secs(20)).await {
        // Registered-but-slow is a transient (launchd will still start it),
        // and an inconclusive query proves nothing — only a definitive
        // "no job" is a hard failure.
        if launchd_job_registered("system", label).await == Some(false) {
            anyhow::bail!(
                "veld-helper service was bootstrapped but launchd does not report it running"
            );
        }
        eprintln!(
            "  Warning: veld-helper is registered but launchd has not reported it running \
             yet; run `veld doctor` to confirm it came up."
        );
    }
    Ok(())
}

/// How a launchd job ended up loaded (see [`bootstrap_launchd_job`]).
#[derive(Debug, PartialEq)]
pub enum BootstrapOutcome {
    /// The new plist was bootstrapped cleanly.
    Bootstrapped,
    /// The new plist could not be loaded; the pre-existing registration was
    /// kickstarted instead and may run a STALE service definition.
    KickstartedStale,
    /// Registered via legacy `launchctl load -w` (no bootstrap-capable
    /// domain, e.g. headless CI/SSH sessions).
    LegacyLoaded,
}

/// Load-and-start a launchd job from `plist_path` with the full race-safe
/// choreography this codebase requires: bootstrap → on exit 5/37 re-drain the
/// old registration and retry (loads the NEW plist) → kickstart the surviving
/// registration only as a last resort → optionally fall back to legacy
/// `load -w` for sessions without a bootstrap-capable domain.
///
/// Callers must have written the plist and booted out + drained the old job
/// first (`wait_for_launchd_job_removal`). `asuser_uid` wraps bootstrap/load
/// in `launchctl asuser <uid>` so a root process can load user-domain agents.
pub async fn bootstrap_launchd_job(
    domain: &str,
    label: &str,
    plist_path: &Path,
    asuser_uid: Option<&str>,
    legacy_load_fallback: bool,
) -> Result<BootstrapOutcome, anyhow::Error> {
    let plist_str = plist_path.to_string_lossy().to_string();
    let bootstrap_args: Vec<String> = match asuser_uid {
        Some(uid) => vec![
            "asuser".into(),
            uid.into(),
            "launchctl".into(),
            "bootstrap".into(),
            domain.into(),
            plist_str.clone(),
        ],
        None => vec!["bootstrap".into(), domain.into(), plist_str.clone()],
    };

    let mut attempt = 0;
    loop {
        attempt += 1;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            Command::new("launchctl")
                .args(&bootstrap_args)
                .stdin(std::process::Stdio::null())
                .status(),
        )
        .await;

        let code = match result {
            Ok(Ok(status)) if status.success() => return Ok(BootstrapOutcome::Bootstrapped),
            Ok(Ok(status)) => status.code().unwrap_or(-1),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("launchctl bootstrap timed out for {label}"),
        };

        // 37 = already loaded; 5 = I/O error from a registration still
        // draining out of launchd. Both can clear once the old job is gone.
        if (code == 37 || code == 5) && attempt == 1 {
            let _ = wait_for_launchd_job_removal(domain, label, std::time::Duration::from_secs(5))
                .await;
            continue;
        }
        if code == 37 || code == 5 {
            tracing::warn!(
                "bootstrap still failing (exit {code}) for {label}; kickstarting the existing \
                 registration (may run a stale plist until the next setup)"
            );
            let kick = Command::new("launchctl")
                .args(["kickstart", "-k", &format!("{domain}/{label}")])
                .stdin(std::process::Stdio::null())
                .status()
                .await;
            if kick.map(|s| s.success()).unwrap_or(false) {
                return Ok(BootstrapOutcome::KickstartedStale);
            }
            if !legacy_load_fallback {
                anyhow::bail!(
                    "launchctl bootstrap failed (exit {code}) and kickstart fallback also failed for {label}"
                );
            }
            // Fall through to legacy load: a missing GUI domain (headless
            // CI/SSH) also surfaces as exit 5, where kickstart has nothing to
            // target but `load -w` can still register the agent.
        }

        // Optionally fall back to legacy `load -w` (headless sessions where
        // the target domain can't be bootstrapped).
        if legacy_load_fallback {
            let load_args: Vec<String> = match asuser_uid {
                Some(uid) => vec![
                    "asuser".into(),
                    uid.into(),
                    "launchctl".into(),
                    "load".into(),
                    "-w".into(),
                    plist_str.clone(),
                ],
                None => vec!["load".into(), "-w".into(), plist_str.clone()],
            };
            let load = Command::new("launchctl")
                .args(&load_args)
                .stdin(std::process::Stdio::null())
                .status()
                .await;
            if load.map(|s| s.success()).unwrap_or(false) {
                return Ok(BootstrapOutcome::LegacyLoaded);
            }
        }
        anyhow::bail!("launchctl bootstrap failed for {label} (exit {code})");
    }
}

/// Poll `launchctl print <domain>/<label>` until it no longer finds the job
/// (bootout finished), or the timeout elapses. Returns true if the job is gone.
pub async fn wait_for_launchd_job_removal(
    domain: &str,
    label: &str,
    timeout: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    loop {
        // Only a clean "not found" proves removal; a failed/timed-out query
        // proves nothing, so keep polling until the deadline.
        if launchd_job_registered(domain, label).await == Some(false) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Whether launchd has a job registered under `domain/label` — `Some(true)`
/// even for a registered-but-not-running job (contrast [`launchd_job_pid`],
/// which requires a live pid). `None` means the query itself failed or timed
/// out ([`SERVICE_QUERY_TIMEOUT`]) and proves nothing — callers must not
/// treat that as "job absent" (a wedged launchctl would otherwise read as an
/// orphaned helper).
pub async fn launchd_job_registered(domain: &str, label: &str) -> Option<bool> {
    let status = tokio::time::timeout(
        SERVICE_QUERY_TIMEOUT,
        Command::new("launchctl")
            .args(["print", &format!("{domain}/{label}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await;
    match status {
        Ok(Ok(s)) => Some(s.success()),
        _ => None,
    }
}

/// Poll systemd until `service` reports a running MainPID, or the timeout
/// elapses. `user_unit` selects the `--user` manager.
pub async fn wait_for_systemd_running(
    service: &str,
    user_unit: bool,
    timeout: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    loop {
        if systemd_main_pid_in(service, user_unit).await.is_some() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Poll `launchctl print <domain>/<label>` until the job reports a running
/// pid, or the timeout elapses.
pub async fn wait_for_launchd_job_running(
    domain: &str,
    label: &str,
    timeout: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    loop {
        if launchd_job_pid(domain, label).await.is_some() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// launchd label of the helper on macOS (system LaunchDaemon in privileged
/// mode, user LaunchAgent in unprivileged mode). The helper's binary watcher,
/// doctor's managed-check, the plists, and uninstall all key on this — a
/// rename must change every consumer atomically or the helper stops
/// recognising itself as managed and auto-update silently dies. Shell copies
/// exist in `install.sh` and `.github/workflows/ci.yml`; update those in
/// lockstep.
pub const HELPER_LABEL_MACOS: &str = "dev.veld.helper";

/// systemd unit name of the helper on Linux. Same lockstep rules as
/// [`HELPER_LABEL_MACOS`].
pub const HELPER_SERVICE_LINUX: &str = "veld-helper";

/// Filename of the helper's launchd plist (shared by the system LaunchDaemon
/// and user LaunchAgent installs), derived from the label so the two cannot
/// drift apart.
pub fn helper_plist_filename() -> String {
    format!("{HELPER_LABEL_MACOS}.plist")
}

/// Upper bound on a single launchctl/systemctl query. These are queried from
/// polling loops, `veld doctor`, and the helper's own binary watcher — a
/// wedged service manager must degrade to "unknown", never hang the caller.
pub const SERVICE_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The MainPID systemd reports for a service, if it exists and is running.
/// `MainPID=0` means not running. `user_unit` selects `systemctl --user`.
pub async fn systemd_main_pid(service: &str) -> Option<u32> {
    systemd_main_pid_in(service, false).await
}

/// See [`systemd_main_pid`]; `user_unit` selects the `--user` manager.
pub async fn systemd_main_pid_in(service: &str, user_unit: bool) -> Option<u32> {
    systemd_pid_query(service, user_unit).await.flatten()
}

/// Three-state systemd query: `None` = systemctl failed/timed out (proves
/// nothing), `Some(None)` = query succeeded but the unit is not running
/// (MainPID=0), `Some(Some(pid))` = running. Callers that need to tell "unit
/// down" apart from "can't tell" (e.g. doctor's orphan check) must use this
/// instead of the flattened [`systemd_main_pid_in`].
pub async fn systemd_pid_query(service: &str, user_unit: bool) -> Option<Option<u32>> {
    let mut args = vec!["show", "-p", "MainPID", "--value", service];
    if user_unit {
        args.insert(0, "--user");
    }
    let out = tokio::time::timeout(
        SERVICE_QUERY_TIMEOUT,
        Command::new("systemctl")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_systemd_main_pid(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// Parse `systemctl show -p MainPID --value` output into a running pid.
/// `0` means the unit exists but is not running.
pub fn parse_systemd_main_pid(output: &str) -> Option<u32> {
    output.trim().parse().ok().filter(|&pid: &u32| pid != 0)
}

/// The pid launchd reports for a job in `domain` (e.g. `system` or
/// `gui/501`), if the job exists and is running.
pub async fn launchd_job_pid(domain: &str, label: &str) -> Option<u32> {
    let out = tokio::time::timeout(
        SERVICE_QUERY_TIMEOUT,
        Command::new("launchctl")
            .args(["print", &format!("{domain}/{label}")])
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_launchctl_pid(&String::from_utf8_lossy(&out.stdout))
}

/// The pid launchd reports for a system-domain job, if the job exists and is
/// running.
pub async fn macos_job_pid(label: &str) -> Option<u32> {
    launchd_job_pid("system", label).await
}

/// Extract the running pid from `launchctl print` output (the `pid = N`
/// line). A registered-but-not-running job (no pid line, or `pid = 0`) is
/// `None` — mirrors the `MainPID=0` filter for systemd.
pub fn parse_launchctl_pid(output: &str) -> Option<u32> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("pid = ") {
            return rest.trim().parse().ok().filter(|&pid| pid != 0);
        }
    }
    None
}

async fn install_helper_linux(bin: &Path, caddy_bin: Option<&Path>) -> Result<(), anyhow::Error> {
    let unit_path_buf = PathBuf::from(format!(
        "/etc/systemd/system/{HELPER_SERVICE_LINUX}.service"
    ));
    let unit_path = unit_path_buf.as_path();
    let mut exec_start = bin.display().to_string();
    if let Some(caddy) = caddy_bin {
        exec_start.push_str(&format!(" --caddy-bin {}", caddy.display()));
    }
    // KillMode=process: on stop/restart, only kill the helper itself, not its
    // whole cgroup. The helper spawns Caddy as a child; the default
    // control-group kill mode would SIGKILL Caddy whenever the helper is
    // restarted (or exits to pick up a new binary), tearing down every URL.
    // Leaving Caddy running mirrors the macOS behavior so a helper restart
    // doesn't drop the proxy.
    let unit = format!(
        "[Unit]\nDescription=Veld Helper\n\n[Service]\nExecStart={exec_start}\nRestart=always\nKillMode=process\n\n[Install]\nWantedBy=multi-user.target\n",
    );
    std::fs::write(unit_path, unit).context("failed to write helper systemd unit")?;

    run_cmd("systemctl", &["daemon-reload"]).await?;
    // restart (not just enable) to pick up new binary on upgrades.
    let _ = run_cmd("systemctl", &["restart", HELPER_SERVICE_LINUX]).await;
    run_cmd("systemctl", &["enable", "--now", HELPER_SERVICE_LINUX]).await?;

    // Mirror the macOS path: don't trust exit codes alone, verify systemd
    // actually reports the service running.
    if !wait_for_systemd_running(
        HELPER_SERVICE_LINUX,
        false,
        std::time::Duration::from_secs(20),
    )
    .await
    {
        anyhow::bail!("veld-helper service was enabled but systemd does not report it running");
    }
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
pub fn is_newer(latest: &str, current: &str) -> bool {
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
    let install_url = "https://veld.oss.life.li/get".to_string();

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
            let helper_plist = format!("/Library/LaunchDaemons/{}", helper_plist_filename());
            let _ = Command::new("launchctl")
                .args(["bootout", &format!("system/{HELPER_LABEL_MACOS}")])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
            let _ = std::fs::remove_file(&helper_plist);

            // Stop and remove daemon (user agent). Use resolve_real_user_macos
            // so uninstall works correctly when running under sudo.
            if let Ok((_user, uid, home)) = resolve_real_user_macos() {
                let _ = Command::new("launchctl")
                    .args(["bootout", &format!("gui/{uid}/dev.veld.daemon")])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
                let daemon_plist = home.join("Library/LaunchAgents/dev.veld.daemon.plist");
                let _ = std::fs::remove_file(&daemon_plist);

                // Stop and remove user-level helper LaunchAgent (unprivileged mode).
                let _ = Command::new("launchctl")
                    .args(["bootout", &format!("gui/{uid}/{HELPER_LABEL_MACOS}")])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
                let helper_agent_plist = home
                    .join("Library/LaunchAgents")
                    .join(helper_plist_filename());
                let _ = std::fs::remove_file(&helper_agent_plist);
            }
        }
        "linux" => {
            // Stop and disable helper (system service).
            let helper_unit = format!("{HELPER_SERVICE_LINUX}.service");
            let _ = Command::new("systemctl")
                .args(["stop", HELPER_SERVICE_LINUX])
                .status()
                .await;
            let _ = Command::new("systemctl")
                .args(["disable", HELPER_SERVICE_LINUX])
                .status()
                .await;
            let _ = std::fs::remove_file(format!("/etc/systemd/system/{helper_unit}"));

            // Stop and disable daemon (user service).
            let _ = Command::new("systemctl")
                .args(["--user", "stop", "veld-daemon"])
                .status()
                .await;
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "veld-daemon"])
                .status()
                .await;
            if let Some(home) = resolve_real_user_home() {
                let _ = std::fs::remove_file(home.join(".config/systemd/user/veld-daemon.service"));

                // Stop and remove user-level helper service (unprivileged mode).
                let _ = Command::new("systemctl")
                    .args(["--user", "stop", HELPER_SERVICE_LINUX])
                    .status()
                    .await;
                let _ = Command::new("systemctl")
                    .args(["--user", "disable", HELPER_SERVICE_LINUX])
                    .status()
                    .await;
                let _ =
                    std::fs::remove_file(home.join(format!(".config/systemd/user/{helper_unit}")));
            }
        }
        _ => {}
    }

    // Remove Caddy CA from system trust store.
    remove_caddy_ca_trust().await;

    // Remove veld library directory (check both possible locations).
    // Use resolve_real_user_home() so we clean the real user's dir under sudo.
    for lib_dir in &[
        PathBuf::from("/usr/local/lib/veld"),
        resolve_real_user_home()
            .map(|h| h.join(".local").join("lib").join("veld"))
            .unwrap_or_default(),
    ] {
        if lib_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(lib_dir) {
                tracing::warn!(path = %lib_dir.display(), error = %e, "failed to remove lib dir");
            }
        }
    }

    // Remove helper sockets (both system and user).
    let socket = helper::system_socket_path();
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }

    // Remove ~/.veld directory — use real user's home when running under sudo.
    if let Some(home) = resolve_real_user_home() {
        let veld_dir = home.join(".veld");
        if veld_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&veld_dir) {
                tracing::warn!(path = %veld_dir.display(), error = %e, "failed to remove .veld dir");
            }
        }
    }

    // Remove the veld data directory: the central database (veld.db +
    // -wal/-shm — it holds secrets: relay tokens, encrypted node outputs),
    // node.key, and any legacy pre-SQLite state files. Derive it from the
    // real user's home (not dirs::data_dir()) so sudo cleans the right one.
    if let Some(home) = resolve_real_user_home() {
        #[cfg(target_os = "macos")]
        let veld_data = home
            .join("Library")
            .join("Application Support")
            .join("veld");
        // Limitation: under sudo, env_reset strips XDG_DATA_HOME, so a user
        // with a custom data home falls back to the default path here and
        // their veld data dir survives a privileged uninstall.
        #[cfg(not(target_os = "macos"))]
        let veld_data = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local").join("share"))
            .join("veld");
        if veld_data.exists() {
            if let Err(e) = std::fs::remove_dir_all(&veld_data) {
                tracing::warn!(path = %veld_data.display(), error = %e, "failed to remove data dir");
            }
        }
    }

    // Remove Hammerspoon Spoon (best-effort).
    uninstall_hammerspoon().await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Locate a sibling binary (e.g. veld-helper) next to the current executable,
/// or in the veld lib directory.
pub fn which_self(name: &str) -> Result<PathBuf, anyhow::Error> {
    // Prefer the canonical lib directory (where install.sh and veld update put
    // helper/daemon binaries). This avoids picking up stale copies that may
    // exist next to the CLI binary (e.g. ~/.local/bin/veld-daemon left over
    // from manual testing or a previous install layout).
    let lib_candidate = crate::paths::lib_dir().join(name);
    if lib_candidate.exists() {
        return Ok(lib_candidate);
    }
    // Fall back to next to the current binary (e.g. target/debug/ during dev).
    let current = std::env::current_exe().context("cannot determine current executable path")?;
    let dir = current
        .parent()
        .context("executable has no parent directory")?;
    let candidate = dir.join(name);
    if candidate.exists() {
        return Ok(candidate);
    }
    // Fall back to PATH lookup.
    Ok(PathBuf::from(name))
}

/// Remove the Caddy CA from the system trust store (best-effort).
async fn remove_caddy_ca_trust() {
    // Try both possible caddy-data locations.
    // Use resolve_real_user_home() so we find the real user's data under sudo.
    let candidates = [
        PathBuf::from("/usr/local/lib/veld/caddy-data"),
        resolve_real_user_home()
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

/// Resolve the real user's home directory, accounting for `sudo`.
///
/// When running under `sudo`, `dirs::home_dir()` returns root's home
/// (`/var/root` on macOS, `/root` on Linux). This helper checks `SUDO_USER`
/// first and returns the real user's home, falling back to `dirs::home_dir()`.
fn resolve_real_user_home() -> Option<PathBuf> {
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        // Under sudo, use the real user's home
        if cfg!(target_os = "macos") {
            return Some(PathBuf::from(format!("/Users/{sudo_user}")));
        } else {
            return Some(PathBuf::from(format!("/home/{sudo_user}")));
        }
    }
    dirs::home_dir()
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
// Hammerspoon Spoon integration (macOS only, optional)
// ---------------------------------------------------------------------------

/// The embedded Spoon init.lua for the Hammerspoon menu bar integration.
const HAMMERSPOON_SPOON_LUA: &str =
    include_str!("../../../integrations/hammerspoon/Veld.spoon/init.lua");

/// Embedded menu bar icons for the Hammerspoon Spoon.
const HAMMERSPOON_ICON_PNG: &[u8] =
    include_bytes!("../../../integrations/hammerspoon/Veld.spoon/icon.png");
const HAMMERSPOON_ICON_2X_PNG: &[u8] =
    include_bytes!("../../../integrations/hammerspoon/Veld.spoon/icon@2x.png");

/// Install the Veld Spoon into ~/.hammerspoon/Spoons/ and load it via `hs` CLI.
///
/// Returns a `HammerspoonResult` with details about what the CLI should prompt
/// the user about (IPC module, loadSpoon line).
pub async fn install_hammerspoon() -> Result<HammerspoonResult, anyhow::Error> {
    let (user, uid, home) = resolve_real_user_macos()?;
    let hs_dir = home.join(".hammerspoon");
    let user_init_lua = hs_dir.join("init.lua");

    if !hs_dir.exists() {
        return Ok(HammerspoonResult {
            message: "Hammerspoon detected but not configured (~/.hammerspoon missing)".into(),
            needs_ipc: false,
            needs_load_spoon: false,
            init_lua_path: user_init_lua,
            user,
        });
    }

    // Write the Spoon to the standard Spoons directory.
    let spoon_dir = hs_dir.join("Spoons").join("Veld.spoon");
    std::fs::create_dir_all(&spoon_dir).context("failed to create Veld.spoon directory")?;
    let init_lua = spoon_dir.join("init.lua");
    std::fs::write(&init_lua, HAMMERSPOON_SPOON_LUA)
        .context("failed to write Veld.spoon/init.lua")?;
    std::fs::write(spoon_dir.join("icon.png"), HAMMERSPOON_ICON_PNG)
        .context("failed to write Veld.spoon/icon.png")?;
    std::fs::write(spoon_dir.join("icon@2x.png"), HAMMERSPOON_ICON_2X_PNG)
        .context("failed to write Veld.spoon/icon@2x.png")?;

    // Fix ownership (setup runs as root via sudo).
    fix_owner_recursive(&spoon_dir, &user);

    // Check what's in the user's init.lua.
    let init_contents = std::fs::read_to_string(&user_init_lua).unwrap_or_default();
    let needs_ipc = !init_contents.contains("hs.ipc");
    let needs_load_spoon = !init_contents.contains("loadSpoon(\"Veld\")")
        && !init_contents.contains("loadSpoon('Veld')");

    // Try to load the Spoon via `hs` CLI (IPC).
    let loaded = load_spoon_via_hs(&uid).await;

    let message = if loaded {
        "Veld.spoon installed and loaded".into()
    } else if needs_ipc {
        "Veld.spoon installed (Hammerspoon IPC not enabled)".into()
    } else {
        "Veld.spoon installed".into()
    };

    Ok(HammerspoonResult {
        message,
        needs_ipc,
        needs_load_spoon,
        init_lua_path: user_init_lua,
        user,
    })
}

/// Patch the user's Hammerspoon init.lua to add IPC and/or Veld Spoon loading.
///
/// Called by the CLI after the user confirms. Prepends the lines at the top of
/// the file to ensure they run early.
pub fn patch_hammerspoon_init_lua(result: &HammerspoonResult) -> Result<(), anyhow::Error> {
    let path = &result.init_lua_path;
    let existing = std::fs::read_to_string(path).unwrap_or_default();

    let mut prepend = String::new();
    if result.needs_ipc {
        prepend.push_str("require(\"hs.ipc\")\n");
    }
    if result.needs_load_spoon {
        prepend.push_str("hs.loadSpoon(\"Veld\"):start()\n");
    }

    if prepend.is_empty() {
        return Ok(());
    }

    // Add a blank line between our additions and existing content.
    let new_contents = if existing.is_empty() {
        prepend
    } else {
        format!("{prepend}\n{existing}")
    };

    std::fs::write(path, &new_contents).context("failed to write Hammerspoon init.lua")?;
    fix_owner_recursive(path.as_ref(), &result.user);

    Ok(())
}

/// Remove the Veld Spoon (best-effort, called during uninstall).
async fn uninstall_hammerspoon() {
    let (_, uid, home) = match resolve_real_user_macos() {
        Ok(t) => t,
        Err(_) => return,
    };

    // Stop the running Spoon so the menu bar icon disappears immediately.
    let stop_lua = r#"if spoon.Veld then spoon.Veld:stop() end"#;
    let _ = Command::new("launchctl")
        .args(["asuser", &uid, "/usr/local/bin/hs", "-c", stop_lua])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    let spoon_dir = home.join(".hammerspoon/Spoons/Veld.spoon");
    if spoon_dir.exists() {
        let _ = std::fs::remove_dir_all(&spoon_dir);
    }
}

/// Load the Veld Spoon in the running Hammerspoon instance via `hs -c`.
async fn load_spoon_via_hs(uid: &str) -> bool {
    let lua_code = r#"hs.loadSpoon("Veld"); spoon.Veld:start()"#;

    // Try direct `hs` CLI first.
    let direct = Command::new("hs")
        .args(["-c", lua_code])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    if direct.is_ok_and(|s| s.success()) {
        return true;
    }

    // Try via launchctl asuser (we're running as root under sudo).
    let via_launchctl = Command::new("launchctl")
        .args(["asuser", uid, "/usr/local/bin/hs", "-c", lua_code])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    via_launchctl.is_ok_and(|s| s.success())
}

/// Recursively fix ownership of a path to the real user.
fn fix_owner_recursive(path: &Path, user: &str) {
    let _ = std::process::Command::new("chown")
        .arg("-R")
        .arg(format!("{user}:staff"))
        .arg(path)
        .output();
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

// ---------------------------------------------------------------------------
// Migration from system-level install
// ---------------------------------------------------------------------------

/// Migrate Caddy data from a previous system-level install (`/usr/local/lib/veld/caddy-data`)
/// to the user-level location (`~/.local/lib/veld/caddy-data`), preserving the CA and
/// certificates so users don't have to re-trust a new root CA.
pub fn migrate_from_system_install() -> Result<(), anyhow::Error> {
    let system_data = PathBuf::from("/usr/local/lib/veld/caddy-data");
    let user_lib = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".local/lib/veld");
    let user_data = user_lib.join("caddy-data");

    if system_data.exists() && !user_data.exists() {
        tracing::info!("Migrating Caddy data from system install...");
        std::fs::create_dir_all(&user_lib)?;
        copy_dir_recursive(&system_data, &user_data)?;
        tracing::info!("Migration complete");
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), anyhow::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        // Never migrate mode-specific runtime state across the system->user
        // move. `caddy.pid` and `veld-routes.json` belong to the *previous*
        // (privileged) helper session; copying them would make the new
        // user/auto helper adopt the root Caddy's pid or replay routes bound to
        // the old mode's ports. Migration exists only to preserve the CA/certs.
        let name = entry.file_name();
        if name == "caddy.pid" || name == "veld-routes.json" {
            continue;
        }
        let ty = entry.file_type()?;
        let dst_path = dst.join(&name);
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_launchctl_pid, parse_systemd_main_pid};

    #[test]
    fn launchctl_pid_running_job() {
        let out = "system/dev.veld.helper = {\n\tactive count = 1\n\tpath = /Library/LaunchDaemons/dev.veld.helper.plist\n\tstate = running\n\n\tprogram = /Users/x/.local/lib/veld/veld-helper\n\tpid = 48490\n\truns = 2\n}\n";
        assert_eq!(parse_launchctl_pid(out), Some(48490));
    }

    #[test]
    fn launchctl_pid_zero_is_not_running() {
        assert_eq!(parse_launchctl_pid("\tpid = 0\n"), None);
    }

    #[test]
    fn launchctl_pid_missing_line_is_not_running() {
        let out = "system/dev.veld.helper = {\n\tstate = not running\n\tlast exit code = 0\n}\n";
        assert_eq!(parse_launchctl_pid(out), None);
    }

    #[test]
    fn launchctl_pid_garbage_is_none() {
        assert_eq!(parse_launchctl_pid("pid = abc\n"), None);
        assert_eq!(parse_launchctl_pid(""), None);
    }

    #[test]
    fn launchctl_pid_ignores_other_pid_like_lines() {
        // "spawn pid" style lines must not match the `pid = ` prefix check.
        let out = "\tspawn pid = 99\n\tpid = 1234\n";
        assert_eq!(parse_launchctl_pid(out), Some(1234));
    }

    #[test]
    fn systemd_main_pid_running() {
        assert_eq!(parse_systemd_main_pid("1234\n"), Some(1234));
    }

    #[test]
    fn systemd_main_pid_zero_is_not_running() {
        assert_eq!(parse_systemd_main_pid("0\n"), None);
    }

    #[test]
    fn systemd_main_pid_garbage_is_none() {
        assert_eq!(parse_systemd_main_pid(""), None);
        assert_eq!(parse_systemd_main_pid("not-a-pid"), None);
    }
}
