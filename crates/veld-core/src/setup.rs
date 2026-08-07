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

/// Make the daemon's log file, and answer whether launchd may be pointed at it.
///
/// `None` means "do not name a log in the plist". That is the safe answer, not a
/// degraded one: launchd refuses to exec a job whose `StandardOutPath` it cannot
/// **open** — exit `EX_CONFIG` (78), program never reached, retried forever under
/// `KeepAlive` — so a path this function is unsure about would take the daemon down
/// on machines that work today. No log is what every veld before this shipped.
///
/// Measured, because the distinction decides what this function has to check: a
/// *missing* directory is fine — launchd creates the intervening directories and the
/// file, so `rm -rf ~/.veld` self-heals on the next launch. An **unwritable** one is
/// fatal, which is the whole reason the log does not live beside the binary where a
/// `/usr/local` prefix is root-owned.
///
/// The file is still created here rather than left to launchd, for the one thing
/// launchd will not do: set the mode. Owner-only, because this file exists to collect
/// diagnostics and the next `warn!` someone adds while chasing a share or a
/// machine-var prompt is exactly where a URL or an environment value gets
/// interpolated — the same reason `spawn_stderr_file` keeps captured output
/// owner-only. launchd opens an existing file `O_APPEND` and does not reset its mode.
///
/// Ownership is the trap and the reason for the `chown`: `veld setup` can be running
/// under `sudo`, and a root-owned `0600` log is precisely the unopenable file
/// described above. If the file cannot end up owned by the user the job runs as, it
/// is removed and `None` is returned.
// `cfg(unix)`, not `cfg(target_os = "macos")`, even though only the macOS branch of
// `install_daemon` calls it: that branch is *compiled* on Linux too (it is a runtime
// `match` on `env::consts::OS`, exactly like `resolve_real_user_macos` above), so a
// macOS-only item here is a Linux build failure that no macOS pre-pass can see.
#[cfg(unix)]
fn prepare_daemon_log(
    real_user: &str,
    real_uid: &str,
    real_home: &std::path::Path,
) -> Option<PathBuf> {
    let uid: u32 = real_uid.parse().ok()?;
    let log = crate::paths::daemon_log_path_in(real_home);
    let dir = log.parent()?;

    // The **directory** is checked as carefully as the file, and that is not
    // symmetry for its own sake. Under `sudo veld setup` this is the first thing in
    // the run to touch the *real* user's `~/.veld` (`setup.rs`'s own `create_dir_all`
    // goes through `dirs::home_dir()`, which under sudo is root's), so a `chown` that
    // does not take — an `NFSHomeDirectory` on a network share, the very case
    // `resolve_real_user_macos` consults `dscl` for — would leave `~/.veld` owned by
    // root. That directory is where the user's daemon socket, holder sockets,
    // `pty-<port>/` and `setup.json` live: a far larger break than this log feature
    // not existing. So if it cannot end up owned by the user, undo what we created
    // and give up on the log.
    let we_created_dir = !dir.exists();
    std::fs::create_dir_all(dir).ok()?;
    if we_created_dir {
        chown_to(real_user, dir);
    }
    if !owned_by(dir, uid) {
        if we_created_dir {
            let _ = std::fs::remove_dir(dir);
        }
        return None;
    }

    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .ok()?;
    let _ = crate::paths::set_owner_only(&log);
    chown_to(real_user, &log);
    // Verified, not assumed: `chown` is a best-effort external command and its failure
    // is the one that matters. A root-owned `0600` log is exactly the file launchd
    // cannot open, and a job whose log it cannot open does not run at all.
    if !owned_by(&log, uid) {
        let _ = std::fs::remove_file(&log);
        return None;
    }
    Some(log)
}

/// `chown <user>:staff <path>`, best-effort — every caller verifies the result
/// instead of trusting it.
#[cfg(unix)]
fn chown_to(user: &str, path: &std::path::Path) {
    let _ = std::process::Command::new("chown")
        .args([format!("{user}:staff"), path.display().to_string()])
        .status();
}

