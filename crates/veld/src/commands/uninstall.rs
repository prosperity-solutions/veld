use crate::output;
use std::io::{self, BufRead, Write};

/// `veld uninstall` -- remove Veld and clean up.
pub async fn run() -> i32 {
    let mode = super::read_setup_mode();
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
        // Names the app explicitly: the installer puts it in /Applications by
        // default, so for most macOS users it is the part of this they can
        // actually see, and "cached state" does not cover a Dock icon.
        let app = veld_core::setup::desktop_app_status()
            .map(|(p, _)| format!(", and {}", p.display()))
            .unwrap_or_default();
        eprintln!(
            "{} This will remove Veld, its daemons, certificates and cached state{app}.",
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
            // Last chance to tell the user their Hammerspoon config now points
            // at nothing — after this, veld is gone and can never say it.
            super::remove_legacy_hammerspoon().await;
            output::print_success("Veld has been uninstalled.");
            0
        }
        Err(e) => {
            output::print_error(&format!("Uninstall failed: {e}"), false);
            1
        }
    }
}
