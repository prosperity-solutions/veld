//! Resolving the user's login-shell `PATH` for spawned commands.
//!
//! The daemon (launchd) and gateway (systemd) run with a bare service `PATH`,
//! so user-installed CLIs (`op`, `vault`, brew-installed tools, version
//! managers) are not found when a config-declared command is executed — even
//! though the same command works in the user's terminal. Every place veld
//! executes a user-supplied command string on a daemon must therefore inherit
//! the user's login-shell `PATH`.
//!
//! Two entry points. [`cached_user_path`] is the one anything running **inside
//! a daemon** wants — request handlers (`spawn_veld`, the desktop picker and
//! git plumbing, `SecretSource::Command` token resolution reached from
//! `POST /api/shares`), the health monitor's liveness probes, and any future
//! daemon-side command-execution surface — because resolution spawns a login
//! shell and a stalled rc file must cost one resolution, not one per call.
//! [`resolve_user_path`] is the uncached primitive, for a one-shot context that
//! resolves once and exits — today only a CLI run's lazy var sources
//! (`values.rs`), since `endpoint::resolve_secret` is shared with the daemon.
//! (Commands spawned by the `veld` CLI itself — orchestrator steps, actions —
//! already inherit the terminal's `PATH` and need neither.)
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
/// Spawns `<shell> -l -i -c 'command env'` — the user's chosen shell where one
/// has been published ([`set_preferred_shell`]), their login shell otherwise —
/// and parses the `PATH=` line, so it
/// captures
/// `PATH` after `.zprofile`/`.zshrc`/`.bash_profile`/`brew shellenv` etc. have
/// run — the value the user's own terminal would have. Parsing `env` output
/// (rather than capturing `echo $PATH`) keeps this correct for any shell —
/// fish would print `$PATH` space-separated, and a chatty rc file's greeting
/// lines don't start with `PATH=` — the environment variable itself is
/// colon-delimited regardless of shell.
///
/// Not cached — this is the primitive. A healthy login shell answers in well
/// under a second; only a hung rc file costs the full timeout, and then the
/// fallback applies. Fine for a one-shot context that resolves once and exits
/// — today only a CLI run's lazy var sources (`values.rs`). Anything running
/// inside a daemon — a request handler, a periodic scan — wants
/// [`cached_user_path`] instead, so a stalled rc file costs one resolution
/// rather than one per call. Note that a caller shared between the two, like
/// `endpoint::resolve_secret`, counts as daemon-side and uses the cache.
pub async fn resolve_user_path() -> String {
    if let Some(path) = resolve_with_fallback().await {
        info!(path = %path, "resolved user PATH from login shell");
        return path;
    }
    process_path_fallback()
}

/// Ask the preferred shell, and — only if it answered nothing — the login shell.
///
/// **A shell someone picked for their *terminals* need not be able to answer this
/// question.** `terminal.shell` can be any executable, and the picker offers what
/// `/etc/shells` lists: on stock macOS that includes `/bin/csh` and `/bin/tcsh`,
/// neither of which prints a `PATH=` line for `-l -i -c 'command env'` (measured:
/// zsh/bash/ksh/dash print one, csh/tcsh print none). Without the second attempt,
/// choosing tcsh for terminals would silently take the user's `PATH` away from
/// every *node command*, `SecretSource::Command` and health probe — and only after
/// the next daemon restart, since [`publish_value`] keeps a good value alive until
/// then, which is about the least diagnosable shape a bug can have.
///
/// The extra spawn happens only on the failing path, so the common case still
/// costs exactly one shell.
async fn resolve_with_fallback() -> Option<String> {
    let preferred = resolution_shell();
    if let Some(path) = login_shell_path(&preferred).await {
        return Some(path);
    }
    let login = crate::shell::auto_shell();
    if login == preferred {
        return None;
    }
    debug!(
        preferred = %preferred,
        falling_back_to = %login,
        "the preferred shell answered no PATH — asking the login shell"
    );
    login_shell_path(&login).await
}

/// The user's chosen shell, published by whoever knows about it.
///
/// `None` — the state in the CLI, the gateway and every test — means
/// [`crate::shell::auto_shell`], which is what this module always did (plus a
/// `passwd` fallback for a `$SHELL` that launchd never set).
fn preferred_shell() -> &'static std::sync::Mutex<Option<String>> {
    static SHELL: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
        std::sync::OnceLock::new();
    SHELL.get_or_init(|| std::sync::Mutex::new(None))
}

