use crate::output;
use std::io::{self, BufRead, Write};

/// Read the setup mode from `~/.veld/setup.json`.
fn read_setup_mode() -> Option<String> {
    let path = dirs::home_dir()?.join(".veld").join("setup.json");
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .get("mode")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// `veld uninstall` -- remove Veld and clean up.
pub async fn run() -> i32 {
    let mode = read_setup_mode();
    let needs_sudo = mode.as_deref() == Some("privileged");

    // Only escalate to sudo for privileged installations.
    if needs_sudo && !super::setup::is_root_user() {
        eprintln!(
            "{} Uninstall requires administrator privileges (privileged mode).",
            output::bold("Note:")
        );
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Cannot determine executable path: {e}");
                return 1;
            }
        };
        let status = std::process::Command::new("sudo")
            .arg(&exe)
            .arg("uninstall")
            .status();
        return match status {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("Failed to run sudo: {e}");
                1
            }
        };
    }

    if output::is_tty() {
        eprintln!(
            "{} This will remove Veld, its daemons, certificates and cached state.",
            output::yellow("Warning:"),
        );
        eprint!("Continue? [y/N] ");
        io::stderr().flush().ok();

        let stdin = io::stdin();
        let line = match stdin.lock().lines().next() {
            Some(Ok(l)) => l,
            _ => return 1,
        };

        if !matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
            output::print_info("Cancelled.");
            return 1;
        }
    }

    match veld_core::setup::uninstall().await {
        Ok(()) => {
            output::print_success("Veld has been uninstalled.");
            0
        }
        Err(e) => {
            output::print_error(&format!("Uninstall failed: {e}"), false);
            1
        }
    }
}
