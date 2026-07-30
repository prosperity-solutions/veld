//! Resolving the user's login-shell `PATH` for spawned commands.
//!
//! The daemon (launchd) and gateway (systemd) run with a bare service `PATH`,
//! so user-installed CLIs (`op`, `vault`, brew-installed tools, version
//! managers) are not found when a config-declared command is executed — even
//! though the same command works in the user's terminal. Every place veld
//! executes a user-supplied command string on a daemon must therefore inherit
//! the user's login-shell `PATH` via [`resolve_user_path`]: liveness probes
//! (the daemon's health monitor), `SecretSource::Command` token resolution
//! (`veld-share`'s `endpoint::resolve_secret`), and any future daemon-side
//! command-execution surface. (Commands spawned by the `veld` CLI itself —
//! orchestrator steps, actions — already inherit the terminal's `PATH` and do
//! not need this.)
//!
//! Only `PATH` is inherited — not the rest of the login shell's environment
//! (exported variables, aliases, functions). On a headless host with no user
//! shell config (the gateway container), the login shell contributes nothing
//! and this cheaply falls back to the process `PATH` — set `PATH` in the
//! image/service definition there.

use std::time::Duration;

use tracing::{debug, info, warn};

/// Bound on how long the login-shell PATH resolution may take. A `.zshrc`
/// that stalls (version managers, network init right after a macOS wake) must
/// not wedge the caller — resolution falls back to the process `PATH` instead.
const PATH_RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve the user's full `PATH` by spawning an interactive login shell.
/// Falls back to the current process `PATH` (or `/usr/local/bin:/usr/bin:/bin`
/// if even that is empty — the result is never empty) when resolution fails
/// or times out.
///
/// Spawns `$SHELL -l -i -c 'command env'` and parses the `PATH=` line, so it
/// captures
/// `PATH` after `.zprofile`/`.zshrc`/`.bash_profile`/`brew shellenv` etc. have
/// run — the value the user's own terminal would have. Parsing `env` output
/// (rather than capturing `echo $PATH`) keeps this correct for any shell —
/// fish would print `$PATH` space-separated, and a chatty rc file's greeting
/// lines don't start with `PATH=` — the environment variable itself is
/// colon-delimited regardless of shell.
///
/// Not cached: callers resolve at most a handful of times per operation
/// (a share's relay/gateway tokens, a gateway boot), and the health monitor
/// keeps its own 60s refresh. A healthy login shell answers in well under a
/// second; only a hung rc file costs the full timeout, and then the fallback
/// applies. A caller on a **request path** — where that worst case is a
/// user-visible stall — wants [`cached_user_path`] instead.
pub async fn resolve_user_path() -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    if let Some(path) = login_shell_path(&shell).await {
        info!(path = %path, "resolved user PATH from login shell");
        return path;
    }
    process_path_fallback()
}

/// The value used when the login shell can't be consulted: this process's own
/// `PATH`. On a daemon that is the bare service `PATH` — the value that made
/// user CLIs unfindable in the first place — so it is a floor, not an answer,
/// and [`cached_user_path`] deliberately does not treat it as one.
fn process_path_fallback() -> String {
    match std::env::var("PATH") {
        Ok(p) if !p.is_empty() => p,
        // Never return "" — `.env("PATH", "")` would disable lookup entirely,
        // reintroducing the "command not found" failure this helper exists to
        // prevent.
        _ => "/usr/local/bin:/usr/bin:/bin".to_owned(),
    }
}

/// How long a cached [`cached_user_path`] value stays fresh. Matches the
/// health monitor's own re-resolution cadence: long enough that a burst of
/// requests costs one login shell, short enough that a user who edits their
/// `.zshrc` (or installs a version manager) doesn't have to restart the daemon
/// to be seen.
const PATH_CACHE_TTL: Duration = Duration::from_secs(60);

/// How long a *failed* resolution is remembered. Far shorter than
/// [`PATH_CACHE_TTL`], because what gets served in its place is the bare
/// process `PATH`: a transient stall must not keep answering with the broken
/// value, and the user's instinctive retry has to be able to clear it. Not
/// zero, so a shell that is reliably hanging costs one 10s resolution per
/// window rather than one per click.
const PATH_CACHE_FAILURE_TTL: Duration = Duration::from_secs(15);

/// A resolved `PATH` plus when and how it was obtained.
struct CachedPath {
    at: std::time::Instant,
    path: String,
    /// `true` when the login shell actually answered. A fallback value gets
    /// [`PATH_CACHE_FAILURE_TTL`] instead of [`PATH_CACHE_TTL`].
    from_login_shell: bool,
}

