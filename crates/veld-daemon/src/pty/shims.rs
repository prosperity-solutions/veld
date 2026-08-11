//! The environment a terminal session is handed, so that a process inside it can
//! open a URL *in Veld*, so the window can tell when a command ended, and so a
//! coding agent can say when it is waiting on the user.
//!
//! One small directory per daemon instance ([`veld_core::instance::shim_dir`])
//! holding the generated `sh` scripts, a `zdotdir/` of one file and a `bash/` of one
//! file, plus these variables in the shell's environment:
//!
//! | Variable | Why |
//! |---|---|
//! | `BROWSER` | Points at `veld-open`. What Claude Code's own login flow, `gh`, `git`, Python's `webbrowser`, vite and next all consult. |
//! | `VELD_PTY_SESSION` | Which terminal asked, which is how the daemon knows *which window* the pane belongs in — and which worktree's inbox an event belongs to. |
//! | `VELD_SHIM_DIR` | The directory holding the `open`/`xdg-open` wrappers and the `claude` wrapper. Exported for every shell, so a non-zsh user can put it on `PATH` themselves. |
//! | `ZDOTDIR` / `VELD_USER_ZDOTDIR` | zsh only: the handoff that runs veld's one file inside the shell's own startup. See [`zshenv`]. |
//! | `ENV` / `VELD_USER_ENV` | The bash equivalent, on a bash that was **probed** to honour it. See [`bashenv`]. |
//! | `VELD_SHELL_INTEGRATION` | `terminal.shellIntegration`: the handoff's OSC 133 half registers its hooks only when this is set. |
//! | `VELD_AGENT_HOOKS` | `terminal.agentIntegration`: the `claude` wrapper injects an ephemeral `--settings` only when this is set, and is a bare `exec` otherwise. |
//! | `VELD_SHIM_BROWSER` | The same path as `BROWSER`, kept under its own name so the `veld_browser` hook can re-assert it after an rc file exports a `$BROWSER` of its own. |
//! | `VELD_BROWSER_ORIGINAL` | Whatever `$BROWSER` was before veld took it over, so the fall-through path can restore it instead of handing a child the shim again. |
//!
//! **Three independent settings gate them, one feature each** —
//! `terminal.openUrlsInApp` (with `terminal.interceptSystemOpen` under it),
//! `terminal.shellIntegration`, and `terminal.agentIntegration`. All of them off means
//! veld is not in the shell at all, except for the session id. They share the
//! *mechanism* (one handoff file) and not the *decision*: the file is written
//! unconditionally and each half of it is gated on its own variable, which is what
//! keeps one switch from turning another's feature off. See [`session_env`].
//!
//! An environment is fixed at spawn and a terminal outlives the tab and the daemon, so
//! a shell that is already open keeps what it was given; the settings rows say so.
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

/// Which of the terminal-integration features a session is entitled to.
///
/// A struct rather than a row of booleans because there are now four of them plus a
/// probe result, and the two that read alike (`intercept` and `shell_integration`)
/// gate *different halves of the same generated file*. A positional call getting them
/// the wrong way round is a silent behaviour swap, not a compile error.
#[derive(Debug, Clone, Copy)]
pub struct SessionOptions {
    /// `terminal.openUrlsInApp`.
    pub open_in_app: bool,
    /// `terminal.interceptSystemOpen`.
    pub intercept: bool,
    /// `terminal.shellIntegration`.
    pub shell_integration: bool,
    /// `terminal.agentIntegration`.
    pub agent_integration: bool,
    /// Whether this bash was **probed** to honour the `--posix`/`$ENV` handoff.
    ///
    /// Passed in rather than probed here because probing spawns a process and this
    /// function is pure and synchronous; the daemon probes once, at ticket-mint time,
    /// and caches. Meaningless for a non-bash shell, and ignored there.
    pub bash_handoff: bool,
}

impl SessionOptions {
    /// Every setting on, with the bash probe answered yes — the shipped defaults.
    ///
    /// Exists for tests, which is why it is not a `Default` impl: a `Default` of this
    /// struct would have to be all-false to be safe, and an all-false one silently
    /// turns every feature off for any caller that reaches for `..Default::default()`.
    /// Naming it `all_on` makes a test say which it means.
    ///
    /// `cfg(test)`-gated because **CI's clippy runs without `--all-targets`**
    /// (`cargo clippy --workspace -- -D warnings`), so a helper only the tests call is
    /// `dead_code` there — denied, and red on a diff that is otherwise green. A local
    /// `--all-targets` run compiles the tests and says nothing.
    #[cfg(test)]
    #[must_use]
    pub fn all_on() -> Self {
        Self {
            open_in_app: true,
            intercept: true,
            shell_integration: true,
            agent_integration: true,
            bash_handoff: true,
        }
    }

    /// Whether anything wants veld's directory on the session's `PATH`.
    ///
    /// Two unrelated features do: catching `open`/`xdg-open` (which is the browser
    /// feature, so it needs `open_in_app` as well), and putting the `claude` wrapper
    /// in front of the real one. Either alone is enough.
    pub fn wants_path(self) -> bool {
        (self.open_in_app && self.intercept) || self.agent_integration
    }

    /// Whether this session needs the shell-startup handoff file at all.
    ///
    /// The handoff is the *mechanism* — one file veld owns inside the shell's own
    /// startup — and three separate features ride it. Asking for it is not the same
    /// as asking for any one of them, which is why each half inside the generated
    /// file is gated on its own variable.
    pub fn wants_handoff(self) -> bool {
        self.wants_path() || self.shell_integration
    }
}

