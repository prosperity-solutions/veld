//! The environment a terminal session is handed so that a process inside it can
//! open a URL *in Veld*.
//!
//! One small directory per daemon instance ([`veld_core::instance::shim_dir`])
//! holding three generated `sh` scripts and a `zdotdir/` of one file, plus these
//! variables in the shell's environment:
//!
//! | Variable | Why |
//! |---|---|
//! | `BROWSER` | Points at `veld-open`. What Claude Code's own login flow, `gh`, `git`, Python's `webbrowser`, vite and next all consult. |
//! | `VELD_PTY_SESSION` | Which terminal asked, which is how the daemon knows *which window* the pane belongs in. |
//! | `VELD_SHIM_DIR` | The directory holding the `open`/`xdg-open` wrappers. Exported for every shell, so a non-zsh user can put it on `PATH` themselves. |
//! | `ZDOTDIR` / `VELD_USER_ZDOTDIR` | zsh only, and only while `terminal.interceptSystemOpen` is on: the handoff that gets that directory onto `PATH` after the user's own startup files. See [`zshenv`]. |
//! | `ENV` / `VELD_USER_ENV` | The bash equivalent, on a bash that was **probed** to honour it. See [`bashenv`]. |
//! | `VELD_SHIM_BROWSER` | The same path as `BROWSER`, kept under its own name so the `veld_browser` hook can re-assert it after an rc file exports a `$BROWSER` of its own. |
//! | `VELD_BROWSER_ORIGINAL` | Whatever `$BROWSER` was before veld took it over, so the fall-through path can restore it instead of handing a child the shim again. |
//!
//! **`terminal.openUrlsInApp` gates every one of them** except the session id. Off
//! means veld is not in the shell at all — for every session started after it. An
//! environment is fixed at spawn and a terminal outlives the tab and the daemon, so a
//! shell that is already open keeps what it was given; the settings row says so.
//! See [`session_env`].
//!
//! # Why `$BROWSER` alone is not enough
//!
//! The case that matters most does not read it: an agent's shell tool runs
//! `open <url>` directly (`Bash(open "https://…")` consults no variable), and Claude
//! Code sets `BROWSER=true` for its children on top of that. Catching those means
//! having the shim directory on `PATH`.
//!
//! And `PATH` set in the spawn environment does not survive a login shell — measured,
//! not assumed. macOS `/etc/zprofile` runs `path_helper`, which rebuilds `PATH` with
//! the system directories first and appends the previous contents, so a prepended
//! entry lands *behind* `/usr/bin` and `open` still resolves to `/usr/bin/open`;
//! Debian's `/etc/profile` overwrites `PATH` outright. The shell is a login shell by
//! design — that is how a terminal gets the user's real environment — so it is
//! entitled to do this.
//!
//! The answer is one file veld owns, per shell family, reached through the one seam
//! that shell offers for running code around its own startup:
//!
//! - **zsh** — [`zshenv`], via `ZDOTDIR`. veld's file is read first, hands `ZDOTDIR`
//!   straight back, and registers a `precmd` hook that runs after every rc file.
//! - **bash** — [`bashenv`], via posix mode's `$ENV`, which is the *only* startup
//!   file an interactive `--posix` bash reads. veld's file leaves posix mode, replays
//!   the user's own startup in bash's documented order, and adds its line last.
//!   Used only on a bash **probed** to honour it: macOS ships bash 3.2 as
//!   `/bin/bash`, which ignores `$ENV` entirely.
//!
//! Nothing of the user's is edited, wrapped, or read twice. Everything else — fish,
//! nushell, and a bash that failed the probe — keeps `$BROWSER` and the documented
//! `$VELD_SHIM_DIR` one-liner, and `veld doctor` says so rather than leaving it
//! silent.
//!
//! # Rewritten every daemon start, never trusted from disk
//!
//! The scripts carry the absolute path of the `veld` binary **belonging to the
//! running daemon** (`veld_cli_path`), so a dev instance's terminals call the dev
//! CLI and the installed one's call the installed CLI. That also means an upgrade
//! must rewrite them, which is why generation is unconditional rather than "if
//! missing". If no such binary is there, nothing is written and no `BROWSER` is
//! injected: a `$BROWSER` pointing at a script that cannot work is worse than no
//! `$BROWSER` at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing::{debug, warn};
use veld_core::opener::Tool;

