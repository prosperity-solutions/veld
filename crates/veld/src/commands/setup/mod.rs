mod hammerspoon;
mod privileged;
mod status;
mod unprivileged;

pub use privileged::is_root_user;

use crate::SetupCommand;

pub async fn run(command: Option<SetupCommand>) -> i32 {
    match command {
        None => status::run().await,
        Some(SetupCommand::Unprivileged) => unprivileged::run().await,
        Some(SetupCommand::Privileged {
            helper_bin,
            user_socket,
            caddy_bin,
        }) => privileged::run(helper_bin, user_socket, caddy_bin).await,
        Some(SetupCommand::Hammerspoon) => hammerspoon::run().await,
    }
}
