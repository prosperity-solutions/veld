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
//! it. Only the *management* route is instance-scoped (`veld-mgmt-<host>`).
//! RUN routes are keyed by hostname ([`crate::url::run_route_id`]), so two
//! instances collide in shared Caddy exactly when they mint the same
//! hostname — which two checkouts of one repo do whenever their run names
//! match (last-write-wins, and stopping one removes the route the other still
//! needs). `veld start` refuses that case *within* one instance by checking
//! the registry, but instances have separate databases and cannot see each
//! other's runs, so nothing detects it across them. Keep dev-instance run
//! names distinct from the installed instance's.

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

/// Where this instance's terminal holder sockets live.
///
/// One owner for the name, because three crates need it: the daemon binds and
/// scans it, `veld doctor` reports it, and `veld uninstall` sweeps it.
///
/// Keyed by daemon **port**, not just by the socket directory: a dev instance must
/// never adopt the installed instance's terminal sessions, whose `worktree_id`s
/// come from a different database entirely. `VELD_PTY_DIR` overrides it outright,
/// which is how the daemon's own recovery test points a child at a temp dir.
///
/// **Under the home directory, not beside the daemon socket**, and that is a
/// length constraint rather than a preference. A unix socket path is capped by
/// `sockaddr_un::sun_path` — 104 bytes on macOS, 108 on Linux — and the port
/// already separates instances, so following `VELD_DAEMON_SOCK` here bought
/// nothing and cost the bound: `just dev-daemon` puts that socket inside the
/// checkout, and a checkout under `~/git/_worktrees/<branch-name>/` produced a
/// 112-byte path. The failure is `bind` reporting "path must be shorter than
/// SUN_LEN", which reaches the user as "could not open a terminal" and names
/// nothing they can act on. `socket_for` in the daemon already digests the
/// *file* name for this reason; this is the other half of the same bound.
pub fn pty_dir() -> PathBuf {
    if let Some(dir) = env_nonempty("VELD_PTY_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".veld")
        .join(format!("{PTY_DIR_PREFIX}{}", daemon_port()))
}

/// The longest a holder socket path may be, and what to do about it.
///
/// `sun_path` is 104 bytes on macOS and 108 on Linux; 104 is the safe floor.
/// Checked before binding so the diagnostic can name the escape hatch — the
/// bare `bind` error says only "path must be shorter than SUN_LEN", which is
/// true, unhelpful, and surfaces as a terminal that will not open.
pub const MAX_SOCKET_PATH: usize = 104;

/// Whether `path` fits in a `sockaddr_un`, with the message to show when it
/// does not.
pub fn socket_path_too_long(path: &std::path::Path) -> Option<String> {
    let len = path.as_os_str().as_encoded_bytes().len();
    if len < MAX_SOCKET_PATH {
        return None;
    }
    Some(format!(
        "the terminal holder's socket path is {len} bytes, over the {MAX_SOCKET_PATH}-byte \
         limit a unix socket allows ({}). Set VELD_PTY_DIR to a shorter directory.",
        path.display()
    ))
}

/// The prefix every [`pty_dir`] shares, for code that must find the holder
/// directories of **every** instance rather than only this one — `veld uninstall`,
/// which has to stop them all.
pub const PTY_DIR_PREFIX: &str = "pty-";

/// Where this instance keeps the little executables it puts in a terminal's
/// environment — today `veld-open`, plus `open`/`xdg-open` wrappers (see
/// `veld-daemon/src/pty/shims.rs`).
///
/// Keyed by daemon **port** for the same reason [`pty_dir`] is: the script points a
/// URL at *this* daemon's session registry, and a dev instance handing the
/// installed instance's daemon a session id it has never heard of would silently
/// route nothing. Deliberately **not** inside [`pty_dir`], whose entries the
/// holder-recovery scan walks looking for sockets.
///
/// `VELD_SHIM_DIR` is the variable a terminal sees, and it is an *output* of this
/// function rather than an input — a client-supplied directory here would be a
/// client-supplied executable on a developer's PATH.
pub fn shim_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".veld")
        .join(format!("{SHIM_DIR_PREFIX}{}", daemon_port()))
}

/// The prefix every [`shim_dir`] shares, so `veld uninstall` can sweep the
/// directories of every instance.
pub const SHIM_DIR_PREFIX: &str = "shim-";

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

            // The holder directory does NOT follow the daemon socket, and that
            // is the length bound rather than a preference — see `pty_dir`. A
            // dev socket inside a deep checkout used to drag the holder sockets
            // in with it and overrun `sun_path`.
            std::env::remove_var("VELD_PTY_DIR");
            std::env::set_var(
                "VELD_DAEMON_SOCK",
                "/Users/someone/git/_worktrees/a-long-branch-name/.veld-dev/daemon.sock",
            );
            let dir = pty_dir();
            assert!(dir.starts_with(dirs::home_dir().unwrap()), "{dir:?}");
            assert!(!dir.to_string_lossy().contains("_worktrees"), "{dir:?}");
            std::env::remove_var("VELD_DAEMON_SOCK");

            // …and the override still wins outright, which is how the recovery
            // test points a child at a temp dir.
            std::env::set_var("VELD_PTY_DIR", "/tmp/veld-pty-test");
            assert_eq!(pty_dir(), PathBuf::from("/tmp/veld-pty-test"));
            std::env::remove_var("VELD_PTY_DIR");

            // The real default has to fit with room for the digest filename, or
            // the bound below is theatre. Asserted here rather than in its own
            // test because `pty_dir` reads the vars this test owns.
            let realistic = pty_dir().join("0123456789abcdef.sock");
            assert_eq!(socket_path_too_long(&realistic), None, "{realistic:?}");
        }
    }

    #[test]
    fn socket_path_length_is_reported_before_bind() {
        // `bind`'s own error is "path must be shorter than SUN_LEN", which names
        // neither the path nor the way out, and reaches the user as a terminal
        // that will not open.
        assert_eq!(
            socket_path_too_long(std::path::Path::new("/tmp/a.sock")),
            None
        );

        let long = PathBuf::from(format!("/tmp/{}/0123456789abcdef.sock", "x".repeat(100)));
        let msg = socket_path_too_long(&long).expect("over the limit");
        assert!(msg.contains("VELD_PTY_DIR"), "{msg}");
        assert!(msg.contains(&MAX_SOCKET_PATH.to_string()), "{msg}");

        // The exact boundary, since this is an off-by-one waiting to happen:
        // `sun_path` holds MAX_SOCKET_PATH bytes *including* the NUL.
        let at = PathBuf::from("x".repeat(MAX_SOCKET_PATH));
        assert!(socket_path_too_long(&at).is_some());
        let under = PathBuf::from("x".repeat(MAX_SOCKET_PATH - 1));
        assert_eq!(socket_path_too_long(&under), None);
    }
}
