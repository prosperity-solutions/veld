use std::path::PathBuf;

use crate::output;
use veld_core::helper::HelperClient;

/// `veld setup privileged` -- run the privileged (sudo) setup sequence.
///
/// This is the original `veld setup` behaviour: it installs the system
/// daemon on ports 80/443, trusts the Caddy CA, etc.
pub async fn run(
    helper_bin: Option<PathBuf>,
    user_socket: Option<PathBuf>,
    caddy_bin: Option<PathBuf>,
) -> i32 {
    // Setup requires root for writing LaunchDaemons, binding /var/run socket,
    // etc. If we're not root, re-exec with sudo so the user gets a password
    // prompt automatically.
    if !is_root_user() {
        eprintln!(
            "{} Setup requires administrator privileges.",
            output::bold("Note:")
        );
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Cannot determine executable path: {e}");
                return 1;
            }
        };

        // Resolve paths while we're still the real user (before sudo changes HOME/PATH).
        let resolved_helper_bin =
            veld_core::setup::which_self("veld-helper").unwrap_or_else(|_| "veld-helper".into());
        let resolved_user_socket = veld_core::helper::user_socket_path();
        let resolved_caddy_bin = veld_core::paths::caddy_bin();

        let status = std::process::Command::new("sudo")
            .arg(&exe)
            .arg("setup")
            .arg("privileged")
            .arg("--helper-bin")
            .arg(&resolved_helper_bin)
            .arg("--user-socket")
            .arg(&resolved_user_socket)
            .arg("--caddy-bin")
            .arg(&resolved_caddy_bin)
            .status();
        return match status {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("Failed to run sudo: {e}");
                1
            }
        };
    }

    // --- Running as root (after sudo) ---

    // Use the pre-resolved paths passed via args, or fall back to resolving now.
    let helper_bin_path = helper_bin.unwrap_or_else(|| {
        veld_core::setup::which_self("veld-helper").unwrap_or_else(|_| "veld-helper".into())
    });
    let user_socket_path = user_socket.unwrap_or_else(veld_core::helper::user_socket_path);
    let caddy_bin_path = caddy_bin.unwrap_or_else(veld_core::paths::caddy_bin);

    println!("{}", output::bold("Veld Setup (privileged)"));
    println!();

    let total: usize = 6;

    // Step 1: Stop user-level helper if running.
    print_step(1, total, "Stopping user-level helper...");
    {
        let user_client = HelperClient::new(&user_socket_path);
        if user_client.status().await.is_ok() {
            let _ = user_client.shutdown().await;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            print_step_ok("stopped");
        } else {
            print_step_ok("not running");
        }

        // Remove user-level LaunchAgent to prevent KeepAlive restart
        if cfg!(target_os = "macos") {
            let uid = std::process::Command::new("id")
                .arg("-u")
                .arg(std::env::var("SUDO_USER").unwrap_or_default())
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if !uid.is_empty() {
                let _ = tokio::process::Command::new("launchctl")
                    .args(["bootout", &format!("gui/{uid}/dev.veld.helper")])
                    .status()
                    .await;
            }
            // Also try to remove the plist file
            if let Ok(sudo_user) = std::env::var("SUDO_USER") {
                let plist =
                    format!("/Users/{sudo_user}/Library/LaunchAgents/dev.veld.helper.plist");
                let _ = std::fs::remove_file(&plist);
            }
        }
        if cfg!(target_os = "linux") {
            // Stop and disable user-level systemd service
            if let Ok(sudo_user) = std::env::var("SUDO_USER") {
                let _ = tokio::process::Command::new("sudo")
                    .args([
                        "-u",
                        &sudo_user,
                        "systemctl",
                        "--user",
                        "stop",
                        "veld-helper",
                    ])
                    .status()
                    .await;
                let _ = tokio::process::Command::new("sudo")
                    .args([
                        "-u",
                        &sudo_user,
                        "systemctl",
                        "--user",
                        "disable",
                        "veld-helper",
                    ])
                    .status()
                    .await;
            }
        }
    }

    // Step 2: Check port availability.
    print_step(2, total, "Checking port availability...");
    match veld_core::setup::check_ports(443, 80).await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 3: Install Caddy.
    print_step(3, total, "Installing Caddy...");
    match veld_core::setup::install_caddy(false).await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 4: Install Veld daemon.
    print_step(4, total, "Installing Veld daemon...");
    match veld_core::setup::install_daemon().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 5: Install Veld helper (starts Caddy).
    // Use the pre-resolved helper binary path for the LaunchDaemon plist.
    print_step(5, total, "Installing Veld helper...");
    match veld_core::setup::install_helper_with_bin(&helper_bin_path, Some(&caddy_bin_path)).await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Write setup.json early — even if CA trust fails, the system services are running.
    //
    // Derive the real user's ~/.veld/ from the user_socket_path (which is ~/.veld/helper.sock).
    if let Some(veld_dir) = user_socket_path.parent() {
        let setup_json = veld_dir.join("setup.json");
        let _ = std::fs::create_dir_all(veld_dir);
        let _ = std::fs::write(&setup_json, r#"{"mode":"privileged"}"#);

        // Fix ownership so the file belongs to the real user, not root.
        if let Ok(sudo_user) = std::env::var("SUDO_USER") {
            let _ = std::process::Command::new("chown")
                .arg(format!("{sudo_user}:staff"))
                .arg(&setup_json)
                .output();
            // Also fix the .veld directory itself in case we just created it.
            let _ = std::process::Command::new("chown")
                .arg(format!("{sudo_user}:staff"))
                .arg(veld_dir)
                .output();
        }
    }

    // Step 6: Trust Caddy's CA in the system store (non-fatal).
    print_step(6, total, "Trusting Caddy CA...");
    match veld_core::setup::trust_caddy_ca().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            eprintln!("  HTTPS will work but browsers may show certificate warnings.");
        }
    }

    println!();
    output::print_success("Privileged setup complete! Run `veld start` to get going.");

    0
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

/// Check if the current process is running as root.
pub fn is_root_user() -> bool {
    std::env::var("EUID")
        .or_else(|_| {
            std::process::Command::new("id")
                .arg("-u")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .map(|id| id == "0")
        .unwrap_or(false)
}
