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
pub async fn install(wait_pid: Option<u32>, relaunch: bool) -> i32 {
    if std::env::consts::OS != "macos" {
        output::print_error(
            "Veld Desktop is installed by this command on macOS only. On Linux, download the \
             AppImage or .deb from the releases page — the AppImage updates itself.",
            false,
        );
        return 1;
    }

    // The app and the CLI ship from one tag with one version, so the app that
    // matches *this* binary is the one to install. `veld update` moves both.
    let version = env!("CARGO_PKG_VERSION");
    output::print_info(&format!("Installing Veld Desktop {version}..."));

    match veld_core::setup::install_desktop(version, wait_pid, relaunch).await {
        Ok(()) => 0,
        Err(e) => {
            output::print_error(&format!("Could not install Veld Desktop: {e}"), false);
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
