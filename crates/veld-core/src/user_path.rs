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
/// Spawns `$SHELL -l -i -c 'command env'` and parses the `PATH=` line, so it
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

/// How long a cached *authoritative* [`cached_user_path`] value stays fresh:
/// long enough that a burst of requests costs one login shell, short enough
/// that a user who edits their `.zshrc` (or installs a version manager) doesn't
/// have to restart the daemon to be seen.
const PATH_CACHE_TTL: Duration = Duration::from_secs(60);

/// How long a *non-authoritative* resolution is remembered. Shorter than
/// [`PATH_CACHE_TTL`], because what gets served in its place is the bare
/// process `PATH`: a transient stall must not keep answering with the broken
/// value long after a good answer became available.
const PATH_CACHE_FAILURE_TTL: Duration = Duration::from_secs(30);

/// How often [`refresh_user_path_cache`] re-resolves on a live daemon.
///
/// Shorter than **both** TTLs — see the assertion below — which is what makes
/// "a request handler never waits on a login shell" true rather than usually
/// true: the entry is replaced before it can expire, so a handler's read is a
/// hit even in the non-authoritative case.
const PATH_WARM_INTERVAL: Duration = Duration::from_secs(20);

// The warm cadence must beat both expiries, or a handler arriving in the gap
// resolves inline (up to PATH_RESOLVE_TIMEOUT) — which is the whole thing the
// cache exists to prevent. Cheap to state, easy to break by editing one
// constant, so state it.
const _: () = assert!(PATH_WARM_INTERVAL.as_secs() < PATH_CACHE_FAILURE_TTL.as_secs());
const _: () = assert!(PATH_WARM_INTERVAL.as_secs() < PATH_CACHE_TTL.as_secs());
// A refresh must also be able to finish inside its own interval, or the warm
// task falls behind its cadence on a stalled host.
const _: () = assert!(PATH_RESOLVE_TIMEOUT.as_secs() <= PATH_WARM_INTERVAL.as_secs());

/// A resolved `PATH` plus when and how it was obtained.
struct CachedPath {
    at: std::time::Instant,
    path: String,
    /// Whether this value actually came from the user's shell config, as
    /// opposed to being the process `PATH` under another name. Governs which
    /// of the two TTLs applies.
    authoritative: bool,
}

impl CachedPath {
    fn is_fresh_at(&self, now: std::time::Instant) -> bool {
        let ttl = if self.authoritative {
            PATH_CACHE_TTL
        } else {
            PATH_CACHE_FAILURE_TTL
        };
        now.saturating_duration_since(self.at) < ttl
    }
}

/// The cached entry. `None` until the first resolution completes.
fn cache() -> &'static std::sync::Mutex<Option<CachedPath>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<CachedPath>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Held for the duration of a resolution, so at most one login shell runs at a
/// time process-wide.
///
/// Without it, the warm task and a handler both hitting a cold entry spawn two
/// concurrent login shells (`tokio::time::interval` fires its first tick
/// immediately, so this happens at every daemon start), and two resolutions can
/// finish out of order — a 10s stall landing on top of a 200ms success,
/// reinstating the bare service `PATH` over a good value already in hand. One
/// resolution at a time makes writes totally ordered, so the store needs no
/// which-one-wins rule at all.
fn resolve_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// The cached value, whether or not it is still fresh. A poisoned mutex reads
/// as empty rather than propagating: the recovery is only ever "resolve again".
fn cached(require_fresh: bool) -> Option<String> {
    let guard = cache().lock().ok()?;
    let entry = guard.as_ref()?;
    (!require_fresh || entry.is_fresh_at(std::time::Instant::now())).then(|| entry.path.clone())
}

