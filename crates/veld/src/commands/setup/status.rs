use crate::output;

/// `veld setup` (no subcommand) -- show current setup status.
pub async fn run() -> i32 {
    println!("{}", output::bold("Veld Setup"));
    println!();

    // Check if helper is reachable and get its mode.
    let status = veld_core::setup::check_setup().await;

    if status.is_complete() {
        // Helper is running -- try to determine the mode.
        match veld_core::helper::HelperClient::connect().await {
            Ok(client) => match client.https_port().await {
                Ok(443) => {
                    println!(
                        "  Mode:       {}",
                        output::green("privileged (ports 80/443)")
                    );
                }
                Ok(port) => {
                    println!(
                        "  Mode:       {}",
                        output::cyan(&format!("unprivileged (port {port})"))
                    );
                }
                Err(_) => {
                    println!("  Mode:       {}", output::green("privileged"));
                }
            },
            Err(_) => {
                println!("  Mode:       {}", output::green("privileged"));
            }
        }
        println!("  Helper:     {}", output::green("running"));
    } else {
        // Helper is not running -- try to connect anyway for partial info.
        match veld_core::helper::HelperClient::connect().await {
            Ok(client) => {
                match client.https_port().await {
                    Ok(443) => {
                        println!("  Mode:       {}", output::green("privileged"));
                    }
                    Ok(port) => {
                        println!(
                            "  Mode:       {}",
                            output::cyan(&format!("unprivileged (port {port})"))
                        );
                    }
                    Err(_) => {
                        println!("  Mode:       {}", output::cyan("unprivileged"));
                    }
                }
                println!("  Helper:     {}", output::green("running"));
            }
            Err(_) => {
                println!("  Mode:       not configured");
                println!("  Helper:     not running");
            }
        }
    }

    let caddy_present = veld_core::paths::caddy_bin().exists();
    println!(
        "  Caddy:      {}",
        if caddy_present {
            output::green("installed")
        } else {
            output::red("not installed")
        }
    );

    println!();
    println!("  {}", output::bold("Available commands:"));
    println!("    veld setup unprivileged    No-sudo setup (port 18443)");
    println!("    veld setup privileged      Clean URLs, ports 80/443 (one-time sudo)");

    if cfg!(target_os = "macos") && std::path::Path::new("/Applications/Hammerspoon.app").exists() {
        println!("    veld setup hammerspoon     Menu bar widget");
    }

    0
}
