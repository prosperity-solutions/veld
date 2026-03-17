use crate::output;

/// `veld setup hammerspoon` -- install Hammerspoon menu bar widget.
pub async fn run() -> i32 {
    if !cfg!(target_os = "macos") {
        output::print_error("Hammerspoon is only available on macOS.", false);
        return 1;
    }

    if !std::path::Path::new("/Applications/Hammerspoon.app").exists() {
        output::print_error(
            "Hammerspoon is not installed. Install it from https://www.hammerspoon.org/",
            false,
        );
        return 1;
    }

    println!("{}", output::bold("Veld Setup: Hammerspoon"));
    println!();

    eprint!("  Installing Hammerspoon Spoon...        ");
    match veld_core::setup::install_hammerspoon().await {
        Ok(hs_result) => {
            eprintln!(
                " {} {}",
                output::checkmark(),
                output::green(&hs_result.message)
            );

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
                                output::green("init.lua updated — reload Hammerspoon to activate")
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "  {} {}",
                                output::cross(),
                                output::red(&format!("failed to update init.lua: {e}"))
                            );
                            return 1;
                        }
                    }
                } else {
                    eprintln!("  Skipped. Add the lines manually when ready.");
                }
            }
        }
        Err(e) => {
            eprintln!(" {} {}", output::cross(), output::red(&format!("{e:#}")));
            return 1;
        }
    }

    println!();
    output::print_success("Hammerspoon setup complete!");

    0
}