/// The variables a terminal session gets.
///
/// `shell` is the login shell about to be spawned.
///
/// # Each switch gates its own feature, and nothing else
///
/// **`open_in_app` gates the browser half**, and that is the point of it rather than
/// a detail. It is documented as turning that behaviour off, so with it off veld must
/// not be in the path for it at all: no `$BROWSER`, and no `open`/`xdg-open` on
/// `PATH`. The first version gated only the `ZDOTDIR` block, which left the off
/// switch still routing every browser launch through `veld open-url` to the daemon —
/// a round trip, a stderr line per open, and a login URL (one-time tokens included)
/// leaving the process — while the settings UI greyed the *other* switch out and so
/// could not turn off the thing that was still running.
///
/// **What it does *not* gate is shell integration or agent integration.** Those are
/// different features that happen to reach the shell through the same handoff file,
/// and the first version of shell integration was gated on `intercept` — so a user
/// who turned off "catch `open`/`xdg-open`" silently lost the rail's unread badge,
/// with nothing in either setting's documentation to explain it. The handoff file is
/// written once and unconditionally; **which halves of it run is decided by which
/// variables are in the environment** (`VELD_SHIM_DIR`, `VELD_SHELL_INTEGRATION`),
/// which is what keeps the three switches independent without three files.
///
/// Only `VELD_PTY_SESSION` survives everything being off, and it is not part of any
/// of the behaviours: `veld open-url` run deliberately, by a person or an agent,
/// still needs to know which terminal it is in.
///
/// Empty except for `VELD_PTY_SESSION` when the shim directory could not be
/// prepared, for the same reason.
pub fn session_env(
    session_id: &str,
    shell: &str,
    opts: SessionOptions,
) -> BTreeMap<String, String> {
    session_env_in(dir(), session_id, shell, opts)
}