/// Tell this module which shell to spawn — the daemon's `terminal.shell`.
///
/// Pushed in rather than read from the database here, and that is the point: this
/// module is linked into the **gateway** (through `veld-share`'s secret
/// resolution) and into the CLI, neither of which has a user database — a
/// `Db::open()` on this path would have a gateway create and migrate a SQLite file
/// as a side effect of working out its `PATH`. The one process that has the
/// setting is the one that sets it.
///
/// Called at daemon startup and again whenever the setting is patched, so a change
/// reaches the *next* resolution rather than the next daemon restart. Values that
/// are not usable are already filtered by [`crate::shell::resolve`] upstream; an
/// empty string here is treated as `None`.
pub fn set_preferred_shell(shell: Option<String>) {
    let shell = shell.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if let Ok(mut guard) = preferred_shell().lock() {
        *guard = shell;
    }
}

/// The shell [`login_shell_path`] is spawned as.
fn resolution_shell() -> String {
    preferred_shell()
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(crate::shell::auto_shell)
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

/// How often [`refresh_user_path_cache`] re-resolves on a live daemon. The
/// published value is therefore at most this old, which is what lets a request
/// handler read it and never wait on a login shell.
const PATH_WARM_INTERVAL: Duration = Duration::from_secs(20);

/// The most recently published `PATH`. `None` until the first resolution.
///
/// One writer ([`refresh_user_path_cache`], driven by
/// [`warm_user_path_cache`]) and many readers, so there is no ordering rule to
/// get wrong: no TTL, no which-of-two-resolutions-wins, no lock held across an
/// await. An earlier revision of this cache had all three and each one grew a
/// bug — a stalled resolution reinstating the bare service `PATH` over a good
/// value, a warm task that only refreshed *after* expiry, a fallback cached as
/// though it were an answer. What replaced them is the invariant in
/// [`publish_value`].
fn cell() -> &'static std::sync::Mutex<Option<String>> {
    static CELL: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new(None))
}

/// What to store given the currently published value and a fresh resolution
/// result — `None` meaning "leave what is there".
///
/// **A resolution that learned nothing never displaces one that did.** This is
/// the whole correctness argument. `login_shell_path` returns `None` on a stall,
/// a spawn failure or a non-zero exit, and can also succeed while contributing
/// nothing (an answer byte-identical to this process's `PATH`: `SHELL` unset so
/// `sh` ran, or an rc file whose version-manager block is gated on a terminal
/// that resolution deliberately does not provide). On a daemon that unhelpful
/// value *is* the bug — the bare launchd `PATH` that cannot find `npx` — so it
/// may seed an empty cell, but it must never overwrite a real answer, however
/// old that answer is. A real answer always wins; the warm loop keeps it
/// current.
fn publish_value(current: Option<&str>, resolved: Option<String>) -> Option<String> {
    let fallback = process_path_fallback();
    match (resolved.filter(|p| *p != fallback), current) {
        (Some(helpful), _) => Some(helpful),
        (None, Some(_)) => None,
        (None, None) => Some(fallback),
    }
}

/// The published `PATH`, or `None` when nothing has been resolved yet.
///
/// The non-blocking read, for a caller that must not `await` and must not
/// trigger a resolution — a listing endpoint that runs once per worktree per
/// poll, say. `None` means "not known yet", which is a different answer from
/// "the process PATH": a caller deciding whether a tool is installed has to be
/// able to tell those apart, because the process PATH is the bare service one
/// and would report every user-installed CLI as missing.
#[must_use]
pub fn published_user_path() -> Option<String> {
    cell().lock().ok().and_then(|g| g.clone())
}

/// The user's login-shell `PATH`, read from the cache — the entry point for
/// anything running inside a daemon.
///
/// Resolution spawns an interactive login shell: sub-second normally, but up to
/// [`PATH_RESOLVE_TIMEOUT`] when an rc file stalls, and the machines with the
/// slowest `.zshrc` are exactly the ones (nvm, rbenv, `brew shellenv`) that need
/// the resolved PATH in the first place. Doing that per request would put it on
/// every click of the management UI's stop/restart/action buttons and Veld
/// Desktop's start button, whose `fetch` calls carry no timeout.
///
/// So this only ever reads the published value. It resolves inline in exactly
/// one case: nothing has been published yet, which on a daemon means the warm
/// task's first resolution has not finished, and in a short-lived CLI or gateway
/// process means this is the first call.
pub async fn cached_user_path() -> String {
    if let Some(published) = published_user_path() {
        return published;
    }
    // Cold. Two callers racing here both resolve and both publish a real
    // answer, which is harmless — there is no wrong winner between two truthful
    // resolutions, and single-flighting it would reintroduce a lock to hold
    // across an await.
    refresh_user_path_cache().await;
    cell()
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(process_path_fallback)
}

