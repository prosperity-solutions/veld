//! Resolving the user's login-shell `PATH` for spawned commands.
//!
//! The daemon (launchd) and gateway (systemd) run with a bare service `PATH`,
//! so user-installed CLIs (`op`, `vault`, brew-installed tools, version
//! managers) are not found when a config-declared command is executed — even
//! though the same command works in the user's terminal. Every place veld
//! executes a user-supplied command string on a daemon must therefore inherit
//! the user's login-shell `PATH`.
//!
//! Three entry points. [`cached_user_path`] is the one anything running
//! **inside a daemon** wants when the answer is directory-independent —
//! `SecretSource::Command` token resolution reached from `POST /api/shares`,
//! the desktop picker and git plumbing — because resolution spawns a login
//! shell and a stalled rc file must cost one resolution, not one per call.
//! [`cached_user_path_for`] is the project-directory-scoped sibling, and the
//! default for anything spawning *a project's own declared command*:
//! `spawn_veld` (the start/stop/restart/action handlers), the health
//! monitor's `command`/`bash` liveness probes and recovery restarts, an
//! `ide.extensions` command, a pane's `requires_bin` preflight. These need
//! the `PATH` a login shell would have *in that project's directory*,
//! because a version manager's directory-based Node switch (an `.nvmrc` +
//! `.zshrc` block keyed on `$PWD`) answers differently per project — a
//! single process-wide cache structurally cannot represent that, however
//! fresh. [`resolve_user_path`] is the uncached primitive, for a one-shot
//! context that resolves once and exits — today only a CLI run's lazy var
//! sources (`values.rs`), since `endpoint::resolve_secret` is shared with
//! the daemon. (Commands spawned by the `veld` CLI itself — orchestrator
//! steps, actions — already inherit the terminal's `PATH` and need neither.)
//!
//! **How fresh the published value is.** Resolving spawns an interactive login
//! shell (0.8-1.5 s of wall time and a burst of CPU on a machine with a
//! version-manager rc file), so the daemon does not do it on a fixed timer. The
//! warm loop ticks every [`PATH_WARM_INTERVAL`] but only spawns when something
//! says the answer may have changed: a shell startup file's mtime or length
//! moved ([`startup_files`]), the shell being asked changed, or
//! [`PATH_MAX_AGE`] has passed since the last helpful answer. The per-directory
//! entries are held for [`DIR_PATH_TTL`] under the same startup-file rule.
//! The resulting staleness bounds:
//!
//! - an edit to a listed startup file, or a change of the configured shell, is
//!   picked up within one tick (20 s) for the global value, and on the next
//!   request for a project directory's (served stale-while-revalidate, so no
//!   caller waits on it);
//! - anything a `stat` of those files cannot see — a file *sourced by* an rc
//!   file, a `brew shellenv` change, an `.nvmrc` edit, a version manager's new
//!   default — is picked up within [`PATH_MAX_AGE`] (5 min, plus up to one
//!   tick) for the global value and [`DIR_PATH_TTL`] (2 min) for a project
//!   directory's;
//! - a resolution that learned nothing (a stall, a failed spawn) is retried on
//!   the next tick, exactly as before.
//!
//! Unlike the daemon's repo fingerprint there is no settle window: the stamp is
//! taken before the shell runs, which catches an edit landing mid-resolution on
//! APFS/ext4's nanosecond mtimes. On a filesystem with whole-second mtimes, a
//! same-length edit within the second of the stamp waits for [`PATH_MAX_AGE`].
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
    if let Some(path) = resolve_with_fallback(&resolution_shell(), None).await {
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
///
/// `cwd` is forwarded to both attempts: `None` for the directory-independent
/// callers ([`resolve_user_path`], [`cached_user_path`]'s warm loop), `Some`
/// for a project-scoped resolution ([`cached_user_path_for`]) so a directory-
/// keyed version-manager hook in the rc file sees the right `$PWD`.
///
/// `preferred` is passed in rather than read from [`resolution_shell`] here, and
/// that is the point: it lets this module's own tests exercise resolution with a
/// stub shell **without publishing that stub into the process-global
/// [`preferred_shell`]**, which every other test in the crate reads. Issue #310
/// was exactly that leak — a stub answering `PATH=/old/bin` made `cat` and `sh`
/// unspawnable for whichever `values` test happened to run alongside it, so the
/// crate's suite was permanently a few tests red locally.
async fn resolve_with_fallback(preferred: &str, cwd: Option<&std::path::Path>) -> Option<String> {
    if let Some(path) = login_shell_path(preferred, cwd).await {
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
    login_shell_path(&login, cwd).await
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
///
/// **The daemon is the only caller — tests must not use this.** Publishing here
/// changes what every *other* concurrently-running test in the crate resolves;
/// a test that needs a stub shell passes it as an argument instead
/// (see [`resolve_with_fallback`]).
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

/// How often the warm loop *looks* at whether a re-resolution is due — a handful
/// of `stat`s, not a login shell. A startup-file edit is therefore seen within
/// this long; see [`resolve_due`] for when it actually spawns.
const PATH_WARM_INTERVAL: Duration = Duration::from_secs(20);

/// The longest the global published value goes without a re-resolution, whatever
/// the startup files say. Backstop for changes a `stat` cannot see: a file an rc
/// file sources, a version manager's default, `brew shellenv` output.
const PATH_MAX_AGE: Duration = Duration::from_secs(300);

/// How long a per-directory entry is served as fresh before the next hit starts
/// a background refresh. Longer than the global tick because the monitor asks
/// for every live run's directory every 5 s and each stale hit costs a login
/// shell; a startup-file change still invalidates it at once.
const DIR_PATH_TTL: Duration = Duration::from_secs(120);

/// The timing and inputs that decide whether a cached `PATH` is still believed.
///
/// A value rather than constants so the tests can run the real decision logic
/// with a zero TTL and a fixture startup file instead of subtracting from
/// `Instant::now()` — which panics on a host booted less than that long ago.
#[derive(Debug, Clone)]
struct Policy {
    /// See [`PATH_MAX_AGE`].
    max_age: Duration,
    /// See [`DIR_PATH_TTL`].
    dir_ttl: Duration,
    /// The files whose change means the answer may differ.
    startup_files: Vec<std::path::PathBuf>,
}

impl Policy {
    /// The production policy: real durations, the real user's startup files.
    fn live() -> Self {
        Self {
            max_age: PATH_MAX_AGE,
            dir_ttl: DIR_PATH_TTL,
            startup_files: startup_files(
                dirs::home_dir().as_deref(),
                std::env::var_os("ZDOTDIR")
                    .filter(|v| !v.is_empty())
                    .map(std::path::PathBuf::from)
                    .as_deref(),
            ),
        }
    }

    /// What the inputs look like now, for comparison with what a resolution saw.
    /// A few `stat`s; fine on the async path, same as the other small
    /// filesystem reads this module's callers already do.
    fn fingerprint(&self, shell: &str) -> Fingerprint {
        Fingerprint {
            shell: shell.to_owned(),
            files: self
                .startup_files
                .iter()
                .map(|p| {
                    std::fs::metadata(p)
                        .ok()
                        .map(|m| (m.modified().ok(), m.len()))
                })
                .collect(),
        }
    }
}

/// The shell startup files a login+interactive shell may read, for zsh, bash and
/// fish, plus macOS `path_helper`'s inputs. `zdotdir` (when set) replaces `$HOME`
/// for zsh's four user files, as it does for zsh itself. Missing files are listed
/// too: creating one is a change. Two directories are stamped as directories
/// (fish's `conf.d/`, `/etc/paths.d/`), which sees a file added or removed there
/// but not one edited in place.
///
/// Known misses, which fall back to [`PATH_MAX_AGE`]: a `ZDOTDIR` set only inside
/// `~/.zshenv` (the daemon's own environment does not carry it, so the files under
/// it are not the ones listed), and fish under a non-default `XDG_CONFIG_HOME`.
fn startup_files(
    home: Option<&std::path::Path>,
    zdotdir: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(zdot) = zdotdir.or(home) {
        for f in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            out.push(zdot.join(f));
        }
    }
    if let Some(home) = home {
        for f in [".bashrc", ".bash_profile", ".profile", ".bash_login"] {
            out.push(home.join(f));
        }
        out.push(home.join(".config/fish/config.fish"));
        out.push(home.join(".config/fish/conf.d"));
    }
    for f in [
        "/etc/zshenv",
        "/etc/zprofile",
        "/etc/zshrc",
        "/etc/profile",
        "/etc/bashrc",
        "/etc/paths",
        "/etc/paths.d",
    ] {
        out.push(std::path::PathBuf::from(f));
    }
    out
}

/// What a resolution's inputs looked like: the shell it was asked and each
/// startup file's `(mtime, length)`, `None` for absent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    shell: String,
    files: Vec<Option<(Option<std::time::SystemTime>, u64)>>,
}

/// Whether the global warm loop should spawn a login shell this tick: nothing
/// helpful resolved yet, the inputs changed, or the answer is `max_age` old.
fn resolve_due(
    last: Option<(std::time::Instant, &Fingerprint)>,
    now: std::time::Instant,
    current: &Fingerprint,
    max_age: Duration,
) -> bool {
    match last {
        None => true,
        Some((at, seen)) => seen != current || now.saturating_duration_since(at) >= max_age,
    }
}

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
/// nothing (an answer byte-identical to this process's `PATH`: no user shell
/// config at all, or an rc file whose version-manager block is gated on a terminal
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
/// anything running inside a daemon whose PATH need is directory-independent
/// (the desktop picker, git plumbing, `SecretSource::Command`). A caller
/// spawning a specific project's own declared command wants
/// [`cached_user_path_for`] instead — that includes the management UI's
/// stop/restart/action/start buttons and Veld Desktop's start button, which
/// used to read this cache and no longer do.
///
/// Resolution spawns an interactive login shell: sub-second normally, but up to
/// [`PATH_RESOLVE_TIMEOUT`] when an rc file stalls, and the machines with the
/// slowest `.zshrc` are exactly the ones (nvm, rbenv, `brew shellenv`) that need
/// the resolved PATH in the first place. Doing that per request would put it on
/// every click of a `fetch` with no timeout — the same reasoning
/// [`cached_user_path_for`] extends per directory.
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

/// Directory-scoped sibling of [`cached_user_path`], for a caller that needs
/// the `PATH` a login shell would have *in a specific project directory* —
/// today `spawn_veld`, which runs a config's declared commands and must
/// resolve the same Node/Ruby/etc. version a directory-based version-manager
/// hook (`.nvmrc` + `.zshrc`, fnm's directory hook) would pick in a real
/// terminal sitting in that project.
///
/// Cached per directory for [`DIR_PATH_TTL`] (or until a shell startup file
/// changes), with no per-directory
/// timer: the set of project directories a daemon serves is every worktree it
/// knows about, and proactively refreshing all of them on a schedule would
/// multiply the login-shell spawns the cache exists to bound. Instead, a
/// **stale hit is served immediately and refreshed in the background** —
/// only a directory with *no* entry yet blocks its caller on a resolution,
/// the same one-time cost [`cached_user_path`] pays on its very first call
/// (and, per the daemon's own boot sequence, one it pays only for a project
/// created since the daemon last started — `main.rs` warms every registered
/// project's entry at boot). Serving a stale value inline (rather than
/// resolving inline, as an earlier version of this function did) is
/// load-bearing, not an optimization: a version-managed rc file is slow
/// precisely on the machines that need this cache, and `spawn_veld`'s callers
/// are UI `fetch`es with no timeout — the exact click-hang [`cached_user_path`]'s
/// own warm task exists to prevent.
///
/// At most one background refresh runs per directory at a time — a stale
/// directory's [`DirEntry::refreshing`] flag is set before the lock is
/// released, so a flood of stale hits for the same directory (several nodes'
/// liveness checks in one scan tick, concurrent HTTP calls) piggyback on the
/// one in flight rather than each spawning their own login shell.
pub async fn cached_user_path_for(project_root: &std::path::Path) -> String {
    cached_user_path_for_with_shell(&resolution_shell(), project_root).await
}

/// [`cached_user_path_for`] with the resolution shell handed in — see
/// [`resolve_with_fallback`] for why that is a parameter and not a global read.
async fn cached_user_path_for_with_shell(shell: &str, project_root: &std::path::Path) -> String {
    cached_user_path_for_with(shell, project_root, &Policy::live()).await
}

/// [`cached_user_path_for_with_shell`] with the [`Policy`] handed in.
async fn cached_user_path_for_with(
    shell: &str,
    project_root: &std::path::Path,
    policy: &Policy,
) -> String {
    // Normalized so distinct spellings of the same physical directory
    // (`./x` vs `x`, a trailing slash, a symlink) don't fragment the cache
    // into separate entries that each pay their own login-shell cost. Falls
    // back to the given path if it can't be resolved (e.g. gone since the
    // caller looked it up) — the resolution below will then simply fail to
    // spawn in it, same as any other bad cwd.
    let key = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_owned());
    let now = std::time::Instant::now();
    let fingerprint = policy.fingerprint(shell);

    // What to do about the current entry, decided entirely inside this block
    // — `guard`'s lexical scope ends at the closing brace, before any
    // `.await` runs, because a `MutexGuard` held across an await is a
    // deadlock waiting to happen (a `drop(guard)` at the same nesting level
    // as the binding does not reliably satisfy clippy's `await_holding_lock`
    // — the block boundary does).
    enum Decision {
        UseCachedValue(String),
        StartBackgroundRefresh(String, tokio::runtime::Handle),
        ResolveColdInline,
        LockPoisoned,
    }

    let decision = 'block: {
        let Ok(mut guard) = dir_cell().lock() else {
            break 'block Decision::LockPoisoned;
        };
        match guard.get(&key).map(|e| {
            (
                e.value.clone(),
                e.at,
                e.refreshing,
                e.fingerprint == fingerprint,
            )
        }) {
            None => Decision::ResolveColdInline,
            // Fresh: inside the TTL *and* resolved against the startup files
            // as they are now.
            Some((value, at, _, true)) if now.saturating_duration_since(at) < policy.dir_ttl => {
                Decision::UseCachedValue(value)
            }
            // Already being refreshed by another caller — serve the stale
            // value rather than piling on a second concurrent login shell.
            Some((value, _, true, _)) => Decision::UseCachedValue(value),
            Some((value, _, false, _)) => match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    if let Some(e) = guard.get_mut(&key) {
                        e.refreshing = true;
                    }
                    Decision::StartBackgroundRefresh(value, handle)
                }
                // No runtime to spawn a background refresh into (unreachable
                // from any daemon call site today — this fn is only ever
                // `.await`ed from inside one — but this module is linked
                // into the CLI and gateway too). Serve the stale value; the
                // next call tries again.
                Err(_) => Decision::UseCachedValue(value),
            },
        }
    };

    match decision {
        Decision::LockPoisoned => {
            return resolve_with_fallback(shell, Some(&key))
                .await
                .unwrap_or_else(process_path_fallback);
        }
        Decision::UseCachedValue(value) => value,
        Decision::StartBackgroundRefresh(value, handle) => {
            let dir = key.clone();
            let shell = shell.to_owned();
            let policy = policy.clone();
            handle.spawn(async move {
                refresh_dir_cache(&shell, &dir, &policy).await;
            });
            value
        }
        Decision::ResolveColdInline => refresh_dir_cache(shell, &key, policy).await,
    }
}

