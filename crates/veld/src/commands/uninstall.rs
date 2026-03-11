use crate::output;
use std::io::{self, BufRead, Write};

/// `veld uninstall` -- remove Veld and clean up.
pub async fn run() -> i32 {
    // Uninstall needs root for removing LaunchDaemons, system files, etc.
    if !super::setup::is_root_user() {
        eprintln!(
            "{} Uninstall requires administrator privileges.",
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