/// Re-resolve and publish per [`publish_value`].
async fn refresh_user_path_cache() {
    let resolved = resolve_with_fallback().await;
    if let Some(path) = &resolved {
        info!(path = %path, "resolved user PATH from login shell");
    }

    // Poisoned mutex: nothing to publish into and nothing to serve from, so the
    // caller falls back. Never panics the warm loop.
    let Ok(mut guard) = cell().lock() else {
        return;
    };
    let decided = publish_value(guard.as_deref(), resolved);
    // Warn only when the cell is *left* holding an unhelpful value — i.e. this
    // resolution seeded the fallback, or it learned nothing and there was
    // nothing better already there. Rate-limited, because the warm loop retries
    // forever and a permanently broken shell would otherwise log every 20s.
    let unhelpful = guard
        .as_deref()
        .is_none_or(|p| p == process_path_fallback())
        && decided
            .as_deref()
            .is_none_or(|p| p == process_path_fallback());
    if unhelpful {
        warn_unhelpful_path();
    }
    if let Some(path) = decided {
        *guard = Some(path);
    }
}

/// At most one "PATH resolution learned nothing" warning per
/// [`UNHELPFUL_WARN_INTERVAL`], with the first one immediate.
///
/// Once-per-process would be worse than it sounds: a daemon runs for weeks, so a
/// shell that breaks at hour 300 — or was broken from the start — would leave
/// nothing in the recent log for whoever is debugging "npx: command not found".
fn warn_unhelpful_path() {
    const UNHELPFUL_WARN_INTERVAL: Duration = Duration::from_secs(600);
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

    let Ok(mut last) = LAST.lock() else {
        return;
    };
    let now = std::time::Instant::now();
    let due = last.is_none_or(|t| now.saturating_duration_since(t) >= UNHELPFUL_WARN_INTERVAL);
    if !due {
        debug!("user PATH resolution still contributing nothing (warning suppressed)");
        return;
    }
    *last = Some(now);
    warn!(
        "user PATH resolution contributed nothing over this process's own PATH — \
         user-installed CLIs may not be found"
    );
}

/// Keep the published `PATH` current, so no request handler pays for a
/// resolution.
///
/// A dedicated task rather than a piggyback on the health monitor's scan: that
/// loop awaits per-node liveness probes (30s each) and `veld restart` recovery
/// (up to 300s), so it cannot be relied on to refresh anything on a cadence.
/// Spawn once at daemon start; never returns. The first tick fires immediately,
/// so the cell is populated within one resolution of boot.
pub async fn warm_user_path_cache() {
    let mut interval = tokio::time::interval(PATH_WARM_INTERVAL);
    // A daemon host that slept overnight must not wake to a queue of missed
    // ticks, each spawning a login shell.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        refresh_user_path_cache().await;
    }
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

    /// Serialises the tests that touch the process-wide published value, so one
    /// test's publish can't be read as another's.
    fn cache_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    // Same non-empty guarantee through the cached entry point, which resolves by
    // a different route (`login_shell_path` directly, so it can tell a real
    // answer from the fallback). Cleared first so this exercises the cold path
    // rather than reading whatever a sibling published.
    #[tokio::test]
    async fn cached_resolves_to_a_non_empty_path() {
        let _serialised = cache_test_lock().lock().await;
        *cell().lock().expect("cell mutex") = None;
        assert!(!cached_user_path().await.is_empty());
    }

    // The correctness argument of the whole cache: a resolution that learned
    // nothing must never displace one that did. `None` here means "leave what is
    // there" — see `publish_value`.
    #[test]
    fn an_unhelpful_resolution_never_displaces_a_real_one() {
        let good = "/opt/homebrew/bin:/usr/bin:/bin";
        let bare = process_path_fallback();

        // A stall (or spawn failure, or non-zero exit) keeps the good value…
        assert_eq!(publish_value(Some(good), None), None);
        // …and so does a shell that answers with nothing better than this
        // process's own PATH, which on a daemon is the bug being fixed.
        assert_eq!(publish_value(Some(good), Some(bare.clone())), None);
        // A real answer always wins, including over a previously seeded fallback.
        assert_eq!(
            publish_value(Some(&bare), Some(good.to_owned())),
            Some(good.to_owned())
        );
        assert_eq!(
            publish_value(Some(good), Some(good.to_owned())),
            Some(good.to_owned())
        );
    }

    // An empty cell is the one case where the fallback is worth publishing:
    // something must be served, and it is what the process would have used
    // anyway. This is also what keeps a headless host (no user shell config)
    // from resolving on every single read.
    #[test]
    fn an_empty_cell_is_seeded_with_the_process_path() {
        let bare = process_path_fallback();
        assert_eq!(publish_value(None, None), Some(bare.clone()));
        assert_eq!(publish_value(None, Some(bare.clone())), Some(bare));
    }

    // A published value is read back without resolving — the property that keeps
    // a stalled `.zshrc` off the request path.
    #[tokio::test]
    async fn a_published_value_is_read_without_resolving() {
        let _serialised = cache_test_lock().lock().await;
        *cell().lock().expect("cell mutex") = Some("/published/only".to_owned());
        assert_eq!(cached_user_path().await, "/published/only");
    }
}