/// Whether `path` is owned by `uid`. False when it cannot be read at all, which is
/// the answer that makes every caller refuse rather than assume.
#[cfg(unix)]
fn owned_by(path: &std::path::Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| m.uid() == uid)
        .unwrap_or(false)
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

            // Give the daemon a log, for the reason the helper has one: launchd
            // discards a job's stdout and stderr, so the daemon's own diagnostics —
            // a terminal that would not spawn, a shim directory it could not write,
            // a run whose command was not found — went nowhere, and every message
            // anywhere in veld that said "check the daemon log" pointed at a file
            // that did not exist.
            //
            // **A path the job cannot create is worse than no path**, which is why
            // this is prepared and checked rather than simply formatted in: launchd
            // exits such a job `EX_CONFIG` (78) *before* running the program, and
            // `KeepAlive` turns that into a permanent retry. A daemon that logs
            // nowhere is the status quo; a daemon that never starts is an outage.
            let log_path = prepare_daemon_log(&real_user, &real_uid, &real_home);
            let log_keys = match &log_path {
                Some(p) => format!(
                    "    <key>StandardOutPath</key>\n    <string>{p}</string>\n    \
                     <key>StandardErrorPath</key>\n    <string>{p}</string>\n",
                    p = p.display()
                ),
                // Silent here, said out loud by `veld doctor`'s `Daemon log:` row,
                // which reads the plist and reports "not captured".
                None => String::new(),
            };

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
    <!-- KeepAlive unconditionally true, like the helper's: install.sh restarts this
         job by signalling it to exit and relies on launchd bringing it back onto the
         new binary. A SuccessfulExit or crash-only condition here would make every
         `veld update` leave the daemon dead — the failure that motivated
         `restart_launch_agent`. That function kickstarts as a belt, so this is not
         the only thing holding it up, but it is the intended mechanism. -->
    <key>KeepAlive</key>
    <true/>
    <key>WatchPaths</key>
    <array>
        <string>{bin_path}</string>
    </array>
{log_keys}</dict>
</plist>
"#,
                bin_path = veld_daemon_bin.display(),
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
            // `XDG_CONFIG_HOME` when set, because that is where systemd itself
            // looks for user units — and because `install.sh` resolves the same
            // path when it patches an existing unit. Hardcoding `~/.config` made
            // the two disagree for anyone who sets it, which silently turned that
            // patch into a no-op.
            let unit_dir = match std::env::var("XDG_CONFIG_HOME") {
                Ok(dir) if !dir.is_empty() => PathBuf::from(dir).join("systemd/user"),
                _ => dirs::home_dir()
                    .context("could not determine home directory")?
                    .join(".config/systemd/user"),
            };
            std::fs::create_dir_all(&unit_dir).context("failed to create systemd user unit dir")?;

            let unit_path = unit_dir.join("veld-daemon.service");
            // KillMode=process, for the same reason the helper's unit has it: on
            // stop/restart, kill only the daemon, not its whole cgroup. The
            // daemon's children include one holder process per open terminal,
            // whose entire purpose is to outlive it — under the default
            // control-group mode systemd SIGKILLs every one of them on
            // `systemctl restart`, which is exactly the `veld update` failure the
            // holders exist to prevent. It also stops a restart from taking down
            // runs the daemon started on the user's behalf.
            let unit = format!(
                "[Unit]\nDescription=Veld Daemon\n\n[Service]\nExecStart={}\nRestart=always\nKillMode=process\n\n[Install]\nWantedBy=default.target\n",
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
    let log_path = crate::paths::service_log_path(bin);

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

/// Deterministically restart the privileged (root) helper so it runs a
/// freshly installed binary, without waiting on the helper's own binary
/// watcher.
///
/// The helper's system-domain LaunchDaemon / systemd unit runs as root, so
/// bouncing it needs privilege. `veld update` runs unprivileged, hence sudo:
/// try passwordless first (`sudo -n`, silent and instant when a cached/NOPASSWD
/// credential exists), and only if that fails and `interactive` is set (the
/// caller has a TTY) prompt once for the password. This is the reliable path —
/// it removes the dependence on the ~12s watcher poll + launchd `WatchPaths`
/// that could otherwise leave `veld update` waiting out its timeout and telling
/// the user to re-run `veld setup privileged`.
///
/// Returns `true` if a restart command was successfully issued (the caller
/// should then verify the version flipped), `false` if sudo was unavailable or
/// the attempt failed — in which case the caller falls back to the helper's
/// self-restart watcher.
///
/// The restart is intentionally **graceful**, not a SIGKILL. On macOS it sends
/// SIGTERM (`launchctl kill TERM`), which the helper handles by exiting while
/// leaving Caddy running (see veld-helper `signal_stream`), and launchd's
/// `KeepAlive` then relaunches the new binary. On Linux `systemctl restart`
/// relies on the unit's `KillMode=process` for the same effect. Both keep the
/// Caddy proxy — and therefore every live URL — up across the bounce; a hard
/// `kickstart -k` / default-KillMode restart could take Caddy's process group
/// down with the helper, which would break the "environments keep serving"
/// guarantee this whole change exists to provide.
pub async fn restart_privileged_helper(interactive: bool) -> bool {
    // Build owned args so the label constant is the single source of truth.
    #[cfg(target_os = "macos")]
    let restart_args: Vec<String> = vec![
        "launchctl".to_string(),
        "kill".to_string(),
        "TERM".to_string(),
        format!("system/{HELPER_LABEL_MACOS}"),
    ];
    #[cfg(not(target_os = "macos"))]
    let restart_args: Vec<String> = vec![
        "systemctl".to_string(),
        "restart".to_string(),
        HELPER_SERVICE_LINUX.to_string(),
    ];

    let args: Vec<&str> = restart_args.iter().map(String::as_str).collect();

    // Passwordless first: never prompts, so it's safe to always try.
    if run_sudo(true, &args).await {
        return true;
    }
    // Fall back to an interactive prompt only when the caller says a human is
    // present (a TTY, and not VELD_NON_INTERACTIVE) — a headless/scripted
    // `veld update` must never block on sudo's password prompt.
    if interactive && run_sudo(false, &args).await {
        return true;
    }
    false
}

/// Run `sudo` with `args`. When `noninteractive`, pass `-n` (fail instead of
/// prompting) and swallow output so a probe stays silent; otherwise inherit the
/// terminal so sudo can prompt for the password. Returns whether it exited 0.
async fn run_sudo(noninteractive: bool, args: &[&str]) -> bool {
    let mut cmd = Command::new("sudo");
    if noninteractive {
        cmd.arg("-n");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }
    cmd.args(args);
    matches!(cmd.status().await, Ok(s) if s.success())
}

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
///
/// The CLI half only. `VELD_DESKTOP=0` is explicit rather than left to the
/// script's default, because the default is now "install the app too" and this
/// is not the code path that decides that — `update_desktop_if_stale` is, and it
/// runs afterwards with a version of its own. Leaving it implicit would also let
/// an ambient `VELD_DESKTOP` in the caller's environment decide whether a GUI app
/// gets installed.
pub async fn perform_update(version: &str) -> Result<(), anyhow::Error> {
    run_install_script(version, &[("VELD_DESKTOP".into(), "0".into())], None).await
}

/// What `install_desktop` needs to know. A struct rather than five positional
/// arguments because three of them are `Option`s that mean nothing to a reader at
/// the call site.
#[derive(Debug, Default, Clone)]
pub struct DesktopInstall {
    /// Wait for this process to exit before replacing the bundle — how the app
    /// updates itself, since an Electron app reads from its own bundle while it
    /// runs.
    pub wait_pid: Option<u32>,
    /// Reopen the app when the script is done, whatever the outcome.
    pub relaunch: bool,
    /// The directory holding the bundle to replace, when the caller knows it.
    ///
    /// The app passes its own (`dirname(process.execPath)`'s bundle), because
    /// otherwise the script picks `/Applications` and an app running from
    /// anywhere else gets a *second* copy installed there while the one in the
    /// user's Dock stays stale.
    pub app_dir: Option<PathBuf>,
    /// Where to write the script's output. The app spawns the CLI detached with
    /// no streams, so without this every word the installer says is discarded —
    /// including the reason it failed.
    pub log: Option<PathBuf>,
}

/// Install or update Veld Desktop, the macOS app.
///
/// The same install script does it, for a reason worth stating: a build that a
/// browser downloaded carries `com.apple.quarantine`, which is what makes
/// Gatekeeper refuse the first launch of a build that is not notarized. curl does
/// not set that attribute, so an app delivered by the installer simply opens.
/// Keeping it in the script also means `veld update` keeps the app in step for
/// free, rather than through a second implementation of download-and-verify here.
///
/// Runs with `VELD_DESKTOP_ONLY=1`, which is what makes this an *app* operation:
/// the script skips the CLI tarball, the binary swap, the sudo negotiation, the
/// service restarts and the PATH edits entirely. Without it, updating an app
/// bounced the daemon and could prompt for a password.
pub async fn install_desktop(version: &str, opts: &DesktopInstall) -> Result<(), anyhow::Error> {
    let mut env: Vec<(String, String)> = vec![
        ("VELD_DESKTOP".into(), "1".into()),
        ("VELD_DESKTOP_ONLY".into(), "1".into()),
    ];
    if let Some(pid) = opts.wait_pid {
        env.push(("VELD_DESKTOP_WAIT_PID".into(), pid.to_string()));
    }
    if opts.relaunch {
        env.push(("VELD_DESKTOP_RELAUNCH".into(), "1".into()));
    }
    if let Some(dir) = &opts.app_dir {
        env.push((
            "VELD_DESKTOP_DIR".into(),
            dir.to_string_lossy().into_owned(),
        ));
    }
    run_install_script(version, &env, opts.log.as_deref()).await
}

/// A local install script to run instead of fetching the published one.
///
/// **Debug builds only.** This is a test hook, and it is the one variable that
/// changes *which program* `bash` executes — so a released binary must not read
/// it at all. Veld Desktop spawns the CLI with the app's inherited environment,
/// and an app's environment comes from the user's launchd session
/// (`launchctl setenv`), so in a release build this would be a way to redirect a
/// GUI-initiated install to an arbitrary local file. Setting that variable
/// already requires the ability to run code as the user — but "the attacker
/// could have done it another way" is a reason to keep a hole cheap to close,
/// not a reason to leave it open.
///
/// Also rejects anything that is not an absolute path to an existing file, so a
/// stray or relative value falls back to the real thing rather than silently
/// running something else — or nothing.
fn install_script_override() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    install_script_override_from(std::env::var_os("VELD_INSTALL_SCRIPT"))
}

