use crate::output;

/// `veld setup` -- run the first-time setup sequence.
pub async fn run(force: bool) -> i32 {
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
        let mut cmd = std::process::Command::new("sudo");
        cmd.arg(&exe).arg("setup");
        if force {
            cmd.arg("--force");
        }
        let status = cmd.status();
        return match status {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("Failed to run sudo: {e}");
                1
            }
        };
    }

    println!("{}", output::bold("Veld Setup"));
    println!();

    let has_hammerspoon = std::path::Path::new("/Applications/Hammerspoon.app").exists();
    let total: usize = if has_hammerspoon { 6 } else { 5 };

    // Step 1: Check port availability.
    print_step(1, total, "Checking port availability...");
    match veld_core::setup::check_ports().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 2: Install Caddy.
    print_step(2, total, "Installing Caddy...");
    match veld_core::setup::install_caddy(force).await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 3: Install Veld daemon.
    print_step(3, total, "Installing Veld daemon...");
    match veld_core::setup::install_daemon().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 4: Install Veld helper (starts Caddy).
    print_step(4, total, "Installing Veld helper...");
    match veld_core::setup::install_helper().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 5: Trust Caddy's CA in the system store.
    print_step(5, total, "Trusting Caddy CA...");
    match veld_core::setup::trust_caddy_ca().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 6: Install Hammerspoon Spoon (optional, non-fatal).
    if has_hammerspoon {
        print_step(6, total, "Installing Hammerspoon Spoon...");
        match veld_core::setup::install_hammerspoon().await {
            Ok(hs_result) => {
                print_step_ok(&hs_result.message);

                // Offer to patch init.lua if IPC or loadSpoon lines are missing.
                if hs_result.needs_ipc || hs_result.needs_load_spoon {
                    println!();
                    let mut lines_to_add = Vec::new();
                    if hs_result.needs_ipc {
                        lines_to_add.push("require(\"hs.ipc\")");
                    }
                    if hs_result.needs_load_spoon {
                        lines_to_add.push("hs.loadSpoon(\"Veld\"):start()");
                    }

                    eprintln!(
                        "  {} The following lines are needed in {}:",
                        output::bold("Hammerspoon:"),
                        hs_result.init_lua_path.display()
                    );
                    for line in &lines_to_add {
                        eprintln!("    {}", output::green(line));
                    }
                    eprintln!();
                    eprint!("  Add them automatically? [Y/n] ");

                    let mut answer = String::new();
                    let _ = std::io::stdin().read_line(&mut answer);
                    let answer = answer.trim().to_lowercase();

                    if answer.is_empty() || answer == "y" || answer == "yes" {
                        match veld_core::setup::patch_hammerspoon_init_lua(&hs_result) {
                            Ok(()) => {
                                eprintln!(
                                    "  {} {}",
                                    output::checkmark(),
                                    output::green(
                                        "init.lua updated — reload Hammerspoon to activate"
                                    )
                                );
                            }
                            Err(e) => {
                                eprintln!(
                                    "  {} {}",
                                    output::cross(),
                                    output::red(&format!("failed to update init.lua: {e}"))
                                );
                            }
                        }
                    } else {
                        eprintln!("  Skipped. Add the lines manually when ready.");
                    }
                }
            }
            Err(e) => {
                // Never fail setup for a menu bar widget.
                print_step_ok(&format!("skipped ({e})"))
            }
        }
    }

    println!();
    output::print_success("Setup complete! Run `veld start` to get going.");

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
