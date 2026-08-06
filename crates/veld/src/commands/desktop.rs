//! `veld desktop` -- install, update and report on Veld Desktop, the macOS app.
//!
//! The CLI installs the app for a reason that is not convenience: a build a
//! browser downloaded carries `com.apple.quarantine`, and that attribute is what
//! makes Gatekeeper refuse the first launch of a build that is not notarized.
//! curl does not set it, so an app delivered through the install script simply
//! opens — no Developer ID required. The trust boundary is unchanged, since the
//! script came from the same origin and the archive is checksum-verified.
//!
//! All three subcommands are thin: the work lives in the install script, so that
//! `veld update` keeps the app in step without a second implementation of
//! download-verify-install (`veld_core::setup::install_desktop`).

use crate::output;

/// `veld desktop install` / `veld desktop update`.
///
/// `wait_pid` and `relaunch` exist for the app updating *itself*: it hands off to
/// the CLI and quits, because an Electron app reads from its own bundle while it
/// runs and cannot be swapped underneath.
pub async fn install(version: Option<String>, wait_pid: Option<u32>, relaunch: bool) -> i32 {
    if std::env::consts::OS != "macos" {
        output::print_error(
            "Veld Desktop is installed by this command on macOS only. On Linux, download the \
             AppImage or .deb from the releases page — the AppImage updates itself.",
            false,
        );
        return 1;
    }

    // Defaults to this binary's version, because the app and the CLI ship from one
    // tag. The app passes an explicit one when it was offered a release the CLI has
    // not caught up to yet — otherwise it would be reinstalled at the CLI's version
    // and offered the newer one again on every launch.
    let version = version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    output::print_info(&format!("Installing Veld Desktop {version}..."));

    if let Err(e) = veld_core::setup::install_desktop(&version, wait_pid, relaunch).await {
        output::print_error(&format!("Could not install Veld Desktop: {e}"), false);
        return 1;
    }

    // The script is fetched from veld.oss.life.li, not from this checkout, so a
    // published copy older than this feature simply has no desktop section: it
    // exits 0 having installed nothing. Without this check the command would
    // report success and the app would never appear.
    match veld_core::setup::desktop_app_status() {
        Some((path, installed)) if installed.as_deref() == Some(version.as_str()) => {
            output::print_success(&format!("Veld Desktop {version} — {}", path.display()));
            0
        }
        Some((path, installed)) => {
            output::print_error(
                &format!(
                    "The installer left Veld Desktop at {} ({}), not {version}. Its install \
                     script may predate this command — try again after the next release.",
                    path.display(),
                    installed.as_deref().unwrap_or("unknown version"),
                ),
                false,
            );
            1
        }
        None => {
            output::print_error(
                "The installer did not place Veld Desktop. Its install script may predate this \
                 command — try again after the next release.",
                false,
            );
            1
        }
    }
}

/// `veld desktop status` -- where the app is and whether it matches this CLI.
pub async fn status(json: bool) -> i32 {
    let cli_version = env!("CARGO_PKG_VERSION");
    let found = veld_core::setup::desktop_app_status();

    if json {
        let payload = match &found {
            Some((path, version)) => serde_json::json!({
                "installed": true,
                "path": path.display().to_string(),
                "version": version,
                "cli_version": cli_version,
                "in_sync": version.as_deref() == Some(cli_version),
            }),
            None => serde_json::json!({
                "installed": false,
                "cli_version": cli_version,
            }),
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return 0;
    }

    match found {
        Some((path, version)) => {
            let version = version.unwrap_or_else(|| "unknown".to_string());
            output::print_info(&format!("Veld Desktop {version}"));
            println!("  {}", output::dim(&path.display().to_string()));
            if version != cli_version {
                println!();
                output::print_info(&format!(
                    "The CLI is {cli_version}. Run 'veld desktop update' to match them."
                ));
            }
            0
        }
        None => {
            output::print_info("Veld Desktop is not installed.");
            if std::env::consts::OS == "macos" {
                println!("  {}", output::dim("veld desktop install"));
            }
            0
        }
    }
}