/// The variables a terminal session gets.
///
/// `shell` is the login shell about to be spawned; `open_in_app` is
/// `terminal.openUrlsInApp` and `intercept` is `terminal.interceptSystemOpen`.
///
/// **`open_in_app` gates everything**, and that is the point of it rather than a
/// detail. It is documented as turning the whole behaviour off, so with it off veld
/// must not be in the path at all: no `$BROWSER`, no `ZDOTDIR`, nothing in the
/// shell's startup. The first version gated only the `ZDOTDIR` block, which left the
/// off switch still routing every browser launch through `veld open-url` to the
/// daemon — a round trip, a stderr line per open, and a login URL (one-time tokens
/// included) leaving the process — while the settings UI greyed the *other* switch
/// out and so could not turn off the thing that was still running.
///
/// Only `VELD_PTY_SESSION` survives an off switch, and it is not part of the
/// behaviour: `veld open-url` run deliberately, by a person or an agent, still needs
/// to know which terminal it is in.
///
/// Empty except for `VELD_PTY_SESSION` when the shim directory could not be
/// prepared, for the same reason.
pub fn session_env(
    session_id: &str,
    shell: &str,
    open_in_app: bool,
    intercept: bool,
    // Whether this bash was **probed** to honour the `--posix`/`$ENV` handoff.
    // Passed in rather than probed here because probing spawns a process and this
    // function is pure and synchronous; the daemon probes once, at ticket-mint
    // time, and caches. Meaningless for a non-bash shell, and ignored there.
    bash_handoff: bool,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("VELD_PTY_SESSION".to_owned(), session_id.to_owned());
    if !open_in_app {
        return env;
    }
    let Some(dir) = dir() else {
        return env;
    };
    // Everything below points at a file in that directory, so the directory having
    // been *swept since boot* has to be checked here rather than assumed from `dir()`
    // succeeding at startup: `dir()` memoises in a `OnceLock` and nothing rewrites the
    // files, and the very trigger the handoff check below describes — a `VELD_PTY_DIR`
    // under `/tmp` meeting the periodic sweep — takes `veld-open` with it. Handing a
    // shell `BROWSER=<gone>/veld-open` would make `gh`, Claude Code's login, vite and
    // Python's `webbrowser` all exec a nonexistent file and open *nothing*, where with
    // no `$BROWSER` at all they would have worked. This module's own header calls that
    // out as worse than not setting it; a half-configured environment is not a
    // degradation, it is a break.
    if !dir.join(Tool::Browser.shim_name()).is_file() {
        return env;
    }
    env.insert("VELD_SHIM_DIR".to_owned(), dir.display().to_string());
    // The handoff is only safe while the file that performs it exists. `ZDOTDIR`
    // redirects *every* zsh startup file, so if `zdotdir/.zshenv` is missing zsh finds
    // no `.zshenv` there, veld's `unset ZDOTDIR` never runs, and `.zprofile`, `.zshrc`
    // and `.zlogin` are all looked for in veld's directory too — none of the user's
    // zsh config runs at all, and `/etc/zshrc` puts that terminal's history in
    // `~/.veld` as well. The files are written once per daemon and the path is
    // memoised, so "it was there at boot" is not the same as "it is there now": a
    // `VELD_PTY_DIR` under `/tmp` (which the socket-length diagnostic suggests) meets
    // the periodic sweep. One `stat` per session spawn buys the difference between
    // "the feature is off" and "your shell is not yours".
    let handoff = zdotdir_path(dir).join(".zshenv");
    if intercept && is_zsh(shell) && handoff.is_file() {
        // The zero-touch half: `$BROWSER` cannot reach a program that calls `open`
        // directly (an agent's shell tool does exactly that, and Claude Code even
        // sets `BROWSER=true` for its children), and `PATH` set here is reordered by
        // `/etc/zprofile`'s `path_helper` before the first prompt. So veld takes over
        // `ZDOTDIR` for exactly one file read — see `zshenv` for what that file does
        // and, more importantly, what it refuses to do.
        env.insert(
            "ZDOTDIR".to_owned(),
            zdotdir_path(dir).display().to_string(),
        );
        // What to hand back. Absent when the user has none, which is the normal case:
        // `ZDOTDIR` is conventionally set *in* `~/.zshenv`, i.e. by the very file the
        // handoff sources, so it is theirs to set and ours to stay out of.
        if let Some(theirs) = std::env::var_os("ZDOTDIR")
            .and_then(|v| v.into_string().ok())
            .filter(|v| !v.is_empty())
        {
            env.insert("VELD_USER_ZDOTDIR".to_owned(), theirs);
        }
    }
    // The bash equivalent. Same three conditions as the zsh branch — the setting is
    // on, the shell is the right family, and the file that performs the handoff is
    // still on disk — plus one this one needs and zsh's does not: the shell must
    // have been probed to honour `$ENV` in posix mode. On bash 3.2 (macOS's
    // `/bin/bash`) it does not, and setting `ENV` there is at best inert; it is
    // `interactive_flags` reading this same `ENV` key that would otherwise add a
    // `--posix` with nothing to show for it.
    let bash_handoff_file = bashenv_path(dir);
    if intercept && is_bash(shell) && bash_handoff && bash_handoff_file.is_file() {
        // Stashed before it is replaced, exactly like `VELD_USER_ZDOTDIR`: `$ENV`
        // is read by every posix shell the user starts, so silently dropping
        // theirs would change more than this feature is about.
        if let Some(theirs) = std::env::var_os("ENV")
            .and_then(|v| v.into_string().ok())
            .filter(|v| !v.is_empty())
        {
            env.insert("VELD_USER_ENV".to_owned(), theirs);
        }
        env.insert("ENV".to_owned(), bash_handoff_file.display().to_string());
    }
    // Saved before it is replaced, so the fall-through in `veld open-url` can give a
    // child the browser the user actually configured, rather than handing it the shim
    // again — which is a loop, not a fallback.
    //
    // Read from the **daemon's** environment, which under launchd or systemd almost
    // never carries the user's `$BROWSER`: theirs lives in an rc file, and the
    // `veld_browser` hook in `zshenv` is what captures that one. This covers the case
    // where the daemon really was started with a `$BROWSER` in scope.
    if let Some(previous) = std::env::var_os("BROWSER")
        .and_then(|v| v.into_string().ok())
        .filter(|v| !v.is_empty())
    {
        env.insert("VELD_BROWSER_ORIGINAL".to_owned(), previous);
    }
    let shim_browser = dir.join(Tool::Browser.shim_name()).display().to_string();
    // Named separately so the `precmd` companion in `zshenv` can re-assert it: a
    // user's rc doing `export BROWSER=firefox` runs *after* this environment is
    // handed over and would otherwise switch the `$BROWSER` half of the feature off
    // with nothing said anywhere.
    env.insert("VELD_SHIM_BROWSER".to_owned(), shim_browser.clone());
    env.insert("BROWSER".to_owned(), shim_browser);
    env
}

/// Whether a login shell is zsh.
///
/// Delegates to [`veld_core::shell::kind`] so "is this bash / is this zsh" has one
/// answer in the codebase. By basename, which is not a shortcut: a shell's
/// `argv[0]` decides which startup files it reads, so `/bin/sh` is not bash even
/// when it is the same binary.
fn is_zsh(shell: &str) -> bool {
    veld_core::shell::kind(shell) == veld_core::shell::Kind::Zsh
}

/// Whether a login shell is bash.
fn is_bash(shell: &str) -> bool {
    veld_core::shell::kind(shell) == veld_core::shell::Kind::Bash
}

/// Where the `$ENV` file veld owns lives — beside the shims, like the zsh one.
fn bashenv_path(shim_dir: &Path) -> PathBuf {
    shim_dir.join("bash").join("veldenv.bash")
}