/// The decision half, split out so it can be tested without touching the
/// process environment.
///
/// Not a style preference: `std::env::set_var` mutates state shared by every
/// thread, which is why Rust 2024 made it `unsafe`, and a unit test that sets a
/// variable while the other tests in its binary run in parallel is a race
/// against every concurrent `getenv` in the process — including the ones inside
/// `dirs`, `reqwest` and tokio. A predicate that takes its input as an argument
/// has nothing to race.
fn install_script_override_from(raw: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let raw = raw.filter(|v| !v.is_empty())?;
    let path = PathBuf::from(raw);
    (path.is_absolute() && path.is_file()).then_some(path)
}

/// Download and run the install script with the given version pinned.
///
/// `log`, when given, receives the script's stdout and stderr instead of this
/// process's.
async fn run_install_script(
    version: &str,
    extra_env: &[(String, String)],
    log: Option<&std::path::Path>,
) -> Result<(), anyhow::Error> {
    let script = match install_script_override() {
        // A *local file path*, never a URL — deliberately, because the whole
        // value of this hook is that it cannot change where code comes from. It
        // reads a file the caller could already have run with `bash` themselves,
        // and it is the only way to exercise the contract between this function
        // and `install.sh`: the published script is what production fetches, so
        // an unreleased change to the script is otherwise unreachable from here.
        // `tests/install_script_contract.rs` is the reason it exists.
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read VELD_INSTALL_SCRIPT {}", path.display()))?,
        None => {
            let install_url = "https://veld.oss.life.li/get".to_string();

            let client = reqwest::Client::builder()
                .user_agent(format!("veld/{}", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(30))
                .build()
                .context("failed to build HTTP client")?;

            client
                .get(&install_url)
                .send()
                .await
                .context("failed to download install script")?
                .text()
                .await
                .context("failed to read install script")?
        }
    };

    // Run the install script with the target version pinned, in
    // non-interactive mode (skip the `veld setup` prompt at the end).
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&script)
        .env("VELD_VERSION", version)
        .env("VELD_NON_INTERACTIVE", "1");

    // The handoff knobs are pure mechanics — which pid to wait for, whether to
    // reopen the app — and never something a user's shell should be able to
    // supply. Clear them, then let `extra_env` put back exactly what this call
    // asked for. `VELD_DESKTOP_DIR` is deliberately NOT in this list: it names
    // where a machine keeps its apps, so an ambient one is a real answer, and a
    // caller that knows better overrides it through `extra_env` below.
    for key in [
        "VELD_DESKTOP_ONLY",
        "VELD_DESKTOP_WAIT_PID",
        "VELD_DESKTOP_RELAUNCH",
        // Not read by the script, but this process was invoked *by* the script's
        // author in dev and may be invoked by the app in production: never let a
        // "which program do I run" variable propagate down a chain of installs.
        "VELD_INSTALL_SCRIPT",
        // `bash -c` sources `$BASH_ENV` *before* the script body, so it is the
        // same class of variable as the one above — it decides what runs — and
        // it arrives from further away: the app forwards its whole inherited
        // environment, and an app's environment comes from the user's launchd
        // session. Setting it already requires running code as the user, so this
        // closes hygiene rather than a hole; it costs one line.
        "BASH_ENV",
    ] {
        cmd.env_remove(key);
    }

    // The script decides **where to install** by asking `command -v veld`, and a
    // PATH that cannot find this binary sends it somewhere else entirely. That is
    // not hypothetical: Veld Desktop spawns the CLI with a deliberate
    // `PATH=/usr/bin:/bin:/usr/sbin:/sbin` (`SAFE_PATH` in `desktop/src/updater.js`,
    // there so a GUI app's arbitrary launchd PATH cannot decide what a subprocess
    // resolves to). Under it, `command -v veld` finds nothing, install.sh's
    // `EXISTING_VELD` block is skipped whole, and `INSTALL_DIR` falls back to
    // `$HOME/.local/bin` — so a machine whose CLI lives in `/opt/homebrew/bin`
    // gets a *second* CLI in `~/.local/bin` and keeps running the old one.
    // Silently, exit 0.
    //
    // **Appended, never prepended.** A terminal `veld update` already has a PATH
    // that finds the right veld, and putting this directory first would let a
    // binary run from one location retarget an install the user's PATH says
    // belongs to another. Appending changes nothing when the inherited PATH can
    // already answer the question, and answers it only when nothing else can.
    if let Ok(dir) = std::env::current_exe().and_then(|exe| {
        exe.parent()
            .map(|d| d.to_path_buf())
            .ok_or_else(|| std::io::Error::other("no parent"))
    }) {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        // Empty components dropped, and that is not tidying. `split_paths("")`
        // yields one *empty* entry, and an empty PATH element means the current
        // working directory — so an unset or empty PATH would come out as
        // `:<exe dir>` and have the install script resolve `curl`, `tar` and
        // `sudo` from whatever directory it happened to be spawned in.
        let mut parts: Vec<PathBuf> = std::env::split_paths(&inherited)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        parts.push(dir);
        match std::env::join_paths(parts) {
            Ok(joined) => cmd.env("PATH", joined),
            // Only reachable if a directory on PATH — or this binary's own —
            // contains a colon, which `join_paths` cannot express. Leaving the
            // inherited PATH would silently reintroduce the wrong-install-dir bug
            // this block exists to prevent, so say so rather than carry on
            // quietly: the log is the only place anyone will see it.
            Err(e) => {
                eprintln!(
                    "Warning: could not extend PATH for the installer ({e}); it may install to \
                     the default location rather than beside this binary."
                );
                &mut cmd
            }
        };
    }

    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    if let Some(path) = log {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Truncated per run on purpose: this is "what happened the last time",
        // read by the app right after it relaunches, not an audit trail.
        let file = std::fs::File::create(path)
            .with_context(|| format!("failed to open install log {}", path.display()))?;
        let err = file
            .try_clone()
            .context("failed to duplicate the install log handle")?;
        cmd.stdout(std::process::Stdio::from(file));
        cmd.stderr(std::process::Stdio::from(err));
    }

    let status = cmd
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

/// Where Veld Desktop is installed, and which version, if it is installed at all.
///
/// Mirrors the install script's own search exactly, including the part that is
/// easy to miss: when `VELD_DESKTOP_DIR` is set it is the *only* place either
/// side looks. A machine that sets it and a CLI that ignored it disagreed about
/// whether the app existed at all — `veld desktop status` said "not installed",
/// `veld update` never refreshed it, and a second copy landed in `/Applications`.
///
/// `app_dir`, when given, overrides both: it is the caller naming the bundle it
/// means (the app passing its own), not a machine-wide preference.
pub fn desktop_app_status_in(
    app_dir: Option<&std::path::Path>,
) -> Option<(PathBuf, Option<String>)> {
    if std::env::consts::OS != "macos" {
        return None;
    }

    let candidates: Vec<PathBuf> = if let Some(dir) = app_dir {
        vec![dir.join("Veld.app")]
    } else if let Some(dir) = std::env::var_os("VELD_DESKTOP_DIR").filter(|v| !v.is_empty()) {
        vec![PathBuf::from(dir).join("Veld.app")]
    } else {
        let mut c = vec![PathBuf::from("/Applications/Veld.app")];
        if let Some(home) = dirs::home_dir() {
            c.push(home.join("Applications/Veld.app"));
        }
        c
    };

    let path = candidates.into_iter().find(|p| p.is_dir())?;
    let plist = path.join("Contents/Info.plist");
    // Read the version with the tool macOS ships rather than a plist crate: this
    // is one field, on macOS only, and the dependency would be carried by every
    // platform's build.
    let version = std::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString"])
        .arg(&plist)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Some((path, version))
}

/// Where Veld Desktop is installed, honouring `VELD_DESKTOP_DIR`.
///
/// macOS only, and that is a deliberate limit rather than an oversight: this
/// answers "which bundle would the installer replace", and veld only installs
/// or replaces the app on macOS. Reporting surfaces that merely want to *show*
/// a Linux install use [`desktop_app_linux`] instead.
pub fn desktop_app_status() -> Option<(PathBuf, Option<String>)> {
    desktop_app_status_in(None)
}

/// The main process of a running Veld Desktop, for the bundle at `bundle`.
///
/// `ps` rather than `pgrep`, and the difference is not stylistic. `pgrep -f`
/// matches a **regex** against a **bounded prefix** of the command line, and
/// install.sh has been bitten by both halves (see the guard in
/// `install_desktop_app`): a destination containing `+` or `.` matches a
/// different set of processes than the one asked about, and a bundle under a deep
/// path hides a bug that a bundle in `/Applications` shows. Reading `ps` output
/// and comparing prefixes in Rust has neither property — there is no pattern to
/// escape and no truncation to fall off the end of.
///
/// Only the main process matches, which is what makes the prefix the right test:
/// Electron's renderer and GPU children run from
/// `…/Contents/Frameworks/Veld Helper.app/…`, so `…/Contents/MacOS/` selects the
/// one process whose exit means the bundle is free to be replaced.
///
/// An empty result means "no process is running from that bundle", not "the app
/// is not installed" — callers must not conflate them.
pub fn desktop_app_pids(bundle: &std::path::Path) -> Vec<u32> {
    if std::env::consts::OS != "macos" {
        return Vec::new();
    }
    // `-ww` disables the width clamp `ps` otherwise applies to the command
    // column; without it a long bundle path is silently cut off and the prefix
    // never matches.
    // `uid=` as well as pid and command: `-a` lists **every user's** processes,
    // and on a machine with fast user switching and a shared `/Applications` that
    // means another account's Veld appears here. Signalling it fails with EPERM,
    // the poll never clears, and this user's app half is permanently `Refused` —
    // by a window they cannot even see. Only this uid's processes can be quit, so
    // only this uid's processes count as running.
    let out = match std::process::Command::new("/bin/ps")
        .args(["-axww", "-o", "uid=,pid=,command="])
        .output()
    {
        Ok(out) if out.status.success() => out,
        // A `ps` that will not run is not evidence that nothing is running, and
        // the callers below treat "no pids" as "safe to replace the bundle". So
        // this is the one place where being wrong is expensive — but there is
        // nothing better to return, and every caller re-checks after the fact by
        // asking whether the installed version actually moved.
        _ => return Vec::new(),
    };
    pids_running_from(
        &String::from_utf8_lossy(&out.stdout),
        bundle,
        nix::unistd::getuid().as_raw(),
    )
}

/// The parsing half of [`desktop_app_pids`], split out so it can be tested
/// against real `ps` output without one.
///
/// Each line is `<uid> <pid> <argv…>`. A process matches when it belongs to
/// `uid` *and* its command line **starts with** `<bundle>/Contents/MacOS/` — a
/// prefix rather than an equality, because argv[0] is followed by the app's own
/// arguments, and a prefix rather than a substring, because the CLI that spawned
/// this carries the same path inside `--app-path` and would otherwise match
/// itself. That exact bug shipped once in install.sh's `pgrep` guard, where it
/// made the app's self-update fail every time by reporting the app as running
/// against its own caller.
fn pids_running_from(ps_output: &str, bundle: &std::path::Path, uid: u32) -> Vec<u32> {
    let prefix = bundle.join("Contents/MacOS/");
    let prefix = prefix.to_string_lossy().into_owned();

    ps_output
        .lines()
        .filter_map(|line| {
            let (owner, rest) = line.trim_start().split_once(char::is_whitespace)?;
            if owner.parse::<u32>().ok()? != uid {
                return None;
            }
            let (pid, command) = rest.trim_start().split_once(char::is_whitespace)?;
            if !command.trim_start().starts_with(&prefix) {
                return None;
            }
            pid.parse::<u32>().ok()
        })
        .collect()
}

/// How [`quit_desktop_app`] went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuitOutcome {
    /// Nothing was running from that bundle; there was nothing to quit.
    NotRunning,
    /// The app is gone and the bundle can be replaced.
    Quit,
    /// It was asked and it is still running — an unsaved-work sheet, a modal, a
    /// hung renderer. The caller must leave the bundle alone.
    Refused,
}