/// Re-resolve `project_root`'s entry and publish per [`publish_value`] — the
/// per-directory analogue of [`refresh_user_path_cache`], sharing its central
/// rule (**a resolution that learned nothing never displaces one that did**)
/// and its unhelpful-resolution log line. Always finishes by clearing
/// [`DirEntry::refreshing`], whether or not this call set it — a cold call
/// (no prior entry) has nothing to clear, and that's fine. Returns the value
/// now published (the fresh resolution, or the preserved existing one).
async fn refresh_dir_cache(shell: &str, project_root: &std::path::Path, policy: &Policy) -> String {
    // Fingerprinted *before* the shell runs: an edit landing while it is
    // resolving then differs from what is recorded and triggers one more
    // refresh, rather than being papered over.
    let fingerprint = policy.fingerprint(shell);
    let resolved = resolve_with_fallback(shell, Some(project_root)).await;
    if let Some(path) = &resolved {
        info!(path = %path, dir = %project_root.display(), "resolved project-directory user PATH from login shell");
    }
    let Ok(mut guard) = dir_cell().lock() else {
        return resolved.unwrap_or_else(process_path_fallback);
    };
    let current = guard.get(project_root).map(|e| e.value.clone());
    let decided = publish_value(current.as_deref(), resolved);
    // Warn only when the entry is *left* holding an unhelpful value, same
    // condition `refresh_user_path_cache` uses for the global cache — this
    // directory-scoped cache must not go silent on the exact bug class it
    // exists to catch just because most callers moved off the global one.
    let unhelpful = current
        .as_deref()
        .is_none_or(|p| p == process_path_fallback())
        && decided
            .as_deref()
            .is_none_or(|p| p == process_path_fallback());
    if unhelpful {
        warn_unhelpful_path();
    }
    let value = decided.unwrap_or_else(|| current.unwrap_or_else(process_path_fallback));
    guard.insert(
        project_root.to_owned(),
        DirEntry {
            value: value.clone(),
            at: std::time::Instant::now(),
            fingerprint,
            refreshing: false,
        },
    );
    value
}