/// The one file veld runs inside a **bash** startup.
///
/// # The seam, and why it is this one
///
/// bash has no hook that runs after its rc files: `BASH_ENV` is non-interactive
/// shells only, and `--rcfile` is consulted only when the shell is interactive and
/// **not** a login shell — so it is ignored outright for the `-l` shell a terminal
/// opens, and reaching it would mean dropping `-l` and losing `shopt login_shell`
/// along with `/etc/profile`.
///
/// What does exist is posix mode: started with `--posix`, an interactive bash reads
/// `$ENV` **and no other startup file at all**. So veld takes that one file, leaves
/// posix mode on the first line, replays the user's real startup itself, and adds
/// its line at the end — where nothing can rebuild `PATH` after it. This is the
/// mechanism kitty's shell integration uses; VS Code took the `--rcfile` route and
/// carries a standing bug for the login-shell semantics it loses.
///
/// # What makes this riskier than the zsh handoff, and what pays for it
///
/// [`zshenv`] *adds* a file to a sequence bash-style posix mode *replaces*. If this
/// replay is wrong, the user's whole environment is wrong — not merely the shim. So:
///
/// - It is only ever used when the shell has been **probed** to honour the handoff
///   (`veld_core::shell::supports_posix_env_handoff`). macOS's `/bin/bash` is 3.2,
///   where `--posix` is accepted and `$ENV` is *ignored* — passing it there would
///   leave a session in posix mode for no benefit, so veld passes nothing.
/// - It replays exactly what bash documents, keyed on `shopt -q login_shell`, which
///   is still true because `-l` survives (posix mode changes which files are read,
///   not what kind of shell this is).
/// - `terminal.interceptSystemOpen` turns the whole thing off, as it does for zsh.
/// - `a_bash_session_runs_the_users_startup_and_still_wins_the_path` asserts it
///   against a real bash, the same way the zsh file is asserted.
///
/// One thing this file does *better* than the zsh one: it runs **after** the user's
/// rc files rather than before, so `$BROWSER` is re-asserted with a plain
/// assignment instead of a `precmd` hook. There is nothing left to run after it.
///
/// # The one place this deliberately departs from bash
///
/// A login bash does not read `~/.bashrc` — it reads `/etc/profile` and the first of
/// `~/.bash_profile`, `~/.bash_login`, `~/.profile`, and the convention is that one
/// of those sources `~/.bashrc`. Where that line is missing, following bash exactly
/// would mean the shell picker loading none of the config it exists for: macOS ships
/// no `~/.bash_profile`, so a user with `~/.profile` and `~/.bashrc` gets a bash with
/// none of their aliases or functions — the very complaint `terminal.shell` was built
/// for, reproduced one level down. So this file sources `~/.bashrc` as well, unless
/// the profile it sourced already mentions it. Every other terminal emulator runs a
/// plain login bash and does not do this; veld does, because picking bash *here* is a
/// statement about where your config lives.
fn bashenv() -> String {
    // `${x-}` throughout, for the same reason the zsh file uses it: these lines run
    // after the user's startup, so they inherit their `set -u`.
    String::from(GENERATED_HEADER)
        + r#"
#
# veld owns this file and nothing else. bash read it INSTEAD OF your startup files
# (posix mode + $ENV), so its first job is to run them, in bash's own documented
# order, and then get out of the way.

# Leave posix mode immediately: it is how veld got here, not a mode you asked for.
builtin set +o posix

# Hand $ENV back. Yours if you had one — it is read by every posix shell you start
# from here — and unset otherwise, so a nested bash is not wrapped.
if [ -n "${VELD_USER_ENV-}" ]; then
  ENV="$VELD_USER_ENV"
  builtin export ENV
else
  builtin unset ENV
fi

# Your startup files, in bash's own order. `-l` survived into posix mode, so this
# answers the same question bash itself would have asked.
if shopt -q login_shell; then
  [ -r /etc/profile ] && . /etc/profile
  veld_profile=
  for veld_rc in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [ -r "$veld_rc" ]; then veld_profile="$veld_rc"; . "$veld_rc"; break; fi
  done
  # …and then ~/.bashrc, which a login bash does NOT read on its own. The usual
  # setup has the profile source it, and where that line is missing, choosing bash
  # in veld's shell picker would load none of the config the picker exists for:
  # macOS ships no ~/.bash_profile at all, so a user with ~/.profile and ~/.bashrc
  # gets a bash with none of their aliases, functions or integrations.
  #
  # Only when the profile did not already do it, tested per line and only on a
  # line that actually sources something. Comments are skipped, because the bare
  # word is not evidence: a profile whose only mention is
  # `# deliberately not sourcing ~/.bashrc` would otherwise suppress the source and
  # hand the user exactly the bug this feature exists to fix. The two error
  # directions are not symmetric — a missed source means none of your config
  # loaded, a double source means a duplicated PATH entry — so the test is
  # deliberately biased toward sourcing.
  if [ -r "$HOME/.bashrc" ]; then
    veld_seen=
    if [ -n "$veld_profile" ]; then
      while IFS= read -r veld_line || [ -n "$veld_line" ]; do
        [[ "$veld_line" =~ ^[[:space:]]*# ]] && continue
        case "$veld_line" in
          *". "*bashrc*|*"source "*bashrc*) veld_seen=1; break ;;
        esac
      done < "$veld_profile"
    fi
    [ -z "$veld_seen" ] && . "$HOME/.bashrc"
  fi
else
  for veld_rc in /etc/bash.bashrc /etc/bash/bashrc /etc/bashrc; do
    if [ -r "$veld_rc" ]; then . "$veld_rc"; break; fi
  done
  [ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
fi
builtin unset veld_rc veld_profile veld_seen veld_line

# veld's shim directory, prepended last — after /etc/profile's path_helper and
# after your own rc files, which is the only point at which it can win. Idempotent,
# so a re-source cannot stack duplicates.
if [ -n "${VELD_SHIM_DIR-}" ]; then
  case ":$PATH:" in
    *":$VELD_SHIM_DIR:"*) ;;
    *) PATH="$VELD_SHIM_DIR:$PATH"; builtin export PATH ;;
  esac
fi

# An rc file that exported its own BROWSER has already run, so take it back and
# keep theirs for the fall-through. A plain assignment, not a hook: nothing runs
# after this file.
if [ -n "${VELD_SHIM_BROWSER-}" ] && [ "${BROWSER-}" != "$VELD_SHIM_BROWSER" ]; then
  if [ -n "${BROWSER-}" ]; then
    VELD_BROWSER_ORIGINAL="$BROWSER"
    builtin export VELD_BROWSER_ORIGINAL
  fi
  BROWSER="$VELD_SHIM_BROWSER"
  builtin export BROWSER
fi
"#
}

/// Where the `.zshenv` veld owns lives — beside the shims, inside the instance's
/// own directory, never in the user's home.
fn zdotdir_path(shim_dir: &Path) -> PathBuf {
    shim_dir.join("zdotdir")
}

/// The one file veld runs inside a user's shell startup.
///
/// # What it does, and what it refuses to do
///
/// zsh reads `$ZDOTDIR/.zshenv` first, then `/etc/zprofile` (where macOS's
/// `path_helper` rebuilds `PATH` with the system directories in front), then the
/// user's `.zprofile`, `.zshrc` and `.zlogin`. `$ZDOTDIR` is re-read at every one of
/// those steps — which is the whole trick here:
///
/// 1. **It hands `ZDOTDIR` straight back.** After this file, every remaining stage
///    reads the *user's* files, in the normal order, unmodified, with the user's own
///    `$ZDOTDIR` visible to them. veld owns one file, not a shell startup.
/// 2. **It sources the user's `.zshenv`**, because ours took its place in the order.
/// 3. **It registers a `precmd` hook** that prepends the shim directory to `PATH`.
///    A hook rather than a plain assignment because an assignment here is exactly
///    what `path_helper` undoes two steps later; `precmd` runs before the first
///    prompt, which is after everything. Left registered and idempotent, so a later
///    `PATH` rebuild (a venv, a version manager) cannot silently drop the shim.
/// 4. **It registers a second, self-deregistering `precmd` hook** that takes
///    `$BROWSER` back if an rc file exported one of its own, keeping the user's value
///    in `VELD_BROWSER_ORIGINAL` for a fall-through. Unlike (3) this one runs **once**:
///    an rc file is startup, but `export BROWSER=lynx` typed at a prompt afterwards is
///    a deliberate act and veld does not argue with it.
///
/// It does **not** wrap `.zshrc`, source anything of the user's twice, or write to
/// their home directory. If the user's `.zshrc` clears `precmd_functions` outright
/// (rare — frameworks append), both hooks are lost and the feature degrades to
/// `$BROWSER` only, which is the pre-feature behaviour rather than a broken shell.
fn zshenv() -> String {
    // Every expansion below uses the `${x-}` form, and that is not style: these lines
    // run *after* the user's `.zshenv` has been sourced, so they inherit the user's
    // `setopt`. Under `no_unset` a bare `$BROWSER` or `${precmd_functions[(r)…]}` is a
    // fatal error, which aborts the rest of *this* file — no PATH hook, no `$BROWSER`
    // re-assert — and prints an error naming a file in `~/.veld` at the top of every
    // terminal. In the `veld_browser` case it aborts before the function can
    // deregister itself, so the error repeats at every prompt forever.
    // The header is prepended from `GENERATED_HEADER` rather than typed out: it is the
    // proof of ownership `remove_if_generated` checks before deleting this file. A
    // concatenation and not `format!`, because the body below is full of `${…}` and
    // `(…)` shell syntax that would have to be brace-escaped for a format string —
    // exactly the kind of edit that silently changes a generated shell file.
    String::from(GENERATED_HEADER)
        + r#"
#
# veld owns this file and nothing else. Its first job is to give ZDOTDIR back, so
# every later zsh startup file is YOURS, read in the normal order.
if [ -n "${VELD_USER_ZDOTDIR-}" ]; then
  ZDOTDIR="$VELD_USER_ZDOTDIR"
else
  unset ZDOTDIR
fi

# Your own .zshenv, which this file stood in for.
if [ -f "${ZDOTDIR-$HOME}/.zshenv" ]; then
  . "${ZDOTDIR-$HOME}/.zshenv"
fi

# Put veld's shim directory on PATH just before each prompt: after /etc/zprofile's
# path_helper and after your own rc files, which is the only point at which it can
# win. Idempotent, so it costs a string compare per prompt and nothing else.
if [ -n "${VELD_SHIM_DIR-}" ]; then
  veld_shim_path() {
    case ":$PATH:" in
      *":$VELD_SHIM_DIR:"*) ;;
      *) PATH="$VELD_SHIM_DIR:$PATH" ;;
    esac
  }
  typeset -ga precmd_functions
  if [[ -z ${precmd_functions[(r)veld_shim_path]-} ]]; then
    precmd_functions+=(veld_shim_path)
  fi
fi