/// Ask Veld Desktop to quit, and wait until it actually has.
///
/// Two mechanisms, in this order, and the order is the whole design:
///
/// 1. An Apple Event (`osascript … to quit`). This is a *polite* quit: the app
///    runs `before-quit`, so window layout and pane state are persisted exactly
///    as they are when the user presses ⌘Q. It works against every version of the
///    app that has ever shipped, which is why it goes first — but it is
///    Automation-TCC-gated, so the first use prompts, and a denial makes it fail.
/// 2. `SIGTERM`, which needs no permission. The app installs a handler that turns
///    it into `app.quit()`, so from this release on it is just as polite as the
///    Apple Event. Against an app that predates the handler it is Node's default
///    disposition — the process dies without running `before-quit`, losing that
///    session's window layout. That is the cost of the fallback and the reason it
///    is the fallback.
///
/// **Never `SIGKILL`.** An app that refuses both is an app with something on
/// screen the user has not answered, and taking it out from under them to install
/// an update is worse than not installing the update.
pub async fn quit_desktop_app(bundle: &std::path::Path, timeout: Duration) -> QuitOutcome {
    let pids = desktop_app_pids(bundle);
    if pids.is_empty() {
        return QuitOutcome::NotRunning;
    }

    // Half the budget for the polite route, the rest for the fallback — rather
    // than spending it all on the first mechanism and leaving the second no time
    // to work. Measured against a real clock rather than handed out twice: the
    // Apple Event *and* the wait for it share this half, so a slow `osascript`
    // eats into its own polling budget instead of extending the total past what
    // this function's caller was promised.
    let started = std::time::Instant::now();
    let half = timeout / 2;
    let remaining = |spent_by: Duration| spent_by.saturating_sub(started.elapsed());

    // The bundle's **path**, not `application id "dev.veld.desktop"`. A machine
    // with two installed copies (`/Applications` and `~/Applications`) has one
    // bundle id and two bundles, and LaunchServices decides which the id means —
    // so an id-addressed quit can close the copy that is *not* being replaced,
    // leaving the target running and an innocent window shut. `open_desktop_app`
    // only ever reopens the bundle this plan named, so that window would not come
    // back either. A path names exactly one app.
    let target = format!(
        "tell application {:?} to quit",
        bundle.to_string_lossy().as_ref()
    );

    // Timed out, and this is what `kill_on_drop` above is for: dropping the
    // future is what kills the child. Sending an Apple Event is Automation-TCC
    // gated, so the first call can sit on a consent prompt, and an app that is
    // not pumping its event loop leaves AppleScript waiting on its own default
    // timeout (~2 minutes) — well past the budget this function promises, and
    // never reaching the SIGTERM fallback the split budget exists to fund.
    let _ = tokio::time::timeout(
        half,
        tokio::process::Command::new("/usr/bin/osascript")
            .args(["-e", &target])
            // Whatever the user's environment says, this needs only what macOS ships.
            .env("PATH", "/usr/bin:/bin")
            .kill_on_drop(true)
            .output(),
    )
    .await;

    if wait_for_exit(bundle, remaining(half)).await {
        return QuitOutcome::Quit;
    }

    // The Apple Event was denied, went to a different copy of the app, or the app
    // is old enough not to answer it. Signal the pids that were actually found
    // running from *this* bundle, which is the part the Apple Event could get
    // wrong.
    for pid in desktop_app_pids(bundle) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }

    if wait_for_exit(bundle, remaining(timeout)).await {
        QuitOutcome::Quit
    } else {
        QuitOutcome::Refused
    }
}