/// [`resolve_user_path`] with a process-wide cache, for callers on a request
/// path.
///
/// Resolution spawns an interactive login shell — sub-second normally, but up
/// to [`PATH_RESOLVE_TIMEOUT`] when an rc file stalls, and the machines with
/// the slowest `.zshrc` are exactly the ones (nvm, rbenv, `brew shellenv`) that
/// need the resolved PATH in the first place. Per-request resolution would put
/// that on every click of the management UI's stop/restart/action buttons and
/// Veld Desktop's start button, whose `fetch` calls carry no timeout.
///
/// A failed resolution is cached only briefly. `resolve_user_path` cannot fail
/// — it falls back to this process's `PATH`, which on a daemon is the bare
/// service one — so caching that for a full minute would make every command in
/// the window fail to find the user's tools, indistinguishably from a real
/// answer and un-clearable by retrying.
///
/// The daemon's health monitor calls this on its own scan cadence, so on a live
/// daemon a request usually finds a warm entry and never waits on a shell.
///
/// Concurrent misses may each resolve — the lock is never held across the
/// await, because a stalled shell must not serialise unrelated requests behind
/// it, and the redundant work produces the same value.
pub async fn cached_user_path() -> String {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<CachedPath>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));

    // A poisoned mutex would mean a panic while holding a `String` clone;
    // treat it as a miss rather than propagating, since the recovery is only
    // ever "resolve again".
    if let Ok(guard) = cache.lock() {
        if let Some(entry) = guard.as_ref() {
            let ttl = if entry.from_login_shell {
                PATH_CACHE_TTL
            } else {
                PATH_CACHE_FAILURE_TTL
            };
            if entry.at.elapsed() < ttl {
                return entry.path.clone();
            }
        }
    }

    // Calls `login_shell_path` rather than `resolve_user_path` so the fallback
    // is distinguishable from a real answer — the whole point of the two TTLs.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    let resolved = login_shell_path(&shell).await;
    if let Some(path) = &resolved {
        info!(path = %path, "resolved user PATH from login shell");
    }
    let from_login_shell = resolved.is_some();
    let path = resolved.unwrap_or_else(process_path_fallback);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedPath {
            at: std::time::Instant::now(),
            path: path.clone(),
            from_login_shell,
        });
    }
    path
}

/// Run `shell -l -i -c 'command env'` and extract the `PATH=` line, bounded
/// by [`PATH_RESOLVE_TIMEOUT`]. `None` on timeout, spawn failure, non-zero
/// exit, or output without a usable `PATH=` line. `command env` (not bare
/// `env`) so an `env` alias or shell function defined in an interactive rc
/// file can't shadow the real binary.
async fn login_shell_path(shell: &str) -> Option<String> {
    let mut cmd = tokio::process::Command::new(shell);
    cmd.arg("-l")
        .arg("-i")
        .arg("-c")
        .arg("command env")
        // No terminal on any fd — PATH extraction only needs stdout.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        // Kill the shell if we abandon it on timeout, so a hung `.zshrc`
        // doesn't leak a live process per resolution.
        .kill_on_drop(true);
    // The shell must have NO CONTROLLING TERMINAL, not just clean stdio: an
    // interactive (-i) zsh opens /dev/tty directly and seizes the terminal's
    // foreground process group, then exits without restoring it — leaving
    // Ctrl-C signalling a dead group. When the daemon runs foreground on a
    // tty (`just dev-daemon`), that killed Ctrl-C for the whole session,
    // re-broken by every 60s PATH re-resolution. setsid() detaches the child
    // from the session so /dev/tty does not resolve to the user's terminal.
    // (Verified by reproducing the foreground-group theft under a pty.)
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            let _ = nix::unistd::setsid();
            Ok(())
        });
    }
    let output = cmd.output();

    match tokio::time::timeout(PATH_RESOLVE_TIMEOUT, output).await {
        Ok(Ok(o)) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            // Last match wins: rc-file noise (including a debugging
            // `echo "PATH=$PATH"`) precedes the `env` dump, and `env` prints
            // each variable once. Residual ambiguity: an env value with an
            // embedded newline followed by `PATH=` would print after the real
            // PATH and win — pathological enough to accept over `env -0`
            // portability games.
            let path = stdout
                .lines()
                .rev()
                .filter_map(|l| l.strip_prefix("PATH="))
                .map(str::trim)
                .find(|p| !p.is_empty())?;
            Some(path.to_owned())
        }
        Ok(Ok(o)) => {
            debug!(
                exit_code = o.status.code(),
                "login shell PATH resolution exited non-zero, using fallback"
            );
            None
        }
        Ok(Err(e)) => {
            debug!(error = %e, "failed to resolve user PATH, using fallback");
            None
        }
        Err(_) => {
            warn!("login shell PATH resolution timed out, using fallback");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write an executable stub "shell" that ignores its `-l -i -c 'command
    /// env'` args and prints the given stdout, so the parsing path is tested
    /// without depending on the machine's real shell config.
    #[cfg(unix)]
    fn stub_shell(dir: &std::path::Path, stdout: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("stub-shell");
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{stdout}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn extracts_path_line_ignoring_rc_noise() {
        let dir = tempfile::tempdir().unwrap();
        // A chatty rc file greets on stdout before the env dump — those lines
        // must not end up inside the resolved PATH.
        let shell = stub_shell(
            dir.path(),
            "Welcome to nvm!\nHOME=/Users/dev\nPATH=/opt/secrets/bin:/usr/bin\nTERM=dumb",
        );
        let path = login_shell_path(shell.to_str().unwrap()).await;
        assert_eq!(path.as_deref(), Some("/opt/secrets/bin:/usr/bin"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_path_line_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let shell = stub_shell(dir.path(), "HOME=/Users/dev");
        assert_eq!(login_shell_path(shell.to_str().unwrap()).await, None);
    }

    // Whatever the environment (CI without a login shell, unset SHELL, a shell
    // that fails to start), the public helper must produce a non-empty PATH so
    // callers can unconditionally `.env("PATH", …)` with the result.
    #[tokio::test]
    async fn resolves_to_a_non_empty_path() {
        let path = resolve_user_path().await;
        assert!(!path.is_empty());
    }
}