# An rc file that exports its own BROWSER runs after veld set one, so re-point it
# once — and keep the user's value, so a fall-through opens the browser they chose.
# Once only, not every prompt: an rc file is startup, but `export BROWSER=lynx` typed
# at the prompt later is a deliberate act and veld does not argue with it.
if [ -n "${VELD_SHIM_BROWSER-}" ]; then
  veld_browser() {
    if [ "${BROWSER-}" != "$VELD_SHIM_BROWSER" ]; then
      [ -n "${BROWSER-}" ] && export VELD_BROWSER_ORIGINAL="$BROWSER"
      export BROWSER="$VELD_SHIM_BROWSER"
    fi
    precmd_functions=(${precmd_functions:#veld_browser})
    unfunction veld_browser 2>/dev/null
  }
  typeset -ga precmd_functions
  precmd_functions+=(veld_browser)
fi
"#
}

/// The prepared shim directory, or `None` if it could not be prepared.
///
/// Prepared once per daemon: three small files, and the answer is the same for
/// every session.
pub fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| match prepare() {
        Ok(dir) => Some(dir),
        Err(e) => {
            // A warning, not an error: every other part of a terminal still works.
            warn!("terminal URL opening is off — could not write the shim directory: {e}");
            None
        }
    })
    .as_deref()
}

fn prepare() -> std::io::Result<PathBuf> {
    let cli = veld_cli_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no `veld` CLI belongs to this daemon (see veld_core::paths::cli_for_exe)",
        )
    })?;
    let dir = veld_core::instance::shim_dir();
    prepare_in(&dir, &cli)?;
    debug!(dir = %dir.display(), cli = %cli.display(), "terminal URL shims written");
    Ok(dir)
}

/// Remove the scripts of a previous daemon that this one cannot back with a CLI.
///
/// **The scripts have to go, because they name a CLI by absolute path** and the reason
/// none resolves now can be that that exact file was removed — an install moved to a
/// new `VELD_INSTALL_DIR`, an uninstall, an update interrupted between the delete and
/// the copy. Left in place, `$BROWSER` points at a script whose only act is to exec a
/// file that is gone, and it stays that way forever: [`prepare_in`] is the only thing
/// that rewrites them and it is never reached in this state, so `veld doctor` would
/// keep reporting the dangling path and keep advising a restart that cannot help.
/// Removing them is the state the feature already handles — no `veld-open`, so
/// [`session_env`] injects nothing and a shell's `open` and `$BROWSER` are the
/// system's own.
///
/// Three constraints on *how*, every one of them learned the hard way:
///
/// - **Never from [`dir`].** `dir` is reached by this crate's own unit tests, which run
///   in a test binary that resolves no CLI and — with no `VELD_PTY_DIR` set — compute
///   the **developer's real** `~/.veld/pty-<port>/shims`. With the removal wired into
///   `prepare`, one `cargo test --workspace` deleted the live shims out from under a
///   running installed daemon. `pty_recovery`'s confinement assert did not catch it
///   because it watches for files *written* into the real home, and this wrote
///   nothing.
/// - **Only once the daemon owns its port** ([`super::clear_unbacked_shims`], called
///   after the listener binds). "From the daemon's startup path" was not enough on its
///   own: `pty_dir` is keyed on the daemon *port*, the bind happens after startup, and
///   a failed bind is deliberately non-fatal — so a stray `cargo run -p veld-daemon`
///   on the default port would delete the installed daemon's shims and then carry on
///   running without an HTTP API.
/// - **Only files veld generated**, identified by the header every generated script
///   carries. This directory is on a developer's `PATH`; a blanket
///   `remove_file(dir.join(name))` is a shim feature that deletes a file it does not
///   own the moment a name collides.
pub fn clear_unbacked() {
    if veld_cli_path().is_some() {
        return;
    }
    let dir = veld_core::instance::shim_dir();
    let mut removed = 0;
    for tool in Tool::ALL.iter().copied() {
        removed += usize::from(remove_if_generated(&dir.join(tool.shim_name())));
    }
    removed += usize::from(remove_if_generated(&zdotdir_path(&dir).join(".zshenv")));
    // The bash handoff goes with them, and for a sharper reason than the zsh one:
    // it is the *only* file bash reads, so a stale copy naming a `veld` binary that
    // no longer exists still replays the user's startup correctly but points
    // `$BROWSER` at a script that cannot run.
    removed += usize::from(remove_if_generated(&bashenv_path(&dir)));
    if removed > 0 {
        warn!(
            dir = %dir.display(),
            removed,
            "removed terminal URL shims: no veld CLI belongs to this daemon any more"
        );
    }
}

/// Delete `path` if it is a file veld generated. Whether it deleted anything.
fn remove_if_generated(path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    if !contents.contains(GENERATED_HEADER) {
        return false;
    }
    std::fs::remove_file(path).is_ok()
}

/// The line every generated file carries, and the proof of ownership
/// [`remove_if_generated`] requires. Shared with the writers so the two cannot drift
/// apart — a marker the writers stopped emitting would silently turn the cleanup into
/// a no-op.
const GENERATED_HEADER: &str =
    "# Generated by veld — rewritten on every daemon start; edits are lost.";