/// Poll until nothing runs from `bundle`, or the budget is spent.
async fn wait_for_exit(bundle: &std::path::Path, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if desktop_app_pids(bundle).is_empty() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Wait for one specific process to exit — the app handing its own update over.
///
/// `kill(pid, 0)` rather than a `ps` scan: the app tells us its pid, and the
/// question here is only whether that process is gone. `ESRCH` is the answer;
/// `EPERM` counts as still-running, since a pid we may not signal is a pid that
/// exists.
///
/// Returns false on timeout, which the caller must treat as "do not touch the
/// bundle" — an app that has not exited is still reading from it.
pub async fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    // `kill(0, …)` means "my own process group", and `kill(-n, …)` means a group
    // too — neither is a process this can wait for, and both would poll until the
    // timeout and then claim the app never quit. Callers filter 0 already; this
    // is the same answer stated where the pid is actually used.
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let target = nix::unistd::Pid::from_raw(pid as i32);
    let start = std::time::Instant::now();
    loop {
        if nix::sys::signal::kill(target, None) == Err(nix::errno::Errno::ESRCH) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Reopen Veld Desktop after an update that had to close it.
///
/// `open` on an app that is already running focuses it rather than starting a
/// second copy, so this is safe to call on a path that is not certain the app is
/// down — which every failure path here is.
pub fn open_desktop_app(app: &std::path::Path) {
    if std::env::consts::OS != "macos" {
        return;
    }
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(app)
        .status();
}

/// The paths electron-builder's `.deb` installs Veld Desktop to.
///
/// `productName: Veld` + `executableName: veld-desktop` (desktop/electron-builder.yml)
/// puts the binary in `/opt/Veld` with a symlink on `PATH`. An **AppImage is not
/// here on purpose**: it is a single file the user saves wherever they like, so
/// there is no location to check and its absence from this list proves nothing.
fn linux_desktop_candidates() -> [&'static str; 3] {
    [
        "/usr/bin/veld-desktop",
        "/usr/local/bin/veld-desktop",
        "/opt/Veld/veld-desktop",
    ]
}

/// Where a Linux Veld Desktop is, if one was installed somewhere findable.
///
/// Report-only. No version: a `.deb`'s version belongs to dpkg and an Electron
/// binary does not answer `--version`, so claiming one would mean guessing.
/// `None` means "not found in the usual places", **not** "not installed" — an
/// AppImage lives wherever the user put it. Callers must phrase it that way.
pub fn desktop_app_linux() -> Option<PathBuf> {
    if std::env::consts::OS != "linux" {
        return None;
    }
    first_existing_file(linux_desktop_candidates().into_iter().map(PathBuf::from))
}

/// First path that is an existing regular file. Split out so the selection can
/// be tested off Linux, where `desktop_app_linux` returns early by design.
fn first_existing_file<I: IntoIterator<Item = PathBuf>>(paths: I) -> Option<PathBuf> {
    paths.into_iter().find(|p| p.is_file())
}

/// `~/.veld/desktop-update.log` — the install script's output from the last app
/// update the CLI ran on the app's behalf.
///
/// `~/.veld` rather than the lib dir, for the same reason the daemon's log moved
/// there: the lib dir is root-owned on a privileged install, and this is written
/// by whichever user is running the app.
pub fn desktop_update_log_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".veld").join("desktop-update.log"))
}

/// `~/.veld/desktop-update.json` — what to tell the user about that update once
/// the app is back on screen.
pub fn desktop_update_report_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".veld").join("desktop-update.json"))
}

/// Leave a note for the app to read when it relaunches.
///
/// The app spawns the CLI detached and quits, so nothing it can await survives to
/// hear the outcome. Without this, every failure of the handoff — a download that
/// 404s, a checksum that does not match, a bundle that could not be replaced —
/// reached the user as an app that reopened on the old version and said nothing.
///
/// Best-effort by design: a report that cannot be written must not turn a
/// successful install into a failed one.
/// Which half of the release a handed-off update was working on when it failed.
///
/// The app's dialog and its retry advice differ, and getting it wrong is worse
/// than saying nothing: telling someone whose *CLI* update failed to run
/// `veld desktop update` would move the app and leave the daemon on the release
/// that actually broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateHalf {
    /// Veld Desktop only — `veld desktop install|update`.
    App,
    /// The whole release, through `veld update`.
    Release,
}

impl UpdateHalf {
    fn as_str(self) -> &'static str {
        match self {
            UpdateHalf::App => "app",
            UpdateHalf::Release => "release",
        }
    }
}