/// [`resolve_user_path`] with a process-wide cache — the entry point for
/// anything running inside a daemon.
///
/// Resolution spawns an interactive login shell — sub-second normally, but up
/// to [`PATH_RESOLVE_TIMEOUT`] when an rc file stalls, and the machines with
/// the slowest `.zshrc` are exactly the ones (nvm, rbenv, `brew shellenv`) that
/// need the resolved PATH in the first place. Per-request resolution would put
/// that on every click of the management UI's stop/restart/action buttons and
/// Veld Desktop's start button, whose `fetch` calls carry no timeout.
///
/// A non-authoritative resolution is cached only briefly. `resolve_user_path`
/// cannot fail — it falls back to this process's `PATH`, which on a daemon is
/// the bare service one — so caching that for a full minute would make every
/// command in the window fail to find the user's tools, indistinguishably from
/// a real answer and un-clearable by retrying. "Authoritative" is decided on
/// content: a login shell whose answer is byte-identical to the process `PATH`
/// demonstrably contributed nothing. That test only catches exact equality — a
/// shell that adds *something* while still missing the version-manager shims
/// (`SHELL` unset, so `sh -l -i` sources `/etc/profile` and `~/.profile` but no
/// `.zshrc`) reads as authoritative, so this narrows the failure window rather
/// than closing it.
///
/// **Never waits on a login shell when a value exists.** A fresh entry is
/// returned outright; a stale one is returned while a refresh runs behind it
/// (serve-stale-while-revalidate), because a stalled `.zshrc` must not become a
/// stalled click. Only the very first call on a cold cache can wait, and on a
/// daemon [`refresh_user_path_cache`] has already run by then.
pub async fn cached_user_path() -> String {
    if let Some(fresh) = cached(true) {
        return fresh;
    }

    match resolve_lock().try_lock() {
        Ok(_guard) => {
            // Re-read under the lock: a refresh may have completed between the
            // miss above and acquiring it, in which case that is the answer.
            if let Some(fresh) = cached(true) {
                return fresh;
            }
            resolve_and_store().await
        }
        // A resolution is already in flight. Serving the stale value beats
        // queueing behind a login shell that may take the full timeout — the
        // refresh that is already running will publish the new value.
        Err(_) => match cached(false) {
            Some(stale) => stale,
            // Nothing cached at all, so there is nothing to serve: wait for the
            // in-flight resolution rather than starting a second one.
            None => {
                let _guard = resolve_lock().lock().await;
                cached(false).unwrap_or_else(process_path_fallback)
            }
        },
    }
}

/// Re-resolve and publish, unconditionally. Call site: [`warm_user_path_cache`].
///
/// Deliberately *not* `cached_user_path()`, which returns early while the entry
/// is fresh — a warm task built on that would only ever act on an already
/// expired entry, leaving exactly the resolve-inline window it exists to close.
pub async fn refresh_user_path_cache() {
    let _guard = resolve_lock().lock().await;
    resolve_and_store().await;
}

/// Resolve and store. The caller must hold [`resolve_lock`], which is what makes
/// writes totally ordered and lets the store be an unconditional overwrite.
async fn resolve_and_store() -> String {
    // `login_shell_path` rather than `resolve_user_path` so the fallback is
    // distinguishable from a real answer — the whole point of the two TTLs.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    let resolved = login_shell_path(&shell).await;
    if let Some(path) = &resolved {
        info!(path = %path, "resolved user PATH from login shell");
    }
    let fallback = process_path_fallback();
    let authoritative = resolved.as_ref().is_some_and(|p| *p != fallback);

    // Once per streak, not once per resolution: the warm task re-resolves every
    // PATH_WARM_INTERVAL forever, and on a host with no user shell config — the
    // gateway container's normal state — every one of those is non-authoritative.
    // Warning each time would be noise, not a diagnosis.
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    use std::sync::atomic::Ordering;
    if authoritative {
        WARNED.store(false, Ordering::Relaxed);
    } else if !WARNED.swap(true, Ordering::Relaxed) {
        warn!(
            "user PATH resolution contributed nothing over this process's own PATH — \
             user-installed CLIs may not be found"
        );
    }

    let path = resolved.unwrap_or(fallback);
    if let Ok(mut guard) = cache().lock() {
        *guard = Some(CachedPath {
            at: std::time::Instant::now(),
            path: path.clone(),
            authoritative,
        });
    }
    path
}

