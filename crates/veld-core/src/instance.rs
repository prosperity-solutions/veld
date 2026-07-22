//! Per-instance overrides, so a source-built dev stack (CLI + daemon) can run
//! ALONGSIDE the installed veld instead of replacing or parking it.
//!
//! An "instance" is: a database (`VELD_DB_PATH`, handled in [`crate::db`]),
//! a daemon HTTP port (`VELD_DAEMON_PORT`), a daemon Unix socket
//! (`VELD_DAEMON_SOCK`), and optionally its own management hostname
//! (`VELD_MANAGEMENT_HOST`, e.g. `veld-dev.localhost` — the daemon
//! self-registers a Caddy route for it at startup). All default to the
//! installed instance's values, so a plain environment is byte-for-byte the
//! behavior veld always had.
//!
//! The helper/Caddy/DNS layer is deliberately NOT instanced — it is a
//! singleton owning ports 80/443/18443 and system DNS; every instance shares
//! it. Only the *management* route is instance-scoped (`veld-mgmt-<host>`):
//! RUN routes stay keyed by `veld-{run}-{node}-{variant}` and run-name-based
//! hostnames, so two instances starting an environment with the SAME name
//! collide in shared Caddy (last-write-wins, and stopping one removes the
//! route the other still needs). Keep dev-instance run names distinct from
//! the installed instance's.

use std::path::PathBuf;

/// The installed instance's daemon HTTP port (management UI, feedback,
/// client-logs, share control API).
pub const DEFAULT_DAEMON_PORT: u16 = 19899;

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Daemon HTTP port: `VELD_DAEMON_PORT` or the default. An unparseable value
/// falls back to the default rather than erroring — the CLI must keep working
/// in a polluted environment.
pub fn daemon_port() -> u16 {
    env_nonempty("VELD_DAEMON_PORT")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_DAEMON_PORT)
}

/// Base URL of this instance's daemon control API.
pub fn daemon_base() -> String {
    format!("http://127.0.0.1:{}", daemon_port())
}

/// Upstream (`host:port`) baked into Caddy routes for feedback/client-log
/// traffic — runs started by a dev-instance CLI route their overlay traffic
/// to the dev daemon.
pub fn daemon_upstream() -> String {
    format!("localhost:{}", daemon_port())
}

/// Daemon Unix socket path: `VELD_DAEMON_SOCK` or `~/.veld/daemon.sock`.
pub fn daemon_socket() -> PathBuf {
    if let Some(p) = env_nonempty("VELD_DAEMON_SOCK") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".veld")
        .join("daemon.sock")
}

/// Management hostname this daemon should self-register with the helper
/// (e.g. `veld-dev.localhost`). `None` for the installed instance — its
/// `veld.localhost` route is part of the helper's base Caddy config.
///
/// Rejected (returns `None`, with a warning): a value that isn't a plausible
/// hostname, and `veld.localhost` itself — self-registering the installed
/// dashboard's hostname would hijack it to this instance (last route wins).
pub fn management_host() -> Option<String> {
    let host = env_nonempty("VELD_MANAGEMENT_HOST")?;
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && host != "veld.localhost";
    if !valid {
        tracing::warn!(
            host,
            "ignoring VELD_MANAGEMENT_HOST: not a valid hostname (or it is \
             the installed dashboard's veld.localhost)"
        );
        return None;
    }
    Some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests mutate process-global state; keep them in ONE test so the
    // default parallel test runner can't interleave them.
    #[test]
    fn overrides_and_defaults() {
        // SAFETY: set_var's contract is process-wide — this is sound only
        // because no other test in the WHOLE test binary reads or writes
        // these variables (verified by grep), so no thread observes the
        // mutation concurrently. Keep it that way.
        unsafe {
            std::env::remove_var("VELD_DAEMON_PORT");
            assert_eq!(daemon_port(), DEFAULT_DAEMON_PORT);

            std::env::set_var("VELD_DAEMON_PORT", "19898");
            assert_eq!(daemon_port(), 19898);
            assert_eq!(daemon_base(), "http://127.0.0.1:19898");
            assert_eq!(daemon_upstream(), "localhost:19898");

            std::env::set_var("VELD_DAEMON_PORT", "not-a-port");
            assert_eq!(daemon_port(), DEFAULT_DAEMON_PORT);
            std::env::set_var("VELD_DAEMON_PORT", "");
            assert_eq!(daemon_port(), DEFAULT_DAEMON_PORT);
            std::env::remove_var("VELD_DAEMON_PORT");

            std::env::set_var("VELD_DAEMON_SOCK", "/tmp/dev.sock");
            assert_eq!(daemon_socket(), PathBuf::from("/tmp/dev.sock"));
            std::env::remove_var("VELD_DAEMON_SOCK");
            assert!(daemon_socket().ends_with(".veld/daemon.sock"));

            std::env::remove_var("VELD_MANAGEMENT_HOST");
            assert_eq!(management_host(), None);
            std::env::set_var("VELD_MANAGEMENT_HOST", "veld-dev.localhost");
            assert_eq!(management_host().as_deref(), Some("veld-dev.localhost"));
            std::env::remove_var("VELD_MANAGEMENT_HOST");
        }
    }
}
