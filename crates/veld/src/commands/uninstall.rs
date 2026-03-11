use crate::output;
use std::io::{self, BufRead, Write};

/// `veld uninstall` -- remove Veld and clean up.
pub async fn run() -> i32 {
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