/// Keep the [`cached_user_path`] entry warm, so no request handler ever pays for
/// a resolution.
///
/// A dedicated task rather than a piggyback on the health monitor's scan: that
/// loop awaits per-node liveness probes (30s each) and `veld restart` recovery
/// (up to 300s), so its cadence is not something a cache TTL can be pinned to —
/// during one recovery the entry would expire and the next click would resolve
/// inline. Spawn once at daemon start; never returns.
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

    /// Serialises the tests that touch the process-wide cache. The resolve lock
    /// cannot do this job: `cached_user_path` treats it as held-by-someone-else
    /// and serves a value rather than blocking, which is exactly the behaviour
    /// under test.
    fn cache_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    // Same guarantee through the cached entry point, which resolves by a
    // different route (`login_shell_path` directly, so it can tell a real answer
    // from the fallback).
    #[tokio::test]
    async fn cached_resolves_to_a_non_empty_path() {
        let _serialised = cache_test_lock().lock().await;
        assert!(!cached_user_path().await.is_empty());
    }

    fn entry(age: Duration, authoritative: bool) -> CachedPath {
        CachedPath {
            // Instants can't be constructed from nothing; subtract from now.
            at: std::time::Instant::now() - age,
            path: "/from/cache".to_owned(),
            authoritative,
        }
    }

    #[test]
    fn the_two_ttls_apply_by_authority() {
        let now = std::time::Instant::now();
        // Ages chosen clear of both boundaries — `entry()` reads the clock again,
        // so an age equal to the TTL lands within measurement noise of it.
        assert!(entry(Duration::from_secs(30), true).is_fresh_at(now));
        assert!(!entry(Duration::from_secs(90), true).is_fresh_at(now));
        // A fallback value expires sooner, so a transient stall can't keep
        // serving the bare process PATH for a whole minute.
        assert!(entry(Duration::from_secs(5), false).is_fresh_at(now));
        assert!(!entry(Duration::from_secs(45), false).is_fresh_at(now));
    }

    // The load-bearing relationship between the constants: the warm cadence
    // must beat both expiries, or a handler arriving in the gap resolves
    // inline — the stall the cache exists to prevent. Also asserted at compile
    // time; restated here so a failure names the invariant.
    #[test]
    fn the_warm_cadence_beats_both_expiries() {
        assert!(PATH_WARM_INTERVAL < PATH_CACHE_FAILURE_TTL);
        assert!(PATH_WARM_INTERVAL < PATH_CACHE_TTL);
        assert!(PATH_RESOLVE_TIMEOUT <= PATH_WARM_INTERVAL);
    }

    // Serve-stale-while-revalidate: with a resolution in flight, a caller takes
    // the stale value instead of queueing behind a login shell. Pinned by
    // holding the resolve lock — the state a slow `.zshrc` produces.
    #[tokio::test]
    async fn a_stale_entry_is_served_while_a_resolution_is_in_flight() {
        // This test owns the process-wide cache for its duration: a sibling
        // resolution completing mid-test would publish a *fresh* entry over the
        // stale one planted below and the read would legitimately return that.
        let _serialised = cache_test_lock().lock().await;
        let held = resolve_lock().lock().await;
        if let Ok(mut guard) = cache().lock() {
            *guard = Some(CachedPath {
                // Older than either TTL, so the read path treats it as a miss.
                at: std::time::Instant::now() - Duration::from_secs(600),
                path: "/stale/but/serviceable".to_owned(),
                authoritative: true,
            });
        }
        // Returns immediately with the stale value rather than resolving.
        assert_eq!(cached_user_path().await, "/stale/but/serviceable");
        drop(held);

        // With the lock free, the next call resolves and replaces it.
        assert_ne!(cached_user_path().await, "/stale/but/serviceable");
    }
}