pub fn write_desktop_update_report(version: &str, result: Result<(), &str>, half: UpdateHalf) {
    let Some(path) = desktop_update_report_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let payload = serde_json::json!({
        "version": version,
        "ok": result.is_ok(),
        "error": result.err(),
        "log": desktop_update_log_path().map(|p| p.display().to_string()),
        // Absent in reports written before this field existed, which the app
        // reads as the app-only half — correct, since that was the only thing
        // that could write one.
        "half": half.as_str(),
        "finished_at": chrono::Utc::now().to_rfc3339(),
    });
    let _ = std::fs::write(&path, payload.to_string());
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

    // Remove Veld Desktop.
    //
    // The installer puts it there by default now, so an uninstall that left it
    // behind would leave the most *visible* half of veld on the machine — a Dock
    // icon whose daemon no longer exists — after promising to remove everything.
    // Best-effort: `/Applications` is group-writable by `admin`, so an admin user
    // succeeds without sudo and anyone else keeps an app they can drag to the
    // trash, which is not worth failing an uninstall over.
    if std::env::consts::OS == "macos" {
        if let Some((path, _)) = desktop_app_status() {
            match std::fs::remove_dir_all(&path) {
                // stderr, like every other human status line here: stdout is
                // reserved for machine-readable output (AGENTS.md).
                Ok(()) => eprintln!("Removed {}", path.display()),
                Err(e) => eprintln!(
                    "Could not remove {} ({e}). Drag it to the Trash to finish.",
                    path.display()
                ),
            }
        }
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
        // Hang up terminal holders first. Their PTYs are deliberately not the
        // daemon's children, so booting the daemon out leaves them running — and
        // removing the directory takes away the only way to reach them, so the
        // shells (and whatever is running in them) would survive an uninstall
        // that promises to remove everything, unreachable, until their orphan
        // grace expired. Best-effort by design: a holder that has already gone is
        // a connection refused, which is the normal case.
        hang_up_terminal_holders(&veld_dir);
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

    // The Spoon left over from the old Hammerspoon integration is removed by the
    // caller (`veld uninstall`), not here — the removal has a report attached
    // and this function returns `Result<()>`.

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
// Legacy Hammerspoon Spoon cleanup (macOS only)
// ---------------------------------------------------------------------------

/// What the legacy Hammerspoon cleanup found and did.
#[derive(Debug, Default)]
pub struct LegacyHammerspoonRemoval {
    /// The `Veld.spoon` directory existed and has been removed.
    pub removed: bool,
    /// Path to the user's `init.lua`, when it still loads the Spoon. veld never
    /// edits a user's config, so the caller has to tell them to drop the line —
    /// a `loadSpoon("Veld")` left behind errors on every Hammerspoon reload.
    pub stale_init_lua: Option<PathBuf>,
    /// The files are gone but the running Spoon could not be stopped, so its
    /// menu bar icon survives until Hammerspoon is reloaded. Reporting "removed"
    /// without this would contradict what the user can still see: stopping needs
    /// `/usr/local/bin/hs`, which only exists if they ran `hs.ipc.cliInstall()`.
    pub needs_hammerspoon_reload: bool,
}

/// Remove the Veld Spoon left over from the Hammerspoon menu bar integration
/// veld used to ship (`veld setup hammerspoon`).
///
/// Best-effort and idempotent: a machine that never had the Spoon does nothing
/// and reports nothing. Driven from the CLI by
/// `commands::remove_legacy_hammerspoon`, which owns the user-facing report —
/// both `veld update` arms and `veld uninstall` call it.
///
/// This is a one-shot cleanup with an expiry, but **do not delete it after one
/// release**. `veld update` runs the *old* binary — it installs the new one and
/// then calls its own copy of this step — so the release that carries this code
/// is never the release that runs it. The no-op ("already on the latest
/// version") arm is wired up for exactly that reason, which turns the wait into
/// "any `veld update` after this one lands" rather than "the next version bump".
/// Someone who only ever upgrades with `install.sh` (`curl … | sh`) still never
/// runs it, and removes the Spoon by hand. Give it several releases.
pub async fn remove_legacy_hammerspoon() -> LegacyHammerspoonRemoval {
    if !cfg!(target_os = "macos") {
        return LegacyHammerspoonRemoval::default();
    }
    let Ok((_, uid, home)) = resolve_real_user_macos() else {
        return LegacyHammerspoonRemoval::default();
    };

    // Gate the exec on the same condition `remove_spoon_files` checks, so a
    // machine that never had the Spoon runs no subprocess at all.
    if !spoon_dir_in(&home).exists() {
        return LegacyHammerspoonRemoval::default();
    }

    // Stop the running Spoon first, so the menu bar icon disappears now instead
    // of lingering as a widget backed by deleted files.
    let stopped = stop_running_spoon(&uid).await;

    let mut result = remove_spoon_files(&home);
    result.needs_hammerspoon_reload = result.removed && !stopped;
    result
}

/// `~/.hammerspoon/Spoons/Veld.spoon` under a given home directory.
fn spoon_dir_in(home: &Path) -> PathBuf {
    home.join(".hammerspoon/Spoons/Veld.spoon")
}

/// Ask a running Hammerspoon to stop the Veld Spoon. Returns whether it worked.
///
/// `/usr/local/bin/hs` only exists once the user has run `hs.ipc.cliInstall()`,
/// so a `false` here is ordinary, not an error.
async fn stop_running_spoon(uid: &str) -> bool {
    let stop_lua = r#"if spoon.Veld then spoon.Veld:stop() end"#;
    Command::new("launchctl")
        .args(["asuser", uid, "/usr/local/bin/hs", "-c", stop_lua])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// The filesystem half of the cleanup, parameterised by home directory so it can
/// be exercised against a tempdir. Everything that makes the outer function
/// untestable — the platform gate, the `sudo` user lookup, the `launchctl`
/// exec — stays outside.
fn remove_spoon_files(home: &Path) -> LegacyHammerspoonRemoval {
    let mut result = LegacyHammerspoonRemoval::default();

    let spoon_dir = spoon_dir_in(home);
    if !spoon_dir.exists() {
        return result;
    }

    match std::fs::remove_dir_all(&spoon_dir) {
        Ok(()) => result.removed = true,
        Err(e) => {
            tracing::warn!(path = %spoon_dir.display(), error = %e, "failed to remove Veld.spoon");
            return result;
        }
    }

    // Read `init.lua` only if it is a regular file of sane size. On the
    // uninstall path this runs as root against a path the invoking user
    // controls, and `read_to_string` would block forever opening a FIFO and has
    // no size cap. `metadata` follows symlinks (so an `init.lua` symlinked into
    // a dotfiles repo still counts) but `stat`s rather than opens, so a FIFO
    // answers instead of hanging.
    let init_lua = home.join(".hammerspoon/init.lua");
    let readable = std::fs::metadata(&init_lua)
        .map(|m| m.is_file() && m.len() <= 1024 * 1024)
        .unwrap_or(false);
    let contents = if readable {
        std::fs::read_to_string(&init_lua).unwrap_or_default()
    } else {
        String::new()
    };
    if init_lua_loads_veld_spoon(&contents) {
        result.stale_init_lua = Some(init_lua);
    }

    result
}

/// Whether a Hammerspoon `init.lua` still loads the Veld Spoon.
///
/// Loose on the call form, strict about comments. The old installer matched the
/// two literal spellings it wrote itself, which was fine when the answer only
/// decided whether to offer a patch. Here it decides whether the user is *told*
/// their config points at nothing, so a miss is the failure the warning exists
/// to prevent — and Lua has more call forms than those two: `hs.loadSpoon "Veld"`
/// and `hs.loadSpoon[[Veld]]` both call without parentheses. A `--`-commented
/// line is skipped, because telling someone to delete an already-inert line is
/// the one false positive that wastes their time rather than costing a line of
/// output. Block comments (`--[[ … ]]`) are not parsed; nobody writes those
/// around a single loadSpoon call, and guessing wrong there costs an advisory.
fn init_lua_loads_veld_spoon(contents: &str) -> bool {
    contents
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("--"))
        .any(|line| line.contains("loadSpoon") && line.contains("Veld"))
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

/// Ask every terminal holder under `veld_dir` to end its shell and exit.
///
/// Writes the one frame the wire protocol pins forever: kind `0x83`, empty
/// payload (`crates/veld-daemon/src/pty/wire.rs`). Hand-written rather than shared
/// through a crate, because that stability is the point — any process, of any
/// version, can always tell a holder to stop, and this is the second
/// implementation that relies on it (a refused protocol version is the first).
fn hang_up_terminal_holders(veld_dir: &Path) {
    use std::io::Write;

    let Ok(entries) = std::fs::read_dir(veld_dir) else {
        return;
    };
    for dir in entries.flatten() {
        // One directory per daemon instance, so *every* instance's holders are
        // swept rather than only the current one's — an uninstall removes all of
        // veld.
        if !dir
            .file_name()
            .to_string_lossy()
            .starts_with(crate::instance::PTY_DIR_PREFIX)
        {
            continue;
        }
        let Ok(sockets) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for socket in sockets.flatten() {
            let path = socket.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sock") {
                continue;
            }
            if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&path) {
                let mut frame = vec![0x83u8];
                frame.extend_from_slice(&0u32.to_be_bytes());
                let _ = stream.write_all(&frame);
                let _ = stream.flush();
                tracing::info!(socket = %path.display(), "asked a terminal holder to hang up");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        first_existing_file, init_lua_loads_veld_spoon, install_script_override_from,
        linux_desktop_candidates, parse_launchctl_pid, parse_systemd_main_pid, pids_running_from,
        remove_spoon_files,
    };

    /// This user, in the fixture below.
    const ME: u32 = 501;

    /// Real `ps -axww -o uid=,pid=,command=` output, trimmed to the lines that
    /// decide this. Every one of them is a case that has to come out right before
    /// the installer is allowed to replace a bundle.
    const PS_OUTPUT: &str = "\
    0     1 /sbin/launchd
  501   901 /Applications/Veld.app/Contents/MacOS/Veld
  501   902 /Applications/Veld.app/Contents/Frameworks/Veld Helper.app/Contents/MacOS/Veld Helper --type=renderer
  501   903 /usr/local/bin/veld update --wait-pid 901 --relaunch --app-path /Applications/Veld.app/Contents/MacOS/Veld
  501   904 /Users/x/Applications/Veld.app/Contents/MacOS/Veld
  501   905 /Applications/Veld.app/Contents/MacOS/Veld --some-flag
  502   906 /Applications/Veld.app/Contents/MacOS/Veld
  501 notapid /Applications/Veld.app/Contents/MacOS/Veld
  501   907
";

    /// Pid 0 is a process *group*, not a process.
    ///
    /// `kill(0, sig)` addresses the caller's own group and always succeeds, so
    /// polling it for `ESRCH` never terminates early: the caller would spend its
    /// whole 30s budget and then report that Veld Desktop had not quit — about a
    /// process that never existed. Asserted rather than commented, because the
    /// natural simplification is to delete the guard as a redundant check on a
    /// value "the app never sends".
    #[test]
    fn pid_zero_is_not_a_process_to_wait_for() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let started = std::time::Instant::now();
        assert!(!rt.block_on(super::wait_for_pid_exit(0, Duration::from_secs(30))));
        // Returned on the guard, not by exhausting the budget. Bounded well below
        // the budget rather than near zero: the claim is "it did not poll for 30
        // seconds", and a tighter bound would only add a way for a loaded CI
        // runner to fail a test about something other than timing.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?} — pid 0 should be refused before any polling",
            started.elapsed(),
        );

        // Anything that would wrap into a negative `Pid` is a group too.
        assert!(!rt.block_on(super::wait_for_pid_exit(u32::MAX, Duration::from_secs(30))));
    }

    #[test]
    fn another_users_copy_of_the_app_is_not_this_users_problem() {
        // pid 906 is the same bundle under a different uid — fast user switching
        // with a shared /Applications. Counting it would mean SIGTERMing a
        // process this user may not signal (EPERM), so the poll would never
        // clear and the app half would be permanently `Refused` by a window this
        // user cannot see.
        let pids = pids_running_from(
            PS_OUTPUT,
            std::path::Path::new("/Applications/Veld.app"),
            ME,
        );
        assert!(!pids.contains(&906));
        // …and it is found when it *is* the asking user.
        assert_eq!(
            pids_running_from(
                PS_OUTPUT,
                std::path::Path::new("/Applications/Veld.app"),
                502
            ),
            vec![906],
        );
    }

    #[test]
    fn only_the_main_process_of_the_named_bundle_counts_as_running() {
        let pids = pids_running_from(
            PS_OUTPUT,
            std::path::Path::new("/Applications/Veld.app"),
            ME,
        );

        // 901 is the app. 905 is the app with arguments — a prefix match, because
        // argv[0] is followed by the app's own flags.
        assert_eq!(pids, vec![901, 905]);

        // 902 is Electron's renderer, under Contents/Frameworks. It dies with its
        // parent, so counting it would report the app as running for as long as a
        // helper took to exit.
        assert!(!pids.contains(&902));

        // 903 is the CLI that spawned this, and it carries the bundle path inside
        // `--app-path`. Matching it is not hypothetical: unanchored, install.sh's
        // `pgrep` guard did exactly this and made the app's self-update fail every
        // single time by finding its own caller.
        assert!(!pids.contains(&903));

        // 904 is a *different* copy of the app. Replacing /Applications because
        // something is running from ~/Applications would be the wrong bundle.
        assert!(!pids.contains(&904));
    }

    #[test]
    fn a_second_copy_is_found_by_naming_it_and_not_otherwise() {
        assert_eq!(
            pids_running_from(
                PS_OUTPUT,
                std::path::Path::new("/Users/x/Applications/Veld.app"),
                ME,
            ),
            vec![904],
        );
        // Nothing runs from here, and "no pids" is what lets the installer
        // proceed — so an unrelated path must come back empty rather than
        // borrowing another bundle's answer.
        assert!(pids_running_from(PS_OUTPUT, std::path::Path::new("/opt/Veld.app"), ME).is_empty());
    }

    #[test]
    fn malformed_ps_lines_are_dropped_rather_than_guessed() {
        // A non-numeric pid and a line with no command are both real `ps` output
        // shapes (a header, a truncated read). Neither may become a pid this
        // sends SIGTERM to.
        let pids = pids_running_from(
            PS_OUTPUT,
            std::path::Path::new("/Applications/Veld.app"),
            ME,
        );
        assert!(pids.iter().all(|p| *p == 901 || *p == 905));
        assert!(
            pids_running_from("", std::path::Path::new("/Applications/Veld.app"), ME).is_empty()
        );
    }

    /// The bundle path is used verbatim, and that is the whole reason `ps` is read
    /// here instead of `pgrep -f` being asked a question.
    ///
    /// `pgrep -f` takes a **regex**: a destination containing `+`, `.` or `(` — a
    /// versioned directory, a user named `a.b` — matches a different set of
    /// processes than the one asked about, in either direction. There is no
    /// pattern to escape in a prefix comparison, so a path full of metacharacters
    /// is just a path.
    #[test]
    fn regex_metacharacters_in_the_path_are_not_a_pattern() {
        let ps = "\
  501 601 /Users/a.b/Apps (2)/Veld.app/Contents/MacOS/Veld
  501 602 /Users/axb/AppsX2X/Veld.app/Contents/MacOS/Veld
";
        assert_eq!(
            pids_running_from(ps, std::path::Path::new("/Users/a.b/Apps (2)/Veld.app"), ME),
            vec![601],
        );
    }

    /// The happy path of the log preparation, against a real filesystem.
    ///
    /// Load-bearing because of what the `None` branch means: no log keys in the
    /// plist. If this function started failing for the ordinary case, the daemon
    /// would keep running and silently stop logging, which is precisely the state
    /// this whole change exists to end — so "it returned a path, and that path is
    /// owner-only" is asserted rather than assumed.
    #[test]
    #[cfg(unix)]
    fn the_daemon_log_is_prepared_owner_only() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let home = std::env::temp_dir().join(format!("veld-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        // Our own uid, taken from a file we just made rather than from a crate.
        let probe = home.join("probe");
        std::fs::write(&probe, b"x").unwrap();
        let uid = std::fs::metadata(&probe).unwrap().uid();
        let user = std::env::var("USER").unwrap_or_default();

        let log = super::prepare_daemon_log(&user, &uid.to_string(), &home)
            .expect("a writable home must yield a log path");
        assert_eq!(log, home.join(".veld").join("veld-daemon.log"));
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600,
            "the file launchd will append diagnostics to must not be world-readable"
        );

        // Idempotent: `veld setup` is re-run routinely, and the second run must not
        // fail or lose an existing log.
        std::fs::write(&log, b"an earlier daemon's line\n").unwrap();
        assert_eq!(
            super::prepare_daemon_log(&user, &uid.to_string(), &home).as_ref(),
            Some(&log)
        );
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("earlier daemon"),
            "preparing the log must open it for append, never truncate it"
        );

        // A uid that does not own the directory: the plist must name no log at all,
        // because a log the job cannot open is worse than no log — launchd exits such
        // a job EX_CONFIG without ever running the program. Skipped when the tests
        // themselves run as root, where uid 0 *is* the owner.
        if uid != 0 {
            assert_eq!(super::prepare_daemon_log(&user, "0", &home), None);
            // And it is left alone rather than deleted. The directory check comes
            // first and this call did not create it, so the file belongs to whoever
            // does own that home — refusing to use a log is not a licence to delete
            // somebody's diagnostics.
            assert!(
                log.exists()
                    && std::fs::read_to_string(&log)
                        .unwrap()
                        .contains("earlier daemon"),
                "refusing the log must not destroy it"
            );
        }

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn only_an_existing_absolute_file_overrides_the_published_install_script() {
        // The override exists to make the CLI↔`install.sh` contract testable
        // (`tests/install_script_contract.rs`), so the one thing it must never do
        // is quietly change which code runs on a real machine. Anything it does
        // not recognise falls through to the published script rather than to a
        // path resolved against whatever directory the CLI was started in —
        // which, for a process the desktop app spawned, is `/`.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("install.sh");
        std::fs::write(&real, "#!/usr/bin/env bash\n").unwrap();

        let some = |s: &std::ffi::OsStr| Some(s.to_os_string());
        assert!(install_script_override_from(some(real.as_os_str())).is_some());
        assert!(install_script_override_from(None).is_none());
        assert!(install_script_override_from(some("".as_ref())).is_none());
        assert!(install_script_override_from(some("install.sh".as_ref())).is_none());
        assert!(install_script_override_from(some("./install.sh".as_ref())).is_none());
        assert!(install_script_override_from(some("/nope/does/not/exist.sh".as_ref())).is_none());
        // A directory is not a script; reading one would fail later with a
        // confusing error rather than here with none.
        assert!(install_script_override_from(some(dir.path().as_os_str())).is_none());
    }

    #[test]
    fn the_linux_desktop_candidates_match_what_electron_builder_installs() {
        // desktop/electron-builder.yml: `productName: Veld` +
        // `executableName: veld-desktop`, so the .deb lands in /opt/Veld with a
        // symlink on PATH. Renaming either without updating this list makes
        // `veld desktop status` and `veld doctor` quietly stop seeing the app.
        let c = linux_desktop_candidates();
        assert!(c.contains(&"/opt/Veld/veld-desktop"));
        assert!(c.contains(&"/usr/bin/veld-desktop"));
        // An AppImage is deliberately absent: it is a single file the user saves
        // wherever they like, so there is no path to check and its absence from
        // this list is why `None` must never be reported as "not installed".
        assert!(!c.iter().any(|p| p.contains("AppImage")));
    }

    #[test]
    fn the_first_existing_candidate_wins_and_a_directory_is_not_one() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let real = dir.path().join("veld-desktop");
        let second = dir.path().join("veld-desktop-2");
        std::fs::write(&real, "#!/bin/sh\n").unwrap();
        std::fs::write(&second, "#!/bin/sh\n").unwrap();

        assert_eq!(
            first_existing_file([missing.clone(), real.clone(), second]),
            Some(real.clone()),
            "order must be preserved — the first hit wins"
        );
        assert_eq!(first_existing_file([missing.clone()]), None);
        // A directory named like the binary is not the binary.
        assert_eq!(first_existing_file([dir.path().to_path_buf()]), None);
        assert_eq!(first_existing_file(Vec::new()), None);
    }

    #[test]
    fn init_lua_veld_spoon_detected_in_every_lua_call_form() {
        // What the removed installer used to prepend.
        assert!(init_lua_loads_veld_spoon(
            "hs.loadSpoon(\"Veld\"):start()\n"
        ));
        assert!(init_lua_loads_veld_spoon("hs.loadSpoon('Veld'):start()\n"));
        // Hand-written variants the old two-literal check missed.
        assert!(init_lua_loads_veld_spoon("hs.loadSpoon( \"Veld\" )\n"));
        assert!(init_lua_loads_veld_spoon("hs.loadSpoon \"Veld\"\n"));
        assert!(init_lua_loads_veld_spoon("hs.loadSpoon[[Veld]]\n"));
        // Real files have other lines around it.
        assert!(init_lua_loads_veld_spoon(
            "require(\"hs.ipc\")\n\nhs.loadSpoon(\"Veld\"):start()\nhs.alert(\"ready\")\n"
        ));
    }

    #[test]
    fn init_lua_without_the_veld_spoon_is_not_flagged() {
        assert!(!init_lua_loads_veld_spoon(""));
        assert!(!init_lua_loads_veld_spoon("require(\"hs.ipc\")\n"));
        // Another Spoon, no Veld.
        assert!(!init_lua_loads_veld_spoon(
            "hs.loadSpoon(\"Caffeine\"):start()\n"
        ));
        // Already inert — telling the user to remove it wastes their time.
        assert!(!init_lua_loads_veld_spoon("-- hs.loadSpoon(\"Veld\")\n"));
        assert!(!init_lua_loads_veld_spoon("   --hs.loadSpoon(\"Veld\")\n"));
        // `loadSpoon` and `Veld` on separate lines are unrelated statements.
        assert!(!init_lua_loads_veld_spoon(
            "hs.loadSpoon(\"Caffeine\")\nhs.alert(\"Veld\")\n"
        ));
    }

    /// Build a fake home with a `Veld.spoon` and the given `init.lua` contents.
    fn hammerspoon_home(init_lua: Option<&str>) -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("tempdir");
        let spoon = home.path().join(".hammerspoon/Spoons/Veld.spoon");
        std::fs::create_dir_all(&spoon).expect("create spoon dir");
        std::fs::write(spoon.join("init.lua"), "-- spoon\n").expect("write spoon init.lua");
        if let Some(contents) = init_lua {
            std::fs::write(home.path().join(".hammerspoon/init.lua"), contents)
                .expect("write user init.lua");
        }
        home
    }

    #[test]
    fn remove_spoon_files_deletes_the_spoon_and_reports_a_stale_init_lua() {
        let home = hammerspoon_home(Some("hs.loadSpoon(\"Veld\"):start()\n"));

        let result = remove_spoon_files(home.path());

        assert!(result.removed);
        assert!(!home.path().join(".hammerspoon/Spoons/Veld.spoon").exists());
        assert_eq!(
            result.stale_init_lua,
            Some(home.path().join(".hammerspoon/init.lua"))
        );
        // The user's own config is read, never touched.
        assert!(home.path().join(".hammerspoon/init.lua").exists());
    }

    #[test]
    fn remove_spoon_files_without_a_loadspoon_line_reports_nothing_to_edit() {
        let home = hammerspoon_home(Some("require(\"hs.ipc\")\n"));

        let result = remove_spoon_files(home.path());

        assert!(result.removed);
        assert_eq!(result.stale_init_lua, None);
    }

    #[test]
    fn remove_spoon_files_tolerates_a_missing_init_lua() {
        let home = hammerspoon_home(None);

        let result = remove_spoon_files(home.path());

        assert!(result.removed);
        assert_eq!(result.stale_init_lua, None);
    }

    #[test]
    fn remove_spoon_files_is_a_no_op_without_a_spoon() {
        // A machine that never ran `veld setup hammerspoon` — the common case on
        // every `veld update` from here on.
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".hammerspoon")).expect("create hs dir");
        std::fs::write(
            home.path().join(".hammerspoon/init.lua"),
            "hs.loadSpoon(\"Veld\")\n",
        )
        .expect("write init.lua");

        let result = remove_spoon_files(home.path());

        assert!(!result.removed);
        // Nothing was removed, so there is nothing to tell the user about.
        assert_eq!(result.stale_init_lua, None);
    }

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
