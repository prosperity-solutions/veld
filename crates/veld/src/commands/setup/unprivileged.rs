use anyhow::Context;
use tokio::process::Command;

use crate::output;
use veld_core::helper::HelperClient;

/// `veld setup unprivileged` -- run the unprivileged (no-sudo) setup sequence.
///
/// Installs everything in user space: Caddy in `~/.local/lib/veld/`,
/// helper as a user-level LaunchAgent/systemd unit on ports 8443/8080.
pub async fn run() -> i32 {
    println!("{}", output::bold("Veld Setup (unprivileged)"));
    println!();

    let total = 4;

    // Step 1: Check port availability (8443, 8080, 2019)
    print_step(1, total, "Checking port availability...");
    match veld_core::setup::check_ports(8443, 8080).await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 2: Install Caddy (to ~/.local/lib/veld/caddy)
    print_step(2, total, "Installing Caddy...");
    match veld_core::setup::install_caddy(false).await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 3: Install helper + start Caddy (user-level, ports 8443/8080)
    print_step(3, total, "Starting Veld helper...");
    match install_unprivileged_helper().await {
        Ok(msg) => print_step_ok(&msg),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 4: Trust Caddy CA
    print_step(4, total, "Trusting Caddy CA...");
    match veld_core::setup::trust_caddy_ca().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Write setup mode
    if let Err(e) = write_setup_mode("unprivileged") {
        eprintln!("Warning: could not save setup mode: {e}");
    }

    println!();
    output::print_success("Setup complete! Run `veld start` to get going.");
    println!();
    let tip = "Run `veld setup privileged` for clean URLs without :8443 (one-time sudo)";
    eprintln!("  {} {tip}", output::bold("Tip:"));

    0
}

/// Install the veld-helper as a user-level service (LaunchAgent on macOS,
/// systemd user unit on Linux), then start Caddy through it.
async fn install_unprivileged_helper() -> Result<String, anyhow::Error> {
    let veld_helper_bin = veld_core::setup::which_self("veld-helper")?;
    let socket_path = veld_core::helper::user_socket_path();

    let service_ok = match std::env::consts::OS {
        "macos" => install_helper_launchagent(&veld_helper_bin, &socket_path).await?,
        "linux" => install_helper_systemd_user(&veld_helper_bin, &socket_path).await?,
        other => anyhow::bail!("unsupported OS: {other}"),
    };

    // Give the service a moment to start.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Check if the helper is reachable.
    let client = HelperClient::new(&socket_path);
    let helper_up = client.status().await.is_ok();

    if !helper_up {
        // Service registration may have failed or the process hasn't started
        // yet. Start the helper directly as a background process.
        tracing::info!("helper not reachable via service manager, starting directly");
        let _child = tokio::process::Command::new(&veld_helper_bin)
            .arg("--socket-path")
            .arg(&socket_path)
            .arg("--https-port")
            .arg("8443")
            .arg("--http-port")
            .arg("8080")
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

    // Start Caddy via the helper.
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
    Ok(format!("veld-helper {via}, Caddy started"))
}

/// Install veld-helper as a macOS LaunchAgent (user-level, no sudo needed).
async fn install_helper_launchagent(
    bin: &std::path::Path,
    socket_path: &std::path::Path,
) -> Result<bool, anyhow::Error> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let plist_dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&plist_dir).context("failed to create LaunchAgents directory")?;
    let plist_path = plist_dir.join("dev.veld.helper.plist");

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
        <string>--socket-path</string>
        <string>{}</string>
        <string>--https-port</string>
        <string>8443</string>
        <string>--http-port</string>
        <string>8080</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#,
        bin.display(),
        socket_path.display(),
    );

    // Get current user's UID for launchctl commands.
    let uid = get_current_uid()?;
    let domain_target = format!("gui/{uid}/{label}");
    let domain = format!("gui/{uid}");

    // Bootout any existing agent (required for upgrades).
    let _ = Command::new("launchctl")
        .args(["bootout", &domain_target])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    std::fs::write(&plist_path, &plist).context("failed to write helper LaunchAgent plist")?;

    // Bootstrap the agent in the current user's GUI domain.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new("launchctl")
            .args(["bootstrap", &domain, &plist_path.to_string_lossy()])
            .stdin(std::process::Stdio::null())
            .status(),
    )
    .await;

    match result {
        Ok(Ok(status)) if status.success() => Ok(true),
        Ok(Ok(status)) if status.code() == Some(37) => {
            // Already loaded — kickstart to restart with new binary.
            let _ = Command::new("launchctl")
                .args(["kickstart", "-k", &domain_target])
                .stdin(std::process::Stdio::null())
                .status()
                .await;
            Ok(true)
        }
        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
            // bootstrap failed — fall back to legacy `load` command.
            let load_result = Command::new("launchctl")
                .args(["load", "-w", &plist_path.to_string_lossy()])
                .stdin(std::process::Stdio::null())
                .status()
                .await;
            Ok(load_result.is_ok_and(|s| s.success()))
        }
    }
}

/// Install veld-helper as a systemd user unit (Linux, no sudo needed).
async fn install_helper_systemd_user(
    bin: &std::path::Path,
    socket_path: &std::path::Path,
) -> Result<bool, anyhow::Error> {
    let unit_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).context("failed to create systemd user unit dir")?;

    let unit_path = unit_dir.join("veld-helper.service");
    let unit = format!(
        "[Unit]\n\
         Description=Veld Helper (unprivileged)\n\
         \n\
         [Service]\n\
         ExecStart={} --socket-path {} --https-port 8443 --http-port 8080\n\
         Restart=always\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bin.display(),
        socket_path.display(),
    );
    std::fs::write(&unit_path, unit).context("failed to write helper systemd user unit")?;

    let reload = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .await;
    if reload.is_err() {
        return Ok(false);
    }

    // Restart to pick up new binary on upgrades.
    let _ = Command::new("systemctl")
        .args(["--user", "restart", "veld-helper"])
        .status()
        .await;

    let enable = Command::new("systemctl")
        .args(["--user", "enable", "--now", "veld-helper"])
        .status()
        .await;

    Ok(enable.is_ok_and(|s| s.success()))
}

/// Get the current user's UID.
fn get_current_uid() -> Result<String, anyhow::Error> {
    let output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .context("failed to run `id -u`")?;
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if uid.is_empty() || !output.status.success() {
        anyhow::bail!("failed to determine current user UID");
    }
    Ok(uid)
}

/// Persist the setup mode (privileged/unprivileged) to `~/.veld/setup.json`.
fn write_setup_mode(mode: &str) -> Result<(), anyhow::Error> {
    let veld_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?
        .join(".veld");
    std::fs::create_dir_all(&veld_dir)?;
    let setup_json = veld_dir.join("setup.json");
    let content = serde_json::json!({"mode": mode});
    std::fs::write(&setup_json, serde_json::to_string_pretty(&content)?)?;
    Ok(())
}

fn print_step(current: usize, total: usize, label: &str) {
    let padded = output::pad_right(label, 40);
    eprint!("{}", output::step(current, total, &padded));
}

fn print_step_ok(detail: &str) {
    eprintln!(" {} {}", output::checkmark(), output::green(detail));
}

fn print_step_fail(detail: &str) {
    eprintln!(" {} {}", output::cross(), output::red(detail));
}