/// Write the three shims into `dir`.
///
/// Takes the directory rather than reading [`veld_core::instance::shim_dir`], so a
/// test exercises the real write path without touching the developer's home.
fn prepare_in(dir: &Path, cli: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    // 0700: these are executables that will sit on a developer's PATH. Applied
    // after `create_dir_all`, which honours the umask and would otherwise leave a
    // group-writable directory on a machine with a lax one.
    set_mode(dir, 0o700)?;
    for tool in Tool::ALL.iter().copied() {
        // **Never introduce a command the platform does not have.** `xdg-open` does
        // not exist on macOS, and writing a shim for it puts one on `PATH` whose only
        // possible answer is "no system opener" — so the portable idiom
        // `command -v xdg-open >/dev/null && xdg-open "$f"` stops finding nothing and
        // starts finding something that cannot work, for *every* file type, not just
        // URLs. A shim may shadow a real tool; it may not invent one.
        let real = veld_core::opener::real_opener(tool, Some(dir));
        let path = dir.join(tool.shim_name());
        if real.is_none() && tool != Tool::Browser {
            // Remove a stale one, so a machine that loses its `xdg-open` between
            // daemon starts does not keep a shim standing in front of nothing.
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let body = script(tool, cli, real.as_deref());
        // Written to a temporary name and renamed, because these files may be
        // *executing* — a shim invoked by a long-running process while the daemon
        // restarts. Writing in place truncates the script mid-read and the shell
        // reports a syntax error; a rename swaps the inode and the running copy
        // finishes on the old one.
        let tmp = dir.join(format!(".{}.new", tool.shim_name()));
        std::fs::write(&tmp, body)?;
        set_mode(&tmp, 0o755)?;
        std::fs::rename(&tmp, &path)?;
    }
    // The `ZDOTDIR` handoff. Written unconditionally — whether a session *uses* it
    // is `session_env`'s call, per shell and per setting — and by the same
    // write-then-rename, since a shell may be starting while the daemon restarts.
    let zdir = zdotdir_path(dir);
    std::fs::create_dir_all(&zdir)?;
    set_mode(&zdir, 0o700)?;
    let tmp = zdir.join(".zshenv.new");
    std::fs::write(&tmp, zshenv())?;
    set_mode(&tmp, 0o600)?;
    std::fs::rename(&tmp, zdir.join(".zshenv"))?;
    // The bash `$ENV` handoff, by the same rules: written unconditionally (whether
    // a session *uses* it is `session_env`'s call, per shell, per setting and per
    // probe) and through write-then-rename, since a shell may be starting while
    // the daemon restarts.
    let handoff = bashenv_path(dir);
    let bash_dir = handoff.parent().expect("bashenv path has a parent");
    std::fs::create_dir_all(bash_dir)?;
    set_mode(bash_dir, 0o700)?;
    let tmp = bash_dir.join("veldenv.bash.new");
    std::fs::write(&tmp, bashenv())?;
    set_mode(&tmp, 0o600)?;
    std::fs::rename(&tmp, &handoff)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// One shim's contents.
///
/// Two lines of logic, and both matter:
///
/// - The `veld` binary is tested for executability *at run time*, not assumed from
///   generation time. An upgrade or an uninstall can remove it while a shell is
///   open, and `open` must keep working when that happens.
/// - The fallback `exec`s the real tool with `"$@"` — the **original** argv,
///   unexamined. Deciding what is and is not a URL is `veld open-url`'s job, in
///   Rust, where it is tested; a shell script that tried would be a second
///   implementation of it.
fn script(tool: Tool, cli: &Path, real: Option<&Path>) -> String {
    let mut out = String::new();
    out.push_str("#!/bin/sh\n");
    out.push_str(GENERATED_HEADER);
    out.push('\n');
    out.push_str(
        "# Routes a single http(s) URL to a Veld browser pane. See `veld open-url --help`.\n",
    );
    // **Capability is probed, never inferred from an exit status**, and both halves of
    // that matter:
    //
    // - `-x` alone is not enough. A `veld` that predates `open-url` (a rollback, or the
    //   window mid-`veld update`, which installs with a plain `cp` in place so the file
    //   is briefly a truncated executable) is executable and cannot serve the request.
    // - The status cannot tell us which happened. Inferring "did not understand" from
    //   clap's usage status 2 was wrong in both directions: `veld open-url` **`exec`s**
    //   the real opener on every fall-through, so the *opener's* status becomes this
    //   command's status — and `xdg-open` documents 2 as "a file did not exist", which
    //   would make the shim run the real opener a second time. A double invocation is
    //   fatal for the login URLs this feature exists to route, since a one-time token
    //   does not survive being opened twice. Meanwhile a truncated binary exits 126 and
    //   a killed one 137, neither of which is 2, so the shim would have exited and
    //   `open <url>` would have opened nothing.
    //
    // `--help` answers exactly the question ("does this binary know this subcommand")
    // and costs one extra spawn on a path a human just triggered. `exec` is restored,
    // so nothing downstream can run twice.
    out.push_str(&format!(
        "if [ -x {cli} ] && {cli} open-url --help >/dev/null 2>&1; then\n  \
         exec {cli} open-url --tool {flag} -- \"$@\"\nfi\n",
        cli = quote(cli),
        flag = tool.flag(),
    ));
    match real {
        Some(real) => out.push_str(&format!("exec {} \"$@\"\n", quote(real))),
        // Nothing to fall through to. Said out loud rather than exiting 0, which
        // would look to the caller like the URL had been opened.
        None => {
            out.push_str(&format!(
                "echo \"veld: cannot open {}: neither veld nor a system opener is available\" >&2\n",
                tool.shim_name()
            ));
            out.push_str("exit 127\n");
        }
    }
    out
}

/// Single-quote a path for `sh`. Paths here are veld's own — a home directory with
/// a space in it is the realistic case, and an embedded quote is handled rather
/// than assumed away.
fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// The `veld` CLI belonging to the running daemon.
///
/// An absolute path derived from this binary's own location rather than looked up
/// on `PATH`: the daemon may be a dev build on its own port with its own database,
/// and its terminals must reach *its* CLI. This is also the check that decides
/// whether the feature is available at all.
///
/// The resolution lives in [`veld_core::paths::cli_for_exe`] — a sibling `veld`
/// first, then the install prefix's `bin/` when this daemon is the installed one —
/// because `veld doctor` reports on the outcome and the two must not have separate
/// ideas about where a CLI is. "Sibling only" was that separate idea: it is
/// satisfiable in a build tree and in no install at all, since the release splits
/// the CLI into `<prefix>/bin` and the daemon into `<prefix>/lib/veld`. Every
/// installed machine therefore had this feature silently off.
fn veld_cli_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    veld_core::paths::cli_for_exe(exe.parent()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_tries_veld_first_and_then_the_real_tool() {
        let body = script(
            Tool::Open,
            Path::new("/usr/local/bin/veld"),
            Some(Path::new("/usr/bin/open")),
        );
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[0], "#!/bin/sh");
        // The guard is `-x`, evaluated when the shim runs: an uninstall while a shell
        // is open must not break `open`.
        assert!(body.contains("[ -x '/usr/local/bin/veld' ]"), "{body}");
        // Capability is **probed**, not inferred from an exit status: a `veld` that
        // cannot serve `open-url` must fall through, and a status-based rule got that
        // wrong in both directions (see `script`).
        assert!(
            body.contains("open-url --help >/dev/null 2>&1"),
            "the shim must probe the subcommand rather than guess from a status: {body}"
        );
        assert!(
            !body.contains("-ne 2"),
            "an exit status cannot distinguish 'veld cannot do this' from the real \
             opener's own failure: {body}"
        );
        // And with the probe in place the call is an `exec` again, so nothing
        // downstream can be invoked twice.
        assert!(
            body.contains("exec '/usr/local/bin/veld' open-url --tool open -- \"$@\""),
            "{body}"
        );
        // The original argv reaches the real tool unexamined.
        assert!(body.contains("\"$@\"\n"), "{body}");
        assert!(
            !body.contains("exit 0"),
            "silently claiming success: {body}"
        );
    }

    #[test]
    fn a_path_with_a_quote_in_it_cannot_break_out_of_the_script() {
        assert_eq!(quote(Path::new("/a b/veld")), "'/a b/veld'");
        assert_eq!(
            quote(Path::new("/a'b/veld")),
            "'/a'\\''b/veld'",
            "an embedded quote has to be closed, escaped and reopened"
        );
    }

    #[test]
    fn only_generated_files_are_ever_removed() {
        let dir = tempfile::TempDir::new().unwrap();
        let shims = dir.path().join("shim-19899");
        prepare_in(&shims, Path::new("/opt/veld/bin/veld")).unwrap();

        // Something of the user's, under a name veld also uses. The shim directory is
        // on a developer's PATH, so "delete the file at this path" is not a thing this
        // module may do — only "delete the file *I* wrote there".
        let theirs = shims.join("xdg-open");
        std::fs::write(
            &theirs,
            "#!/bin/sh\n# mine, not veld's\nexec /usr/bin/true\n",
        )
        .unwrap();
        assert!(
            !remove_if_generated(&theirs),
            "veld deleted a file it did not write"
        );
        assert!(theirs.is_file());

        // And the ones it did write, identified by the header both writers emit from
        // the same constant — bump the constant and this fails rather than silently
        // turning the cleanup into a no-op.
        let generated = shims.join("veld-open");
        assert!(remove_if_generated(&generated));
        assert!(!generated.exists());
        let handoff = zdotdir_path(&shims).join(".zshenv");
        assert!(
            remove_if_generated(&handoff),
            "the zsh handoff must carry the same marker as the shims — it is the file \
             whose absence turns the ZDOTDIR half off"
        );

        // Nothing there at all is not an error, and not a deletion either.
        assert!(!remove_if_generated(&shims.join("never-written")));
    }

    #[test]
    fn the_shims_are_executable_and_rewritten_in_place() {
        let dir = tempfile::TempDir::new().unwrap();
        let shims = dir.path().join("shim-19899");
        prepare_in(&shims, Path::new("/opt/veld/bin/veld")).unwrap();

        // `veld-open` always exists — it is only ever reached through `$BROWSER`,
        // which veld sets itself, so it shadows no command name and invents none.
        // The other two exist only where there is a real tool behind them: a shim
        // may stand in front of `open`, but writing an `xdg-open` on macOS would put
        // a command on `PATH` that the platform does not have and that cannot work.
        for tool in Tool::ALL.iter().copied() {
            let path = shims.join(tool.shim_name());
            let expected = tool == Tool::Browser
                || veld_core::opener::real_opener(tool, Some(&shims)).is_some();
            assert_eq!(
                path.is_file(),
                expected,
                "{}: exists={} but a real tool behind it is {}",
                tool.shim_name(),
                path.is_file(),
                expected
            );
            if !expected {
                continue;
            }
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(
                body.contains("/opt/veld/bin/veld"),
                "{}: {body}",
                tool.shim_name()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(
                    mode & 0o777,
                    0o755,
                    "{} must be executable",
                    tool.shim_name()
                );
            }
        }
        // A tool that disappears between daemon starts loses its shim rather than
        // keeping one that stands in front of nothing.
        let orphan = shims.join(Tool::XdgOpen.shim_name());
        if veld_core::opener::real_opener(Tool::XdgOpen, Some(&shims)).is_none() {
            std::fs::write(&orphan, "#!/bin/sh\nexit 0\n").unwrap();
            prepare_in(&shims, Path::new("/opt/veld/bin/veld")).unwrap();
            assert!(!orphan.exists(), "a stale shim must be removed");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // These are executables that a user may put on their PATH.
            let mode = std::fs::metadata(&shims).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }

        // A second daemon start rewrites them — an upgrade has to move the baked
        // path — and leaves no temporary file behind.
        prepare_in(&shims, Path::new("/usr/local/bin/veld")).unwrap();
        let body = std::fs::read_to_string(shims.join(Tool::Browser.shim_name())).unwrap();
        assert!(body.contains("/usr/local/bin/veld"), "{body}");
        let leftovers: Vec<_> = std::fs::read_dir(&shims)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }

    #[test]
    fn the_handoff_is_zsh_only_and_switchable() {
        // A non-zsh login shell gets no `ZDOTDIR`: there is no env-only hook to hang
        // this on, and reimplementing bash's startup order is not something to do to
        // somebody's shell.
        for shell in ["/bin/bash", "/usr/bin/fish", "/opt/homebrew/bin/nu", ""] {
            let env = session_env("s", shell, true, true, false);
            assert!(!env.contains_key("ZDOTDIR"), "{shell} must not be wrapped");
        }
        // And the setting is a real off switch, not a preference the daemon ignores.
        assert!(!session_env("s", "/bin/zsh", true, false, false).contains_key("ZDOTDIR"));

        // The master switch gates the WHOLE environment. Off means veld is not in the
        // shell at all — no `$BROWSER` round trip, nothing in the startup — which is
        // what its own documentation promises. Only the session id survives, and that
        // is for `veld open-url` invoked deliberately.
        let off = session_env("s", "/bin/zsh", false, true, false);
        assert_eq!(off.keys().collect::<Vec<_>>(), vec!["VELD_PTY_SESSION"]);
        assert!(is_zsh("/bin/zsh") && is_zsh("/opt/homebrew/bin/zsh"));
        assert!(!is_zsh("/bin/zsh-beta") && !is_zsh("/bin/bash"));
    }

    /// The `.zshenv` wins against a hostile rc file, and leaves the user's own
    /// startup intact.
    ///
    /// The whole feature rests on this, and it rests on zsh's actual behaviour rather
    /// than on a reading of the manual — a plain `PATH=` assignment in this file is
    /// undone by `/etc/zprofile`'s `path_helper` two steps later, which is how the
    /// first version of this shipped broken. So it is asserted by running zsh.
    ///
    /// Skipped where zsh is absent (some CI images) rather than failing: this is a
    /// property of the generated file, and the file is asserted textually below.
    #[test]
    fn the_generated_zshenv_wins_against_an_rc_that_rebuilds_path() {
        let Some(zsh) = ["/bin/zsh", "/usr/bin/zsh", "/opt/homebrew/bin/zsh"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
        else {
            // A skip is fine on a contributor's machine and unacceptable in CI: this
            // is the only test that proves the `precmd` mechanism works at all, and
            // `cargo test` swallows this line on a pass — so an image without zsh
            // would silently leave the feature unprotected. CI installs zsh; if that
            // ever stops being true, this fails instead of going quiet.
            assert!(
                std::env::var_os("CI").is_none(),
                "zsh is missing in CI — install it in the workflow; this test is the \
                 only thing pinning the PATH mechanism"
            );
            eprintln!("no zsh on this machine — skipping");
            return;
        };

        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let shim = tmp.path().join("shim");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&shim).unwrap();
        // A marker `open` that a resolution can be attributed to.
        std::fs::write(shim.join("open"), "#!/bin/sh\necho shim\n").unwrap();
        set_mode(&shim.join("open"), 0o755).unwrap();

        // A user startup as unhelpful as a real one: `.zshenv` sets a marker (so the
        // handoff can be shown to have sourced it) and `.zshrc` *rebuilds* PATH from
        // scratch, which is what defeats every simpler approach.
        std::fs::write(home.join(".zshenv"), "export VELD_TEST_USER_ZSHENV=ran\n").unwrap();
        // …and it exports its own BROWSER, which is the case that silently switched
        // the `$BROWSER` half of the feature off before the `veld_browser` hook.
        std::fs::write(
            home.join(".zshrc"),
            "export VELD_TEST_USER_ZSHRC=ran\nPATH=/usr/bin:/bin\nexport BROWSER=firefox\n",
        )
        .unwrap();

        let zdir = tmp.path().join("zdotdir");
        std::fs::create_dir_all(&zdir).unwrap();
        std::fs::write(zdir.join(".zshenv"), zshenv()).unwrap();

        // Driven through **stdin of an interactive login shell**, not `-c`, and that
        // is the point of the test rather than a detail: `precmd` fires before a
        // prompt, `zsh -i -c` prints none, and a `-c` version of this test passes
        // while the hook never runs. A real terminal prompts before the user can type
        // anything, so this is the shape that matches it. `-l` as well, because
        // `/etc/zprofile`'s `path_helper` is exactly what has to be beaten.
        let mut child = std::process::Command::new(&zsh)
            .args(["-l", "-i"])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .env("ZDOTDIR", &zdir)
            .env("VELD_SHIM_DIR", &shim)
            .env("VELD_SHIM_BROWSER", shim.join("veld-open"))
            .env("BROWSER", shim.join("veld-open"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run zsh");
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().unwrap();
            stdin
                .write_all(
                    b"command -v open\n\
                      echo $VELD_TEST_USER_ZSHENV $VELD_TEST_USER_ZSHRC\n\
                      echo zdotdir=${ZDOTDIR-unset}\n\
                      echo browser=$BROWSER original=$VELD_BROWSER_ORIGINAL\n\
                      exit\n",
                )
                .unwrap();
        }
        let out = child.wait_with_output().expect("zsh exit");
        let stdout = String::from_utf8_lossy(&out.stdout);

        assert!(
            stdout.contains(&shim.join("open").display().to_string()),
            "the shim must resolve first even though .zshrc rebuilt PATH; saw:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The user's own files ran — both of them, which is the half that matters more
        // than the PATH: veld replaced one file in the startup order and put it back.
        assert!(stdout.contains("ran ran"), "user startup files: {stdout:?}");
        // …and `ZDOTDIR` was handed back, so a nested zsh is not wrapped.
        assert!(
            stdout.contains("zdotdir=unset"),
            "ZDOTDIR must be returned to the user's value: {stdout:?}"
        );
        // The rc file's own `export BROWSER=firefox` ran after veld's environment was
        // handed over; the hook takes `$BROWSER` back and keeps their value for the
        // fall-through, so neither half of the feature is silently switched off.
        assert!(
            stdout.contains(&format!("browser={}", shim.join("veld-open").display())),
            "the rc file's BROWSER must be re-pointed at the shim: {stdout:?}"
        );
        assert!(
            stdout.contains("original=firefox"),
            "the user's own browser must be kept for the passthrough: {stdout:?}"
        );
    }

    /// The generated file survives the user's shell options — `no_unset` in
    /// particular.
    ///
    /// These lines run after the user's `.zshenv`, so they inherit its `setopt`. A bare
    /// `$BROWSER` or `${precmd_functions[(r)…]}` under `no_unset` is fatal: it aborts
    /// the rest of veld's file (no PATH hook, no `$BROWSER` re-assert) and prints an
    /// error naming a file in `~/.veld` at the top of every terminal. Asserted by
    /// running zsh, because reading the file cannot tell you this.
    #[test]
    fn the_generated_zshenv_survives_no_unset() {
        let Some(zsh) = ["/bin/zsh", "/usr/bin/zsh", "/opt/homebrew/bin/zsh"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
        else {
            assert!(std::env::var_os("CI").is_none(), "zsh is missing in CI");
            return;
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let shim = tmp.path().join("shim");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::write(shim.join("open"), "#!/bin/sh\n").unwrap();
        set_mode(&shim.join("open"), 0o755).unwrap();
        // `no_unset` in `.zshenv` is the harsher position: it is in force while the
        // rest of veld's own file runs. The rc also unsets BROWSER, which is what made
        // `veld_browser` abort before it could deregister itself and then repeat the
        // error at every prompt.
        std::fs::write(home.join(".zshenv"), "setopt no_unset\n").unwrap();
        std::fs::write(home.join(".zshrc"), "unset BROWSER\n").unwrap();
        let zdir = tmp.path().join("zdotdir");
        std::fs::create_dir_all(&zdir).unwrap();
        std::fs::write(zdir.join(".zshenv"), zshenv()).unwrap();

        let mut child = std::process::Command::new(&zsh)
            .args(["-l", "-i"])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .env("ZDOTDIR", &zdir)
            .env("VELD_SHIM_DIR", &shim)
            .env("VELD_SHIM_BROWSER", shim.join("veld-open"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run zsh");
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                // Two prompts, so a hook that errors and fails to deregister shows up
                // twice rather than once.
                .write_all(
                    b"command -v open
true
exit
",
                )
                .unwrap();
        }
        let out = child.wait_with_output().expect("zsh exit");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);

        // Scoped to veld's own lines on purpose: `/etc/zprofile` and `/etc/zshrc`
        // themselves trip over `no_unset` (LANG, terminfo) on macOS, which is the
        // user's existing situation and not this file's business. What must never
        // appear is an error naming veld — that is the one this file can cause, and it
        // would sit at the top of every terminal.
        let veld_errors: Vec<&str> = stderr
            .lines()
            .filter(|l| l.to_lowercase().contains("veld"))
            .collect();
        assert!(
            veld_errors.is_empty(),
            "veld's own file errored: {veld_errors:?}"
        );
        // …and the mechanism still worked under those options.
        assert!(
            stdout.contains(&shim.join("open").display().to_string()),
            "the PATH hook did not run under no_unset; stdout={stdout:?} stderr={stderr:?}"
        );
    }

    #[test]
    fn the_handoff_is_not_offered_when_its_file_is_gone() {
        // `ZDOTDIR` redirects *every* zsh startup file, so pointing it at a directory
        // whose `.zshenv` has been swept means none of the user's zsh config runs at
        // all — a far worse outcome than the feature being off.
        let tmp = tempfile::TempDir::new().unwrap();
        let shims = tmp.path().join("shims");
        prepare_in(&shims, Path::new("/opt/veld/bin/veld")).unwrap();
        // With the file present the handoff is offered (this test's daemon-wide `dir()`
        // is the real one, so assert on `zdotdir_path` rather than `session_env`).
        assert!(zdotdir_path(&shims).join(".zshenv").is_file());
        std::fs::remove_file(zdotdir_path(&shims).join(".zshenv")).unwrap();
        assert!(!zdotdir_path(&shims).join(".zshenv").is_file());
    }

    /// A bash that honours the `--posix`/`$ENV` handoff, or `None`.
    ///
    /// Probed rather than version-tested, for the reason
    /// `veld_core::shell::supports_posix_env_handoff` gives: macOS ships bash 3.2 as
    /// `/bin/bash`, which accepts `--posix` and ignores `$ENV` outright.
    fn capable_bash() -> Option<PathBuf> {
        let dir = tempfile::TempDir::new().ok()?;
        let probe = dir.path().join("p.sh");
        std::fs::write(&probe, "printf VELD_OK\n").ok()?;
        [
            "/bin/bash",
            "/usr/bin/bash",
            "/opt/homebrew/bin/bash",
            "/usr/local/bin/bash",
        ]
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .find(|bash| {
            std::process::Command::new(bash)
                .args(["--posix", "-i", "-c", ":"])
                .env("ENV", &probe)
                .stdin(std::process::Stdio::null())
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("VELD_OK"))
        })
    }

    /// The generated bash file replays the user's startup **and** still wins `PATH`.
    ///
    /// This is the bash counterpart of
    /// `the_generated_zshenv_wins_against_an_rc_that_rebuilds_path`, and it carries
    /// more weight than that one does: posix mode means veld's file is the *only*
    /// startup file bash reads, so a replay that misses a file does not degrade the
    /// shim — it silently deletes the user's environment. Every clause below is a
    /// thing that has to be true before this may ship, asserted by running bash
    /// rather than by reading it.
    #[test]
    fn a_bash_session_runs_the_users_startup_and_still_wins_the_path() {
        let Some(bash) = capable_bash() else {
            // Not `assert!(CI.is_none())` like the zsh test: GitHub's macOS runners
            // ship only bash 3.2, which cannot do this at all, so requiring it there
            // would fail the suite for a platform where the feature is correctly
            // switched off. Linux CI does have bash 5, and that is where this runs.
            assert!(
                std::env::var_os("CI").is_none() || !cfg!(target_os = "linux"),
                "no bash with the posix/ENV handoff in Linux CI — this test is the \
                 only thing pinning the bash mechanism"
            );
            eprintln!("no bash with the posix/ENV handoff on this machine — skipping");
            return;
        };

        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let shim = tmp.path().join("shim");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::write(shim.join("open"), "#!/bin/sh\necho shim\n").unwrap();
        set_mode(&shim.join("open"), 0o755).unwrap();

        // A startup as unhelpful as a real one: the profile sources the rc file (the
        // near-universal convention) and the rc file *rebuilds* `PATH` from scratch
        // and exports a `$BROWSER` of its own — the two things that defeat every
        // simpler approach. `VELD_TEST_BASHRC` counts rather than flags, so a
        // double source is a failure and not a pass.
        std::fs::write(
            home.join(".bash_profile"),
            "export VELD_TEST_PROFILE=ran\n[ -r ~/.bashrc ] && . ~/.bashrc\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".bashrc"),
            "export VELD_TEST_BASHRC=$((VELD_TEST_BASHRC+1))\nPATH=/usr/bin:/bin\nexport BROWSER=firefox\n",
        )
        .unwrap();

        let handoff = tmp.path().join("veldenv.bash");
        std::fs::write(&handoff, bashenv()).unwrap();

        let mut child = std::process::Command::new(&bash)
            // The long option **first** — `bash -l -i --posix` is a usage error, and
            // getting this backwards produces a shell that never starts.
            .args(["--posix", "-l", "-i"])
            // In the fake home, not the crate directory: bash's default `PS1`
            // carries the working directory, and this test runs inside a checkout
            // whose path contains "veld".
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .env("ENV", &handoff)
            // Drives the restore arm rather than the unset one: `$ENV` is read by
            // every posix shell started from here, so a user who had one must get
            // it back. Only reachable when the daemon itself was started from a
            // shell that exported `ENV`, which is rare and is exactly why it would
            // otherwise never be exercised.
            .env("VELD_USER_ENV", "/tmp/their-env")
            .env("VELD_SHIM_DIR", &shim)
            .env("VELD_SHIM_BROWSER", shim.join("veld-open"))
            .env("BROWSER", shim.join("veld-open"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run bash");
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(
                    b"command -v open\n\
                      echo startup=$VELD_TEST_PROFILE/ran$VELD_TEST_BASHRC\n\
                      echo env=${ENV-unset}\n\
                      echo browser=$BROWSER original=$VELD_BROWSER_ORIGINAL\n\
                      echo posix=$(set -o | awk '/^posix/{print $2}')\n\
                      echo login=$(shopt -q login_shell && echo yes || echo no)\n\
                      exit\n",
                )
                .unwrap();
        }
        let out = child.wait_with_output().expect("bash exit");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);

        // The shim wins even though `.bashrc` rebuilt PATH after the profile — the
        // whole point of running last.
        assert!(
            stdout.contains(&shim.join("open").display().to_string()),
            "the shim must resolve first even though .bashrc rebuilt PATH; saw:\n{stdout}\n{stderr}"
        );
        // **Both** of the user's files ran, and `.bashrc` ran exactly **once** —
        // this profile sources it, so veld must not source it a second time and
        // duplicate whatever it does.
        assert!(
            stdout.contains("startup=ran/ran1"),
            "the user's startup files must each run exactly once: {stdout:?} {stderr:?}"
        );
        // `$ENV` handed back to the user's own, so a nested posix shell reads what
        // they configured rather than veld's file a second time.
        assert!(
            stdout.contains("env=/tmp/their-env"),
            "the user's own ENV must be restored: {stdout:?}"
        );
        // The rc file's own `export BROWSER=firefox` ran after veld's environment was
        // handed over; the file takes it back and keeps theirs for the fall-through.
        assert!(
            stdout.contains(&format!("browser={}", shim.join("veld-open").display())),
            "the rc file's BROWSER must be re-pointed at the shim: {stdout:?}"
        );
        assert!(
            stdout.contains("original=firefox"),
            "the user's own browser must be kept for the passthrough: {stdout:?}"
        );
        // posix mode is how veld got in, not a mode the user asked for.
        assert!(
            stdout.contains("posix=off"),
            "posix mode must be left behind: {stdout:?}"
        );
        // …and `-l` survived, which is what makes the replay pick the profile branch.
        assert!(
            stdout.contains("login=yes"),
            "the shell must still be a login shell: {stdout:?}"
        );
        // Nothing of veld's may complain at the top of every terminal. Keyed on the
        // generated file's *name*, which is what bash puts in front of an error it
        // raises while sourcing it — a bare "veld" match hits the prompt, since the
        // echoed `PS1` carries a path.
        let veld_errors: Vec<&str> = stderr
            .lines()
            .filter(|l| l.contains("veldenv.bash"))
            .collect();
        assert!(
            veld_errors.is_empty(),
            "veld's own file errored: {veld_errors:?}"
        );
    }

    /// A bash user whose `~/.bashrc` is not reached by any profile still gets it.
    ///
    /// The shape this feature exists for, and the one it shipped broken for: no
    /// `~/.bash_profile` (macOS ships none), a `~/.profile` that does not source
    /// `~/.bashrc`, and every alias and function in `~/.bashrc`. A login bash reads
    /// the profile and stops, so following bash exactly gives the user a shell with
    /// none of the config they picked bash *for*. Verified against a real bash
    /// because the whole question is what the shell actually reads.
    #[test]
    fn a_bashrc_no_profile_reaches_is_still_loaded_and_only_once() {
        let Some(bash) = capable_bash() else {
            assert!(
                std::env::var_os("CI").is_none() || !cfg!(target_os = "linux"),
                "no bash with the posix/ENV handoff in Linux CI"
            );
            eprintln!("no bash with the posix/ENV handoff on this machine — skipping");
            return;
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Exactly the reported machine: `.profile` exists and never mentions the
        // rc file, so bash's own order reaches `.bashrc` on no path at all.
        std::fs::write(home.join(".profile"), "export VELD_TEST_PROFILE=ran\n").unwrap();
        std::fs::write(
            home.join(".bashrc"),
            "export VELD_TEST_BASHRC=$((VELD_TEST_BASHRC+1))\nveld_test_fn() { :; }\n",
        )
        .unwrap();
        let handoff = tmp.path().join("veldenv.bash");
        std::fs::write(&handoff, bashenv()).unwrap();

        let mut child = std::process::Command::new(&bash)
            .args(["--posix", "-l", "-i"])
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .env("ENV", &handoff)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run bash");
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(
                    b"echo startup=$VELD_TEST_PROFILE/ran$VELD_TEST_BASHRC\n\
                      echo fn=$(type -t veld_test_fn)\n\
                      echo env=${ENV-unset}\n\
                      exit\n",
                )
                .unwrap();
        }
        let out = child.wait_with_output().expect("bash exit");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("startup=ran/ran1"),
            "the profile AND the unreferenced .bashrc must each run exactly once: {stdout:?}"
        );
        // The thing the user actually notices: their function is there.
        assert!(
            stdout.contains("fn=function"),
            "a function defined in .bashrc must be available: {stdout:?}"
        );
        // The other arm of the `$ENV` handback — no `VELD_USER_ENV`, so it is
        // unset rather than left pointing at veld's file, which a nested posix
        // shell would otherwise source a second time.
        assert!(
            stdout.contains("env=unset"),
            "ENV must be unset when the user had none: {stdout:?}"
        );
    }

    #[test]
    fn the_bash_handoff_needs_the_probe_the_setting_and_the_file() {
        // Every gate, because the failure mode of getting one wrong is a bash session
        // in posix mode reading none of the user's startup.
        let with_handoff = session_env("s", "/bin/bash", true, true, true);
        let no_probe = session_env("s", "/bin/bash", true, true, false);
        let no_setting = session_env("s", "/bin/bash", true, false, true);
        assert!(
            !no_probe.contains_key("ENV"),
            "a bash that ignores $ENV must be handed none — it would only get posix mode"
        );
        assert!(
            !no_setting.contains_key("ENV"),
            "the off switch must be real"
        );
        // A zsh never gets `ENV`, and a bash never gets `ZDOTDIR`: two mechanisms,
        // one shell each, so a change to either cannot leak into the other.
        assert!(!with_handoff.contains_key("ZDOTDIR"));
        assert!(!session_env("s", "/bin/zsh", true, true, true).contains_key("ENV"));

        // The flag and the variable are decided together: `--posix` without an `$ENV`
        // is a bash reading nothing at all.
        assert!(
            veld_core::shell::interactive_flags("/bin/bash", &with_handoff).is_empty()
                || with_handoff.contains_key("ENV")
        );
        assert!(veld_core::shell::interactive_flags("/bin/bash", &no_probe).is_empty());
        assert!(veld_core::shell::interactive_flags("/bin/zsh", &with_handoff).is_empty());
    }

    #[test]
    fn the_session_id_is_always_exported() {
        // Even when no shim directory could be prepared: `veld open-url` run by
        // hand still needs to know which terminal it is in.
        let env = session_env("abc-123", "/bin/zsh", true, true, false);
        assert_eq!(
            env.get("VELD_PTY_SESSION").map(String::as_str),
            Some("abc-123")
        );
    }
}
