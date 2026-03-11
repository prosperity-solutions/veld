use crate::output;

/// `veld setup` -- run the first-time setup sequence.
pub async fn run() -> i32 {
    println!("{}", output::bold("Veld Setup"));
    println!();

    const TOTAL: usize = 6;

    // Step 1: Check port availability.
    print_step(1, TOTAL, "Checking port availability...");
    match veld_core::setup::check_ports().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 2: Install Caddy.
    print_step(2, TOTAL, "Installing Caddy...");
    match veld_core::setup::install_caddy().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 3: Install mkcert.
    print_step(3, TOTAL, "Installing mkcert...");
    match veld_core::setup::install_mkcert().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 4: Generate TLS certificates.
    print_step(4, TOTAL, "Generating TLS certificates...");
    match veld_core::setup::generate_certs().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 5: Install Veld daemon.
    print_step(5, TOTAL, "Installing Veld daemon...");
    match veld_core::setup::install_daemon().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
        }
    }

    // Step 6: Install Veld helper.
    print_step(6, TOTAL, "Installing Veld helper...");
    match veld_core::setup::install_helper().await {
        Ok(info) => print_step_ok(&info.message),
        Err(e) => {
            print_step_fail(&format!("{e:#}"));
            return 1;
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
