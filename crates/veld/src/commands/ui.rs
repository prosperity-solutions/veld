use crate::output;

/// The management UI URL. Uses HTTPS on the reserved `veld.localhost` domain.
fn management_url(https_port: u16) -> String {
    if https_port == 443 {
        "https://veld.localhost".to_string()
    } else {
        format!("https://veld.localhost:{https_port}")
    }
}

/// `veld ui` — open the management dashboard in the default browser.
pub async fn run() -> i32 {
    // Determine HTTPS port from the helper.
    let https_port = match veld_core::helper::HelperClient::connect().await {
        Ok(client) => client.https_port().await.unwrap_or(18443),
        Err(_) => {
            output::print_error("Veld helper not running. Run `veld setup` first.", false);
            return 1;
        }
    };

    let url = management_url(https_port);

    // Open in the default browser.
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&url).status()
    } else if cfg!(target_os = "linux") {
        std::process::Command::new("xdg-open").arg(&url).status()
    } else {
        output::print_info(&format!("Open in your browser: {url}"));
        return 0;
    };

    match result {
        Ok(status) if status.success() => {
            output::print_info(&format!("Opened {url}"));
            0
        }
        _ => {
            output::print_info(&format!("Open in your browser: {url}"));
            0
        }
    }
}