/// One directory's published `PATH`, on the same "never overwrite a real
/// answer with an unhelpful one" invariant as [`cell`] — see [`publish_value`].
struct DirEntry {
    value: String,
    at: std::time::Instant,
    /// The shell and startup files this value was resolved against; a mismatch
    /// with the current ones makes the entry stale whatever its age.
    fingerprint: Fingerprint,
    /// Set while a background refresh for this directory is in flight, so a
    /// second stale hit serves the current value instead of spawning a
    /// second login shell for the same directory. Cleared unconditionally
    /// when [`refresh_dir_cache`] republishes.
    refreshing: bool,
}

/// One published `PATH` per project directory. Unbounded, like the set of
/// worktrees a daemon serves: realistically dozens of short strings for the
/// life of a daemon process, not a growth path worth an eviction policy.
fn dir_cell() -> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, DirEntry>>
{
    static CELL: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, DirEntry>>,
    > = std::sync::OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Re-resolve and publish per [`publish_value`]. Returns whether the shell
/// produced a helpful answer (one that differs from the process `PATH`) — the
/// warm loop only treats the value as freshly resolved, and so waits out
/// [`PATH_MAX_AGE`], when it did.
async fn refresh_user_path_cache() -> bool {
    let resolved = resolve_with_fallback(&resolution_shell(), None).await;
    if let Some(path) = &resolved {
        info!(path = %path, "resolved user PATH from login shell");
    }

    // Poisoned mutex: nothing to publish into and nothing to serve from, so the
    // caller falls back. Never panics the warm loop.
    let Ok(mut guard) = cell().lock() else {
        return false;
    };
    let helpful = resolved
        .as_deref()
        .is_some_and(|p| p != process_path_fallback());
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
    helpful
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
/// so the cell is populated within one resolution of boot. It ticks every
/// [`PATH_WARM_INTERVAL`] but only spawns a login shell when [`resolve_due`] says
/// so — see the module docs for the staleness bounds.
pub async fn warm_user_path_cache() {
    let mut last: Option<(std::time::Instant, Fingerprint)> = None;
    let mut interval = tokio::time::interval(PATH_WARM_INTERVAL);
    // A daemon host that slept overnight must not wake to a queue of missed
    // ticks, each spawning a login shell.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let policy = Policy::live();
        let fingerprint = policy.fingerprint(&resolution_shell());
        let now = std::time::Instant::now();
        if resolve_due(
            last.as_ref().map(|(at, fp)| (*at, fp)),
            now,
            &fingerprint,
            policy.max_age,
        ) {
            // A resolution that learned nothing is not remembered, so it is
            // retried next tick as it always was: a stall right after a wake must
            // not leave the bare service `PATH` in place for five minutes.
            last = refresh_user_path_cache()
                .await
                .then_some((now, fingerprint));
        }
    }
}

/// Run `shell -l -i -c 'command env'` and extract the `PATH=` line, bounded
/// by [`PATH_RESOLVE_TIMEOUT`]. `None` on timeout, spawn failure, non-zero
/// exit, or output without a usable `PATH=` line. `command env` (not bare
/// `env`) so an `env` alias or shell function defined in an interactive rc
/// file can't shadow the real binary.
///
/// `cwd`, when given, becomes the shell's working directory before its rc
/// files run — the same lever [`cached_user_path_for`] uses to reach a
/// directory-keyed version-manager hook. `None` leaves it wherever the
/// daemon process itself started.
async fn login_shell_path(shell: &str, cwd: Option<&std::path::Path>) -> Option<String> {
    let mut cmd = tokio::process::Command::new(shell);
    cmd.arg("-l").arg("-i").arg("-c").arg("command env");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd
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
    // re-broken by every PATH re-resolution. setsid() detaches the child
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
        let path = login_shell_path(shell.to_str().unwrap(), None).await;
        assert_eq!(path.as_deref(), Some("/opt/secrets/bin:/usr/bin"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_path_line_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let shell = stub_shell(dir.path(), "HOME=/Users/dev");
        assert_eq!(login_shell_path(shell.to_str().unwrap(), None).await, None);
    }

    // Whatever the environment (CI without a login shell, unset SHELL, a shell
    // that fails to start), the public helper must produce a non-empty PATH so
    // callers can unconditionally `.env("PATH", …)` with the result.
    //
    // No longer sensitive to a sibling's stub shell — nothing in this module
    // publishes one any more (issue #310) — but still serialised, because the
    // real login shell it spawns is the expensive resolution this crate's tests
    // otherwise avoid piling up concurrently.
    #[tokio::test]
    async fn resolves_to_a_non_empty_path() {
        let _serialised = cache_test_lock().lock().await;
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

    // The bug this module exists to fix: a directory-based version-manager
    // hook must see the PROJECT's directory, not wherever the daemon process
    // happens to be — and two projects must not share one answer.
    #[cfg(unix)]
    #[tokio::test]
    async fn cached_user_path_for_is_scoped_per_directory() {
        let _serialised = cache_test_lock().lock().await;
        let root = tempfile::tempdir().unwrap();
        // Canonicalized: on macOS `$TMPDIR` is a symlink into `/private`, and
        // plain `pwd` in the stub shell below prints the resolved physical
        // path — comparing against the pre-canonicalization path would fail
        // on an irrelevant symlink mismatch, not the thing under test.
        let root_path = root.path().canonicalize().unwrap();
        let project_a = root_path.join("project-a");
        let project_b = root_path.join("project-b");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();
        // Emits a PATH derived from the shell's actual working directory, the
        // way an `.nvmrc`-driven rc file would emit a different Node bin dir
        // per project.
        use std::os::unix::fs::PermissionsExt;
        let shell = root_path.join("stub-shell");
        std::fs::write(&shell, "#!/bin/sh\necho \"PATH=$(pwd)/bin:/usr/bin\"\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let stub = shell.to_str().unwrap();

        let path_a = cached_user_path_for_with_shell(stub, &project_a).await;
        let path_b = cached_user_path_for_with_shell(stub, &project_b).await;

        // The stub reached resolution as an argument, never as published
        // process-wide state — see `resolve_with_fallback` and issue #310.
        assert_eq!(resolution_shell(), crate::shell::auto_shell());

        assert_eq!(path_a, format!("{}/bin:/usr/bin", project_a.display()));
        assert_eq!(path_b, format!("{}/bin:/usr/bin", project_b.display()));
        assert_ne!(path_a, path_b);
    }

    // The bug a review round caught in the first version of this function: a
    // stale directory entry must be served immediately (never blocking the
    // caller on a fresh login shell — that reintroduces the click-hang
    // `cached_user_path`'s warm task exists to prevent), and a resolution
    // that stalls or learns nothing must never downgrade the cache to the
    // bare fallback — it must keep serving the last real answer.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_directory_entry_is_served_immediately_and_refreshed_in_background() {
        use std::os::unix::fs::PermissionsExt;

        let _serialised = cache_test_lock().lock().await;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().canonicalize().unwrap();
        let shell = dir.join("stub-shell");
        let write_shell = |stdout: &str| {
            std::fs::write(&shell, format!("#!/bin/sh\necho 'PATH={stdout}'\n")).unwrap();
            std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        };

        write_shell("/old/bin");
        let stub = shell.to_str().unwrap().to_owned();

        // A zero TTL makes every entry stale on its next hit — the policy is
        // injected, so nothing here subtracts from `Instant::now()` (which
        // panics on a host booted less than that long ago).
        let policy = Policy {
            max_age: Duration::ZERO,
            dir_ttl: Duration::ZERO,
            startup_files: Vec::new(),
        };

        let first = cached_user_path_for_with(&stub, &dir, &policy).await;
        assert_eq!(first, "/old/bin");

        // Change what the shell would now answer.
        write_shell("/new/bin");

        // A stale hit returns the OLD value with no wait — the whole point.
        let second = cached_user_path_for_with(&stub, &dir, &policy).await;
        assert_eq!(second, "/old/bin");

        // The background refresh it triggered eventually publishes the new
        // answer, without ever having served the bare fallback in between.
        let mut refreshed = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let path = published_dir_path(&dir);
            assert_ne!(
                path.as_deref(),
                Some(process_path_fallback().as_str()),
                "must never downgrade to the fallback while a real answer is known"
            );
            if path.as_deref() == Some("/new/bin") {
                refreshed = Some(path);
                break;
            }
        }
        // The background refresh carried the stub as an argument too — nothing
        // about it leaked into the process-global shell (issue #310).
        assert_eq!(resolution_shell(), crate::shell::auto_shell());
        assert_eq!(refreshed.flatten(), Some("/new/bin".to_owned()));
    }

    #[test]
    fn startup_files_cover_zsh_bash_fish_and_system_files() {
        let home = std::path::Path::new("/h");
        let files = startup_files(Some(home), None);
        for want in [
            "/h/.zshenv",
            "/h/.zprofile",
            "/h/.zshrc",
            "/h/.zlogin",
            "/h/.bashrc",
            "/h/.bash_profile",
            "/h/.profile",
            "/h/.bash_login",
            "/h/.config/fish/config.fish",
            "/h/.config/fish/conf.d",
            "/etc/zshenv",
            "/etc/zprofile",
            "/etc/zshrc",
            "/etc/profile",
            "/etc/bashrc",
            "/etc/paths",
            "/etc/paths.d",
        ] {
            assert!(
                files.contains(&want.into()),
                "{want} missing from {files:?}"
            );
        }
        // ZDOTDIR replaces $HOME for zsh's files only.
        let files = startup_files(Some(home), Some(std::path::Path::new("/z")));
        assert!(files.contains(&"/z/.zshrc".into()));
        assert!(!files.contains(&"/h/.zshrc".into()));
        assert!(files.contains(&"/h/.bashrc".into()));
    }

    #[test]
    fn fingerprint_moves_with_the_files_and_the_shell_only() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        let policy = Policy {
            max_age: PATH_MAX_AGE,
            dir_ttl: DIR_PATH_TTL,
            startup_files: vec![rc.clone()],
        };
        let absent = policy.fingerprint("zsh");
        assert_eq!(
            absent,
            policy.fingerprint("zsh"),
            "stable when nothing moved"
        );

        std::fs::write(&rc, "export A=1\n").unwrap();
        let created = policy.fingerprint("zsh");
        assert_ne!(absent, created, "creating a startup file is a change");
        assert_eq!(created, policy.fingerprint("zsh"));

        // A different length changes the fingerprint even on a filesystem whose
        // mtime would not have moved.
        std::fs::write(&rc, "export A=11\n").unwrap();
        assert_ne!(created, policy.fingerprint("zsh"));

        assert_ne!(
            policy.fingerprint("zsh"),
            policy.fingerprint("bash"),
            "a different shell answers a different question"
        );
    }

    #[test]
    fn the_warm_loop_spawns_on_first_change_or_age_only() {
        let policy = Policy {
            max_age: Duration::from_secs(300),
            dir_ttl: DIR_PATH_TTL,
            startup_files: Vec::new(),
        };
        let fp = policy.fingerprint("zsh");
        let other = policy.fingerprint("bash");
        let t0 = std::time::Instant::now();
        let later = |secs| t0 + Duration::from_secs(secs);

        // Nothing helpful resolved yet — including after a failed attempt.
        assert!(resolve_due(None, t0, &fp, policy.max_age));
        // Same inputs, inside max_age: the 20 s tick must not spawn.
        assert!(!resolve_due(
            Some((t0, &fp)),
            later(20),
            &fp,
            policy.max_age
        ));
        assert!(!resolve_due(
            Some((t0, &fp)),
            later(299),
            &fp,
            policy.max_age
        ));
        // Max age reached.
        assert!(resolve_due(
            Some((t0, &fp)),
            later(300),
            &fp,
            policy.max_age
        ));
        // Inputs changed, however recent.
        assert!(resolve_due(
            Some((t0, &fp)),
            later(1),
            &other,
            policy.max_age
        ));
    }

    // A fresh directory entry is served without a refresh no matter how many
    // times it is asked for — the saving this reshape exists for — until a
    // startup file changes, which makes it stale at once (served old, refreshed
    // in the background) even though its TTL has hours left.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_directory_entry_is_fresh_until_a_startup_file_changes() {
        use std::os::unix::fs::PermissionsExt;

        let _serialised = cache_test_lock().lock().await;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().canonicalize().unwrap();
        let rc = dir.join(".zshrc");
        std::fs::write(&rc, "# v1\n").unwrap();
        let shell = dir.join("stub-shell");
        let write_shell = |stdout: &str| {
            std::fs::write(&shell, format!("#!/bin/sh\necho 'PATH={stdout}'\n")).unwrap();
            std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write_shell("/old/bin");
        let stub = shell.to_str().unwrap().to_owned();
        let policy = Policy {
            max_age: Duration::from_secs(3600),
            dir_ttl: Duration::from_secs(3600),
            startup_files: vec![rc.clone()],
        };

        assert_eq!(
            cached_user_path_for_with(&stub, &dir, &policy).await,
            "/old/bin"
        );

        // The shell would now answer differently, but nothing it reads moved:
        // no refresh may start, so the old answer must keep coming back.
        write_shell("/new/bin");
        for _ in 0..3 {
            assert_eq!(
                cached_user_path_for_with(&stub, &dir, &policy).await,
                "/old/bin"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert_eq!(published_dir_path(&dir).as_deref(), Some("/old/bin"));

        // Now a startup file changes: this hit still returns instantly with the
        // old value, and kicks a refresh that publishes the new one.
        std::fs::write(&rc, "# v2 — longer\n").unwrap();
        assert_eq!(
            cached_user_path_for_with(&stub, &dir, &policy).await,
            "/old/bin"
        );
        let mut seen = None;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            seen = published_dir_path(&dir);
            if seen.as_deref() == Some("/new/bin") {
                break;
            }
        }
        assert_eq!(seen.as_deref(), Some("/new/bin"));
    }

    fn published_dir_path(dir: &std::path::Path) -> Option<String> {
        dir_cell()
            .lock()
            .ok()
            .and_then(|g| g.get(dir).map(|e| e.value.clone()))
    }
}
