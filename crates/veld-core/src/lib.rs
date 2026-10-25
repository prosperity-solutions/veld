/// The version every veld binary reports: the newer of `Cargo.toml`'s version and
/// the newest release tag the build saw. See `build.rs` for why it is not simply
/// `CARGO_PKG_VERSION`.
pub const VERSION: &str = env!("VELD_VERSION");

pub mod agent;
pub mod config;
pub use uuid;
pub mod agent_runtime;
pub mod console;
pub mod db;
pub mod desktop_pref;
pub mod engine_lock;
pub mod files;
pub mod graph;
pub mod health;
pub mod helper;
pub mod helper_gate;
pub mod helper_store;
pub mod ide;
pub mod include;
pub mod instance;
pub mod jsonc;
pub mod logging;
pub mod opener;
pub mod paths;
pub mod percent;
pub mod port;
pub mod presets;
pub mod process;
pub mod project_id;
#[doc(hidden)]
pub mod release_version;
pub mod remote;
pub mod runner;
pub mod sensitive;
pub mod setup;
pub mod share;
pub mod shell;
pub mod signing;
pub mod state;
pub mod stats;
#[cfg(test)]
pub(crate) mod test_support;
pub mod tls_health;
pub mod update_lock;
pub mod url;
pub mod user_path;
pub mod values;
pub mod variables;