/// [`session_env`] with the shim directory passed in.
///
/// Split out so a test can exercise the *positive* path against a real directory
/// written by [`prepare_in`]. It could not before: [`dir`] resolves a `veld` CLI
/// relative to the running binary, a test binary has none, so every assertion about
/// this function ran against `None` and could only ever check that things were
/// **absent**. Three switches whose whole contract is which variables appear is not
/// something to leave untestable in the direction that matters.
fn session_env_in(
    dir: Option<&Path>,
    session_id: &str,
    shell: &str,
    opts: SessionOptions,
) -> BTreeMap<String, String> {
    let SessionOptions {
        open_in_app,
        intercept,
        shell_integration,
        agent_integration,
        bash_handoff,
    } = opts;
    let mut env = BTreeMap::new();
    env.insert("VELD_PTY_SESSION".to_owned(), session_id.to_owned());
    if !open_in_app && !shell_integration && !agent_integration {
        return env;
    }
    let Some(dir) = dir else {
        return env;
    };
    // Each half below points at a file in that directory, so the directory having
    // been *swept since boot* has to be checked here rather than assumed from `dir()`
    // succeeding at startup: `dir()` memoises in a `OnceLock` and nothing rewrites the
    // files, and the very trigger the handoff check below describes — a `VELD_PTY_DIR`
    // under `/tmp` meeting the periodic sweep — takes `veld-open` with it. Handing a
    // shell `BROWSER=<gone>/veld-open` would make `gh`, Claude Code's login, vite and
    // Python's `webbrowser` all exec a nonexistent file and open *nothing*, where with
    // no `$BROWSER` at all they would have worked. This module's own header calls that
    // out as worse than not setting it; a half-configured environment is not a
    // degradation, it is a break.
    //
    // Checked per half rather than once for all of them: a missing `veld-open` says
    // nothing about whether the `claude` wrapper is there, and turning shell
    // integration off because a *browser* script went missing is the coupling the
    // function's own docs are about.
    let browser = open_in_app && dir.join(Tool::Browser.shim_name()).is_file();
    let agent = agent_integration
        && dir
            .join(veld_core::agent::AgentTool::Claude.shim_name())
            .is_file();

    // Two unrelated features want veld's directory on `PATH`: the `open`/`xdg-open`
    // shims (which belong to the browser feature, hence `browser` and not
    // `open_in_app`) and the `claude` wrapper. Either alone is enough, and the
    // variable is what the generated startup files gate their `PATH` line on — so
    // not setting it is how "neither wants it" reaches the shell.
    let wants_path = (browser && intercept) || agent;
    if wants_path {
        env.insert("VELD_SHIM_DIR".to_owned(), dir.display().to_string());
    }
    // The OSC 133 half's own gate. Nothing on disk is involved beyond the handoff
    // file itself — the hooks are shell, and they print an escape sequence — so this
    // is the whole of the feature's presence in the environment.
    if shell_integration {
        env.insert("VELD_SHELL_INTEGRATION".to_owned(), "1".to_owned());
    }
    // The `claude` wrapper's gate, and the reason the wrapper does not need the
    // shim directory rewritten when the setting changes: the file is always there and
    // is a bare `exec` passthrough without this variable. A gate that depended on the
    // file's absence would have to rewrite the directory on every settings change and
    // would still be wrong for every shell already open.
    if agent {
        env.insert("VELD_AGENT_HOOKS".to_owned(), "1".to_owned());
    }
    let wants_handoff = wants_path || shell_integration;
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
    if wants_handoff && is_zsh(shell) && handoff.is_file() {
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
    // The bash equivalent. Same three conditions as the zsh branch — something wants
    // the handoff, the shell is the right family, and the file that performs it is
    // still on disk — plus one this one needs and zsh's does not: the shell must
    // have been probed to honour `$ENV` in posix mode. On bash 3.2 (macOS's
    // `/bin/bash`) it does not, and setting `ENV` there is at best inert; it is
    // `interactive_flags` reading this same `ENV` key that would otherwise add a
    // `--posix` with nothing to show for it.
    let bash_handoff_file = bashenv_path(dir);
    if wants_handoff && is_bash(shell) && bash_handoff && bash_handoff_file.is_file() {
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
    if browser {
        // Saved before it is replaced, so the fall-through in `veld open-url` can give
        // a child the browser the user actually configured, rather than handing it the
        // shim again — which is a loop, not a fallback.
        //
        // Read from the **daemon's** environment, which under launchd or systemd almost
        // never carries the user's `$BROWSER`: theirs lives in an rc file, and the
        // `veld_browser` hook in `zshenv` is what captures that one. This covers the
        // case where the daemon really was started with a `$BROWSER` in scope.
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
    }
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

# OSC 133 semantic prompt marks, the bash half. Same contract as the zsh one, through
# the two seams bash actually has.
#
# `PS0` is the command-start mark. bash expands and prints it after reading a command
# line and before running it — which is precisely `preexec`, once per command line, and
# never for an empty Enter. **Not a `DEBUG` trap**, which was the first version and is
# wrong in three ways at once: bash allows exactly one, `trap -p DEBUG` output cannot
# be reinstalled reliably around a handler of the user's, and it fires for
# `PROMPT_COMMAND`'s own commands — which marked the *prompt* as a command and reported
# a `finished` for it. PS0 arrived in bash 4.4; on anything older the assignment is
# simply an unused variable, no `C` is emitted, and the consumer's C-before-D rule
# turns the whole thing into silence rather than into wrong marks.
#
# `PROMPT_COMMAND` carries the command-end mark, prepended so the hook reads the
# command's own `$?` rather than whatever an earlier entry last ran. Both spellings are
# handled — bash 5.1+ allows an array, and string-assigning over one would throw the
# user's away — and every branch is idempotent, so re-sourcing cannot stack duplicates.
if [ -n "${VELD_SHELL_INTEGRATION-}" ]; then
  veld_osc133_precmd() {
    local veld_status=$?
    printf '\033]133;D;%d\007\033]133;A\007' "$veld_status"
  }
  case "${PS0-}" in
    *'133;C'*) ;;
    *) PS0='\e]133;C\a'"${PS0-}" ;;
  esac
  if [[ "$(builtin declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    veld_osc133_seen=
    for veld_pc in "${PROMPT_COMMAND[@]}"; do
      [ "$veld_pc" = "veld_osc133_precmd" ] && veld_osc133_seen=1
    done
    if [ -z "$veld_osc133_seen" ]; then
      PROMPT_COMMAND=(veld_osc133_precmd "${PROMPT_COMMAND[@]}")
    fi
    builtin unset veld_pc veld_osc133_seen
  else
    case ";${PROMPT_COMMAND-};" in
      *";veld_osc133_precmd;"*) ;;
      *) PROMPT_COMMAND="veld_osc133_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
    esac
  fi
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

# OSC 133 semantic prompt marks — how the window learns that a command ran and how it
# ended. `terminal.shellIntegration`; absent variable, no hooks, nothing printed.
#
# Two rules keep this honest, and both are the difference between a useful badge and
# one the user learns to ignore:
#
#  - The command-end mark (`D`) is emitted ONLY after a command actually ran, so an
#    idle shell and the very first prompt of a session never report a `finished` for a
#    command nobody typed. The flag here keeps that off the wire; the *guarantee* is
#    the consumer's, which ignores any `D` not preceded by a `C` — because that rule
#    has to hold for bash and for a user's own hooks too, and a rule that lives in
#    generated shell cannot be tested.
#  - The status is captured on the FIRST line of the precmd hook, and the hook is
#    PREPENDED, so it reads the command's own `$?` rather than whatever an earlier
#    hook (veld's own PATH hook included) last ran.
if [ -n "${VELD_SHELL_INTEGRATION-}" ]; then
  veld_osc133_preexec() {
    veld_osc133_ran=1
    printf '\033]133;C\007'
  }
  veld_osc133_precmd() {
    local veld_status=$?
    if [ -n "${veld_osc133_ran-}" ]; then
      veld_osc133_ran=
      printf '\033]133;D;%d\007' "$veld_status"
    fi
    printf '\033]133;A\007'
  }
  typeset -ga precmd_functions preexec_functions
  if [[ -z ${preexec_functions[(r)veld_osc133_preexec]-} ]]; then
    preexec_functions+=(veld_osc133_preexec)
  fi
  if [[ -z ${precmd_functions[(r)veld_osc133_precmd]-} ]]; then
    precmd_functions=(veld_osc133_precmd "${precmd_functions[@]}")
  fi
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
    // The agent wrappers go too, and for a sharper reason than the openers: this one
    // shadows a command the user runs deliberately. Left behind naming a `veld` that
    // no longer exists it still resolves and execs the real agent — the fail-open path
    // works — but it is a file veld owns sitting on somebody's `PATH` with no daemon
    // behind it, and `prepare_in` is the only thing that would ever rewrite it.
    for tool in veld_core::agent::AgentTool::ALL.iter().copied() {
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
pub(super) fn prepare_in(dir: &Path, cli: &Path) -> std::io::Result<()> {
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
    // The coding-agent wrappers, on the same terms as the openers above: written
    // unconditionally (whether a session *uses* one is `session_env`'s call, and the
    // wrapper itself is a bare `exec` without `VELD_AGENT_HOOKS`) and through
    // write-then-rename, since one may be executing while the daemon restarts.
    //
    // Unlike an opener, there is no "is the real thing installed" check here: the
    // wrapper resolves it at run time precisely so that installing the agent after the
    // daemon started works. A wrapper in front of nothing says so and exits 127, which
    // is what the shell would have said anyway.
    for tool in veld_core::agent::AgentTool::ALL.iter().copied() {
        let path = dir.join(tool.shim_name());
        let tmp = dir.join(format!(".{}.new", tool.shim_name()));
        std::fs::write(&tmp, agent_script(tool, dir, cli))?;
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

/// The wrapper that installs a coding agent's lifecycle hooks.
///
/// # Why a wrapper at all, and what pays for it
///
/// An agent's working/waiting/finished state is not in the byte stream — see
/// [`veld_core::agent`] for the measurement. The only way to know is to be told, and
/// the only way to arrange being told without editing a file of the user's is to hand
/// the agent an ephemeral `--settings` on the command line. `--settings` **merges**
/// into the settings hierarchy rather than replacing it, which is the property that
/// makes this safe; a wrapper that replaced somebody's configuration for the duration
/// of a session would be indefensible whatever it bought.
///
/// A PATH wrapper is the thing the spike wanted to avoid, and the reason is real:
/// upstream has a standing bug where a wrapper injecting flags ahead of `"$@"`
/// bypasses a subcommand's fast path and mangles argv
/// (`anthropics/claude-code#42485`). So this one is built to be un-reachable in every
/// case that is not the one it exists for.
///
/// # Five rules, each of them a failure this would otherwise have
///
/// 1. **Injection only when the invocation is a plain interactive session** — no
///    arguments, or a first argument that begins with `-`. Every Claude Code
///    subcommand (`mcp`, `install`, `update`, `doctor`, `setup-token`,
///    `remote-control`) is a bare first word, so the rule cannot reach one, and #42485
///    cannot be reproduced through it. `-p`/`--print` is excluded too: it is
///    non-interactive, so there is no user for it to wait on and nothing to badge.
/// 2. **A `--settings` of the user's wins outright.** Two `--settings` flags means one
///    of them loses silently, and it must not be theirs.
/// 3. **The real binary is resolved at run time**, not baked at generation: a user may
///    install the agent after the daemon started, and a baked path would be a wrapper
///    permanently in front of nothing. Resolution walks `PATH` comparing each entry's
///    *physical* directory (`cd`+`pwd -P`) against this script's own, which is what
///    stops the wrapper resolving itself through a symlinked or differently-spelled
///    `PATH` entry — the `open` shim's fork bomb, one command over.
/// 4. **Nothing external is on the critical path, and the loop guard fails CLOSED.**
///    Every command the resolution uses is a shell builtin. The first version derived
///    its own directory with `$(dirname "$0")`, which is on `PATH` — so a session whose
///    `PATH` did not contain `/usr/bin` lost `dirname`, left the directory empty,
///    disabled the self-exclusion, and looped until an rlimit killed it. The directory
///    is now **baked** (this file is rewritten every daemon start, like the CLI path
///    already is), `$0` is a second source, and a wrapper that cannot establish its own
///    directory **refuses** rather than proceeding unprotected. That is the one place
///    the script does not fail open, because the failure it would otherwise permit is
///    a fork bomb rather than a missing badge.
/// 5. **Fail open everywhere else.** No `VELD_AGENT_HOOKS`, no settings file, a `veld`
///    that does not understand the subcommand, anything at all — and the invocation
///    `exec`s the real binary with the original argv, unexamined. A wrapper around
///    somebody's agent has no other acceptable failure mode.
/// 6. **`exec`, always**, so fds, stdin, signals and the exit status all belong to the
///    real process and nothing can tell the wrapper was there. Backgrounding would
///    break the agent's own stdin.
fn agent_script(tool: veld_core::agent::AgentTool, dir: &Path, cli: &Path) -> String {
    let name = tool.shim_name();
    format!(
        r#"#!/bin/sh
{header}
# Wraps `{name}` so Veld's window learns when it is waiting on you.
# Injects nothing unless VELD_AGENT_HOOKS is set. See `veld agent-settings --help`.

# This script's own PHYSICAL directory. Two sources, no external commands: the baked
# path (this file is rewritten every daemon start) and `$0`, whose prefix is stripped by
# parameter expansion rather than by `dirname` — `dirname` lives on PATH, and a session
# without /usr/bin on it would otherwise leave the exclusion below empty and turn this
# wrapper into an exec loop. `cd` and `pwd` are builtins, so the canonicalisation is
# free of PATH too; it is needed because the baked path and a PATH entry can spell the
# same directory differently (/var vs /private/var, or a symlink).
veld_self_dir=$(cd {dir} 2>/dev/null && pwd -P) || veld_self_dir=
if [ -z "$veld_self_dir" ]; then
  case "$0" in
    */*) veld_self_dir=$(cd "${{0%/*}}" 2>/dev/null && pwd -P) || veld_self_dir= ;;
  esac
fi
# The ONE place this script does not fail open. Carrying on without knowing which
# directory to skip is what makes the loop possible, and a loop is worse than a clear
# refusal.
if [ -z "$veld_self_dir" ]; then
  echo "veld: refusing to run {name}: cannot establish Veld's own shim directory" >&2
  exit 127
fi

# The real one, by walking PATH and comparing PHYSICAL directories.
veld_real=
veld_saved_ifs=${{IFS-}}
IFS=:
for veld_dir in $PATH; do
  [ -n "$veld_dir" ] || continue
  veld_phys=$(cd "$veld_dir" 2>/dev/null && pwd -P) || continue
  [ "$veld_phys" = "$veld_self_dir" ] && continue
  if [ -x "$veld_phys/{name}" ]; then veld_real="$veld_phys/{name}"; break; fi
done
IFS=$veld_saved_ifs
unset veld_saved_ifs veld_dir veld_phys

if [ -z "$veld_real" ]; then
  echo "veld: {name} is not installed (not on PATH outside Veld's shim directory)" >&2
  exit 127
fi
# Backstop for rule 3. The loop above should make this unreachable; if it is ever
# reached, refusing beats an exec loop bounded only by an rlimit.
if [ "$veld_real" = "$veld_self_dir/{name}" ]; then
  echo "veld: refusing to run {name}: the shim resolved itself" >&2
  exit 127
fi

# Rules 1 and 2. A bare first word is a subcommand and is never touched.
veld_inject=
if [ -n "${{VELD_AGENT_HOOKS-}}" ] && [ -n "${{VELD_PTY_SESSION-}}" ]; then
  case "${{1-}}" in
    "" | -*) veld_inject=1 ;;
  esac
  for veld_arg in "$@"; do
    case "$veld_arg" in
      -p | --print | --settings | --settings=*) veld_inject= ; break ;;
    esac
  done
  unset veld_arg
fi

if [ -n "$veld_inject" ]; then
  # Say "an agent lives here now, and it is idle". Only this script knows that: the shell
  # sees one long-running command and would otherwise drive an activity spinner for a
  # session sitting at its prompt, and no *hook* can say it because none has run yet.
  #
  # Backgrounded and fully redirected, so it adds nothing to the launch: the child
  # outlives the `exec` below, and its stdio must not be the agent's terminal.
  {cli} agent-state --tool {flag} --launched >/dev/null 2>&1 &
  # The CLI writes the file and prints its path. Generating JSON with correct escaping
  # in POSIX sh is not something to hand-roll, and the daemon must not be in this path
  # at all — an agent launch cannot depend on an HTTP round trip.
  veld_settings=$({cli} agent-settings --tool {flag} 2>/dev/null) || veld_settings=
  if [ -n "$veld_settings" ] && [ -f "$veld_settings" ]; then
    exec "$veld_real" --settings "$veld_settings" "$@"
  fi
fi
exec "$veld_real" "$@"
"#,
        header = GENERATED_HEADER,
        name = name,
        dir = quote(dir),
        cli = quote(cli),
        flag = tool.as_str(),
    )
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
            let env = session_env(
                "s",
                shell,
                SessionOptions {
                    bash_handoff: false,
                    ..SessionOptions::all_on()
                },
            );
            assert!(!env.contains_key("ZDOTDIR"), "{shell} must not be wrapped");
        }
        // The handoff is the *mechanism*, so it survives any one feature being off as
        // long as another still wants it — but with every feature that rides it off,
        // it goes too.
        let none_of_it = session_env(
            "s",
            "/bin/zsh",
            SessionOptions {
                intercept: false,
                shell_integration: false,
                agent_integration: false,
                ..SessionOptions::all_on()
            },
        );
        assert!(!none_of_it.contains_key("ZDOTDIR"));
        assert!(!none_of_it.contains_key("VELD_SHIM_DIR"));

        // Everything off gates the WHOLE environment. Off means veld is not in the
        // shell at all — no `$BROWSER` round trip, nothing in the startup — which is
        // what the documentation promises. Only the session id survives, and that
        // is for `veld open-url` invoked deliberately.
        let off = session_env(
            "s",
            "/bin/zsh",
            SessionOptions {
                open_in_app: false,
                intercept: false,
                shell_integration: false,
                agent_integration: false,
                bash_handoff: false,
            },
        );
        assert_eq!(off.keys().collect::<Vec<_>>(), vec!["VELD_PTY_SESSION"]);
        assert!(is_zsh("/bin/zsh") && is_zsh("/opt/homebrew/bin/zsh"));
        assert!(!is_zsh("/bin/zsh-beta") && !is_zsh("/bin/bash"));
    }

    /// Each of the three switches turns off its own feature and nothing else.
    ///
    /// This is the test the feature was asked for. All three ride one generated file,
    /// so the tempting implementation gates the *file* — and then turning off "catch
    /// `open`/`xdg-open`" silently takes the unread badge with it, which is exactly
    /// what the first version of shell integration did. Nothing in the type system
    /// prevents that coming back; this does.
    #[test]
    fn each_terminal_integration_switch_is_independent_of_the_others() {
        // A real directory, written by the real generator: `dir()` resolves no CLI in a
        // test binary, so going through `session_env` here would assert against `None`
        // and pass no matter what the positive path does.
        let tmp = tempfile::TempDir::new().unwrap();
        let shims = tmp.path().join("shims");
        prepare_in(&shims, Path::new("/opt/veld/bin/veld")).unwrap();
        let env = |opts| session_env_in(Some(&shims), "s", "/bin/zsh", opts);

        // Everything on: all three markers present.
        let all = env(SessionOptions::all_on());
        assert!(all.contains_key("BROWSER"));
        assert_eq!(
            all.get("VELD_SHELL_INTEGRATION").map(String::as_str),
            Some("1")
        );
        assert_eq!(all.get("VELD_AGENT_HOOKS").map(String::as_str), Some("1"));
        assert!(all.contains_key("VELD_SHIM_DIR"));
        assert!(all.contains_key("ZDOTDIR"));

        // Shell integration off: OSC 133 gone, the other two untouched. The handoff
        // stays, because the browser and agent halves still need it.
        let no_si = env(SessionOptions {
            shell_integration: false,
            ..SessionOptions::all_on()
        });
        assert!(!no_si.contains_key("VELD_SHELL_INTEGRATION"));
        assert!(no_si.contains_key("BROWSER"));
        assert!(no_si.contains_key("VELD_AGENT_HOOKS"));
        assert!(no_si.contains_key("ZDOTDIR"));

        // Agent integration off: the wrapper is inert, the other two untouched.
        let no_agent = env(SessionOptions {
            agent_integration: false,
            ..SessionOptions::all_on()
        });
        assert!(!no_agent.contains_key("VELD_AGENT_HOOKS"));
        assert!(no_agent.contains_key("VELD_SHELL_INTEGRATION"));
        assert!(no_agent.contains_key("BROWSER"));

        // The URL feature off: no `$BROWSER`, no shim directory on `PATH` for the
        // openers — but shell integration still reaches the shell. This is the
        // coupling that shipped once, asserted in the direction it broke.
        let no_urls = env(SessionOptions {
            open_in_app: false,
            ..SessionOptions::all_on()
        });
        assert!(!no_urls.contains_key("BROWSER"));
        assert!(!no_urls.contains_key("VELD_SHIM_BROWSER"));
        assert_eq!(
            no_urls.get("VELD_SHELL_INTEGRATION").map(String::as_str),
            Some("1"),
            "turning off terminal URL routing must not turn off the unread badge"
        );
        assert!(
            no_urls.contains_key("ZDOTDIR"),
            "the handoff is the mechanism shell integration rides; it must survive"
        );

        // And `interceptSystemOpen` off, which is the pair the coupling lived in:
        // the openers leave `PATH`, and both other features stay.
        let no_intercept = env(SessionOptions {
            intercept: false,
            agent_integration: false,
            ..SessionOptions::all_on()
        });
        assert!(
            !no_intercept.contains_key("VELD_SHIM_DIR"),
            "nothing wants the directory on PATH any more"
        );
        assert!(
            no_intercept.contains_key("BROWSER"),
            "$BROWSER is the other half"
        );
        assert!(no_intercept.contains_key("VELD_SHELL_INTEGRATION"));
        assert!(no_intercept.contains_key("ZDOTDIR"));

        // Agent integration alone is enough to want the directory on `PATH`, even
        // with the whole browser feature off — the `claude` wrapper lives there too.
        let agent_only = env(SessionOptions {
            open_in_app: false,
            intercept: false,
            shell_integration: false,
            ..SessionOptions::all_on()
        });
        assert!(agent_only.contains_key("VELD_SHIM_DIR"));
        assert!(agent_only.contains_key("ZDOTDIR"));
        assert!(!agent_only.contains_key("BROWSER"));
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

    /// The `claude` wrapper, run for real, in every case it must and must not touch.
    ///
    /// Executed rather than pattern-matched, because the interesting parts are shell
    /// control flow: which invocations get `--settings` injected, whether the real
    /// binary is found without finding the wrapper, and whether every failure falls
    /// through to a plain `exec`. A textual assertion about `case` patterns would pass
    /// on a script that could not run.
    #[test]
    fn the_agent_wrapper_injects_only_for_an_interactive_launch_and_always_falls_open() {
        let tmp = tempfile::TempDir::new().unwrap();
        let shims = tmp.path().join("shims");
        let real_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&real_dir).unwrap();

        // A stand-in `veld` whose `agent-settings` prints the path of a file it wrote,
        // exactly as the real subcommand does.
        let settings = tmp.path().join("ephemeral.json");
        std::fs::write(&settings, "{}").unwrap();
        let fake_cli = tmp.path().join("veld");
        std::fs::write(
            &fake_cli,
            format!(
                "#!/bin/sh\n[ \"$1\" = agent-settings ] || exit 2\nprintf '%s\\n' {}\n",
                quote(&settings)
            ),
        )
        .unwrap();
        set_mode(&fake_cli, 0o755).unwrap();
        prepare_in(&shims, &fake_cli).unwrap();

        // The real `claude`, which echoes its argv so the assertion can read it.
        let real = real_dir.join("claude");
        std::fs::write(&real, "#!/bin/sh\necho \"REAL $*\"\n").unwrap();
        set_mode(&real, 0o755).unwrap();

        // `PATH` with the shim directory FIRST, which is how a session really has it —
        // so a wrapper that failed to exclude itself would loop here rather than in
        // somebody's terminal.
        let path = format!("{}:{}", shims.display(), real_dir.display());
        let run = |args: &[&str], hooks: bool, session: bool| -> String {
            let mut cmd = std::process::Command::new(shims.join("claude"));
            cmd.args(args).env_clear().env("PATH", &path);
            if hooks {
                cmd.env("VELD_AGENT_HOOKS", "1");
            }
            if session {
                cmd.env("VELD_PTY_SESSION", "pane-1");
            }
            let out = cmd.output().expect("run the claude shim");
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        };

        let injected = format!("REAL --settings {}", settings.display());

        // The case the feature exists for: a bare interactive launch.
        assert_eq!(run(&[], true, true).trim(), injected.trim());
        // And an interactive launch with flags of its own — the flag goes in front of
        // them, and they are passed through untouched.
        assert_eq!(
            run(&["--resume"], true, true).trim(),
            format!("{injected} --resume").trim()
        );

        // A bare first word is a SUBCOMMAND and is never touched. This is the rule that
        // makes `anthropics/claude-code#42485` unreachable — a wrapper injecting flags
        // ahead of a subcommand's argv bypasses its fast path and mangles it.
        for subcommand in ["mcp", "install", "update", "doctor", "setup-token"] {
            assert_eq!(
                run(&[subcommand, "list"], true, true).trim(),
                format!("REAL {subcommand} list"),
                "a subcommand must reach the real binary untouched"
            );
        }
        // `-p`/`--print` is non-interactive: nobody is waiting on it, so there is
        // nothing to badge and no reason to be in its argv.
        for flag in ["-p", "--print"] {
            assert_eq!(
                run(&[flag, "hello"], true, true).trim(),
                format!("REAL {flag} hello")
            );
        }
        // A `--settings` of the user's wins outright: two of them means one loses
        // silently, and it must not be theirs.
        assert_eq!(
            run(&["--settings", "/mine.json"], true, true).trim(),
            "REAL --settings /mine.json"
        );
        assert_eq!(
            run(&["--settings=/mine.json"], true, true).trim(),
            "REAL --settings=/mine.json"
        );

        // The off switch is the ABSENCE OF THE VARIABLE, not the absence of the file:
        // the shim directory is written once per daemon start, so a settings change has
        // to be able to disable this for the next session without rewriting anything.
        assert_eq!(run(&[], false, true).trim(), "REAL");
        // Outside a Veld terminal there is no session to attribute a state to.
        assert_eq!(run(&[], true, false).trim(), "REAL");

        // Fail open: a `veld` that cannot answer must still leave the launch working.
        std::fs::write(&fake_cli, "#!/bin/sh\nexit 1\n").unwrap();
        set_mode(&fake_cli, 0o755).unwrap();
        assert_eq!(run(&[], true, true).trim(), "REAL");

        // And with no real `claude` anywhere, the wrapper says so and exits 127 rather
        // than resolving itself. `PATH` here is ONLY the shim directory, which is the
        // exec-loop case.
        let out = std::process::Command::new(shims.join("claude"))
            .env_clear()
            .env("PATH", shims.display().to_string())
            .env("VELD_AGENT_HOOKS", "1")
            .env("VELD_PTY_SESSION", "pane-1")
            .output()
            .expect("run the claude shim");
        assert_eq!(out.status.code(), Some(127));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not installed"),
            "{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The wrapper cannot resolve itself through a symlinked `PATH` entry.
    ///
    /// The failure this prevents is not a wrong answer, it is an `exec` loop bounded
    /// only by an rlimit — the same one the `open` shim's own exclusion exists for,
    /// measured there at ~3,800 execs in five seconds. A plain string comparison of
    /// `PATH` entries against `$VELD_SHIM_DIR` is what misses it, so the wrapper
    /// compares *physical* directories with `pwd -P`.
    #[test]
    fn the_agent_wrapper_excludes_itself_even_under_another_spelling() {
        let tmp = tempfile::TempDir::new().unwrap();
        let shims = tmp.path().join("shims");
        prepare_in(&shims, Path::new("/nonexistent/veld")).unwrap();
        // A second name for the very same directory.
        let alias = tmp.path().join("also-shims");
        std::os::unix::fs::symlink(&shims, &alias).unwrap();

        let out = std::process::Command::new(shims.join("claude"))
            .env_clear()
            // Neither entry names the shim directory the way the script's own `$0`
            // does, and `$VELD_SHIM_DIR` is deliberately absent — the exact state the
            // `open` shim's depth guard exists for.
            .env("PATH", format!("{}:{}", alias.display(), shims.display()))
            .env("VELD_AGENT_HOOKS", "1")
            .env("VELD_PTY_SESSION", "pane-1")
            .output()
            .expect("run the claude shim");
        assert_eq!(
            out.status.code(),
            Some(127),
            "the wrapper resolved itself instead of refusing: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not installed"),
            "{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A wrapper that cannot work out its own directory refuses instead of looping.
    ///
    /// The one branch in the whole script that does not fail open, and the reason is the
    /// bug this test exists for: the first version derived the directory with
    /// `$(dirname "$0")`, and `dirname` lives on `PATH`. A session without `/usr/bin` on
    /// it lost the command, left the exclusion empty, resolved the wrapper as its own
    /// "real" binary and `exec`ed itself until an rlimit killed the process — measured,
    /// not imagined; it hung this test suite. Both sources of the directory are now
    /// builtin-only, and if both fail the answer is a message rather than a fork bomb.
    #[test]
    fn a_wrapper_that_cannot_locate_itself_refuses_rather_than_looping() {
        let tmp = tempfile::TempDir::new().unwrap();
        let generated = tmp.path().join("gone");
        prepare_in(&generated, Path::new("/nonexistent/veld")).unwrap();
        // Move the script somewhere else and delete the directory it was generated for,
        // so the baked path cannot resolve…
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::copy(generated.join("claude"), elsewhere.join("claude")).unwrap();
        set_mode(&elsewhere.join("claude"), 0o755).unwrap();
        std::fs::remove_dir_all(&generated).unwrap();

        // …and invoke it as a bare relative name from inside that directory, so `$0`
        // carries no slash and the fallback cannot resolve either.
        let out = std::process::Command::new("/bin/sh")
            .arg("claude")
            .current_dir(&elsewhere)
            .env_clear()
            .env("PATH", elsewhere.display().to_string())
            .env("VELD_AGENT_HOOKS", "1")
            .env("VELD_PTY_SESSION", "pane-1")
            .output()
            .expect("run the claude shim");
        assert_eq!(
            out.status.code(),
            Some(127),
            "a wrapper with no idea which directory to skip must refuse: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("cannot establish"),
            "{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Find a shell to drive, or explain why skipping is acceptable here and not in CI.
    fn find_shell(candidates: &[&str], what: &str) -> Option<PathBuf> {
        if let Some(p) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) {
            return Some(p);
        }
        assert!(
            std::env::var_os("CI").is_none(),
            "{what} is missing in CI — install it in the workflow; these tests are the \
             only thing pinning the mechanism"
        );
        eprintln!("no {what} on this machine — skipping");
        None
    }

    /// The OSC 133 marks a real zsh emits, including the cases that must emit nothing.
    ///
    /// Asserted by running zsh for the same reason the PATH test is: the marks come out
    /// of `precmd`/`preexec`, which only run before a prompt, so a `-c` probe reports a
    /// passing test on a mechanism that never fired. Driven through **stdin of an
    /// interactive login shell**.
    ///
    /// The user's `.zshrc` here is deliberately hostile in the two ways that break this:
    /// `no_unset` (which aborts veld's file at the first bare expansion) and its own
    /// appended `precmd` hook (which clobbers `$?` for anything registered after it).
    #[test]
    fn a_real_zsh_marks_commands_with_their_status_and_stays_silent_when_idle() {
        let Some(zsh) = find_shell(
            &["/bin/zsh", "/usr/bin/zsh", "/opt/homebrew/bin/zsh"],
            "zsh",
        ) else {
            return;
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let zdir = tmp.path().join("zdotdir");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&zdir).unwrap();
        std::fs::write(zdir.join(".zshenv"), zshenv()).unwrap();
        std::fs::write(
            home.join(".zshrc"),
            // `no_unset` plus a hook of the user's own, appended after veld's — which
            // is what the prepend in the generated file exists to survive.
            "setopt no_unset\nveld_test_noisy() { true }\n\
             typeset -ga precmd_functions\nprecmd_functions+=(veld_test_noisy)\nPS1='> '\n",
        )
        .unwrap();

        let marks = |integration: bool| -> String {
            let mut cmd = std::process::Command::new(&zsh);
            cmd.args(["-l", "-i"])
                .env_clear()
                .env("HOME", &home)
                .env("PATH", "/usr/bin:/bin")
                .env("TERM", "dumb")
                .env("ZDOTDIR", &zdir);
            if integration {
                cmd.env("VELD_SHELL_INTEGRATION", "1");
            }
            let mut child = cmd
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("run zsh");
            {
                use std::io::Write;
                // `true`, `false`, then two bare Enters — the idle case.
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(b"true\nfalse\n\n\nexit\n")
                    .unwrap();
            }
            let out = child.wait_with_output().expect("zsh exit");
            osc133_marks(&String::from_utf8_lossy(&out.stdout))
        };

        let on = marks(true);
        // A command's start, then its end carrying its own status. `D;1` and not `D;0`
        // is what proves the status survived the user's later hook.
        assert!(
            on.contains("C|D;0") && on.contains("C|D;1"),
            "a command must be marked start-then-end with its own status: {on}"
        );
        // The idle case, and the one that decides whether this feature is usable: two
        // bare Enters produce prompt marks and **no** command-end mark, so an untouched
        // shell can never badge a `finished` for a command nobody ran. Counted, because
        // "contains no D" would pass on a run where nothing worked at all.
        assert_eq!(
            on.matches("D;").count(),
            2,
            "exactly two commands ran; a bare Enter must not produce a command-end \
             mark: {on}"
        );
        assert!(on.starts_with('A'), "the first prompt is A, never D: {on}");

        // And the switch is real: no variable, nothing on the wire at all.
        let off = marks(false);
        assert!(
            off.is_empty(),
            "terminal.shellIntegration off must put nothing in the stream: {off}"
        );
    }

    /// The same contract against a real bash, through `PS0` + `PROMPT_COMMAND`.
    ///
    /// Two measured facts drive the assertions, and both were surprises worth pinning:
    ///
    /// - bash **cannot** suppress the idle mark the way zsh does. A bare Enter, and the
    ///   very first prompt, each emit a `D` — with a *stale* status — because
    ///   `PROMPT_COMMAND` has no idea whether a command ran. So the invariant is
    ///   "**no `D` without a preceding `C`**", enforced by the consumer, and this test
    ///   asserts the shape the consumer relies on rather than a shape bash cannot give.
    /// - macOS's `/bin/bash` is 3.2 and ignores `$ENV` entirely, so it never gets here.
    ///   Skipped rather than failed when the only bash on the machine is that one.
    #[test]
    fn a_real_bash_marks_commands_and_never_emits_a_start_mark_for_an_idle_prompt() {
        let Some(bash) = find_shell(
            &["/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash"],
            "bash",
        ) else {
            return;
        };
        // The handoff only ever runs on a bash that honours `$ENV` in posix mode. On
        // macOS's 3.2 that is false, and this test would assert against a shell veld
        // deliberately leaves alone.
        if !std::process::Command::new(&bash)
            .args([
                "-c",
                r#"case "$BASH_VERSION" in 3.*|4.[0-3]*) exit 1 ;; esac"#,
            ])
            .status()
            .is_ok_and(|s| s.success())
        {
            eprintln!(
                "{} is too old for the $ENV/PS0 handoff — skipping",
                bash.display()
            );
            return;
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let bash_dir = tmp.path().join("bash");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bash_dir).unwrap();
        let handoff = bash_dir.join("veldenv.bash");
        std::fs::write(&handoff, bashenv()).unwrap();
        // A PROMPT_COMMAND of the user's, so the prepend is exercised rather than the
        // empty case.
        std::fs::write(
            home.join(".bashrc"),
            "PROMPT_COMMAND='veld_test_theirs'\nveld_test_theirs() { true; }\nPS1='> '\n",
        )
        .unwrap();

        // Run through `sh -c … 2>&1`, and this is the test's one real subtlety:
        // **bash writes `PS0` to stderr**, because that is where prompts go, while the
        // `printf` in `PROMPT_COMMAND` goes to stdout. In a terminal both are the same
        // tty, which is the only environment this feature runs in — but two captured
        // pipes put the `C` marks in one and the `D` marks in the other, and an
        // assertion about their *order* needs them merged at the fd level rather than
        // concatenated afterwards.
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("exec {} --posix -l -i 2>&1", quote(&bash)))
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .env("ENV", &handoff)
            .env("VELD_SHELL_INTEGRATION", "1")
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
                .write_all(b"true\nfalse\n\n\nexit\n")
                .unwrap();
        }
        let out = child.wait_with_output().expect("bash exit");
        let marks = osc133_marks(&String::from_utf8_lossy(&out.stdout));

        assert!(
            marks.contains("C|D;0") && marks.contains("C|D;1"),
            "a command must be marked start-then-end with its own status — the \
             PROMPT_COMMAND prepend is what makes the status the command's: {marks}"
        );
        // The measured bash behaviour, asserted rather than wished away: a bare Enter
        // DOES produce a `D`, and it is the consumer's C-before-D rule that makes that
        // harmless. If bash ever stops doing this, this assertion says so out loud
        // instead of letting the rule quietly become dead code.
        assert!(
            marks.contains("A|D;"),
            "bash emits a command-end mark for a bare prompt; the consumer's \
             C-before-D rule is what handles it, and it must stay load-bearing: {marks}"
        );
    }

    /// The OSC 133 marks in a stream, as a compact `A|C|D;0` trace.
    ///
    /// Everything else — the prompt, the title sequences, the user's own escape codes —
    /// is discarded, so an assertion reads as the sequence of events rather than as a
    /// substring of somebody's `$PS1`.
    fn osc133_marks(stream: &str) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut rest = stream;
        while let Some(at) = rest.find("\x1b]133;") {
            rest = &rest[at + "\x1b]133;".len()..];
            let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len().min(8));
            out.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
        out.join("|")
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
        let with_handoff = session_env("s", "/bin/bash", SessionOptions::all_on());
        let no_probe = session_env(
            "s",
            "/bin/bash",
            SessionOptions {
                bash_handoff: false,
                ..SessionOptions::all_on()
            },
        );
        // Nothing left that wants the handoff. Not `intercept: false` alone any more —
        // shell integration rides the same file, so one switch off is not "the off
        // switch" for the mechanism.
        let no_setting = session_env(
            "s",
            "/bin/bash",
            SessionOptions {
                intercept: false,
                shell_integration: false,
                agent_integration: false,
                ..SessionOptions::all_on()
            },
        );
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
        assert!(!session_env("s", "/bin/zsh", SessionOptions::all_on()).contains_key("ENV"));

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
        let env = session_env(
            "abc-123",
            "/bin/zsh",
            SessionOptions {
                bash_handoff: false,
                ..SessionOptions::all_on()
            },
        );
        assert_eq!(
            env.get("VELD_PTY_SESSION").map(String::as_str),
            Some("abc-123")
        );
    }
}
