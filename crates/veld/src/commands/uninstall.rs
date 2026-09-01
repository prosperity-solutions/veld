use crate::output;
use std::io::{self, BufRead, Write};

/// `veld uninstall` -- remove Veld and clean up.
pub async fn run() -> i32 {
    let mode = super::read_setup_mode();
    // Escalate for a recorded privileged mode **or** for a root-owned helper
    // directory that is still on disk (#262).
    //
    // The second half is not redundant. `install.sh`'s switch-to-user-paths path
    // overwrites `setup.json` with `{}` while leaving the store behind, so a
    // machine can carry a root-owned helper and its signature with nothing
    // saying it was ever privileged. Without this the uninstall runs
    // unprivileged, `remove_dir_all` fails, and the user is left with root-owned
    // files and no veld able to remove them — precisely the leftover this issue
    // set out to prevent.
    let needs_sudo =
        mode.as_deref() == Some("privileged") || veld_core::paths::privileged_helper_dir().exists();

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
        // Backups are named rather than folded into "cached state": they are the
        // one thing here somebody may have been keeping deliberately, and the copies
        // of the database go with the database for the same reason it goes at all —
        // they carry the same secrets.
        // "the default backup folder", not "its backups": a `backup.dir` pointed at
        // an external drive is the user's own folder and is deliberately left alone
        // (see `setup.rs`). Promising more than that would tell somebody their
        // secrets-bearing copies were gone when they are still on the drive.
        eprintln!(
            "{} This will remove Veld, its daemons, certificates, cached state and the \
             default database-backup folder{app}.",
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
