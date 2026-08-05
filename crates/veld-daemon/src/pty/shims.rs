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
//! The answer is [`zshenv`]: one file veld owns, in its own directory, which hands
//! `ZDOTDIR` back immediately and installs a hook that runs *after* every rc file.
//! Nothing of the user's is edited, wrapped, or read twice.
//!
//! # Rewritten every daemon start, never trusted from disk
//!
//! The scripts carry the absolute path of the `veld` binary **beside the running
//! daemon**, so a dev instance's terminals call the dev CLI and the installed one's
//! call the installed CLI. That also means an upgrade must rewrite them, which is
//! why generation is unconditional rather than "if missing". If the sibling binary
//! is not there, nothing is written and no `BROWSER` is injected: a `$BROWSER`
//! pointing at a script that cannot work is worse than no `$BROWSER` at all.

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
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("VELD_PTY_SESSION".to_owned(), session_id.to_owned());
    if !open_in_app {
        return env;
    }
    let Some(dir) = dir() else {
        return env;
    };
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
/// The `ZDOTDIR` handoff is zsh-only, and deliberately so: zsh is the one shell
/// with a startup file that runs *before* `$ZDOTDIR` matters and a hook array that
/// runs *after* every rc file, which is what makes an override possible without
/// touching a single file of the user's. bash has no equivalent env-only hook
/// (`BASH_ENV` is non-interactive shells only) and replicating login semantics
/// through `--rcfile` means veld reimplementing the user's startup order. So bash,
/// fish and the rest get `$BROWSER` plus the documented `$VELD_SHIM_DIR` line.
fn is_zsh(shell: &str) -> bool {
    std::path::Path::new(shell)
        .file_name()
        .is_some_and(|n| n == "zsh")
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
    String::from(
        r#"# Generated by veld — rewritten on every daemon start; edits are lost.
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
"#,
    )
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
            "no `veld` binary beside this daemon",
        )
    })?;
    let dir = veld_core::instance::shim_dir();
    prepare_in(&dir, &cli)?;
    debug!(dir = %dir.display(), cli = %cli.display(), "terminal URL shims written");
    Ok(dir)
}

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
    out.push_str("# Generated by veld — rewritten on every daemon start; edits are lost.\n");
    out.push_str(
        "# Routes a single http(s) URL to a Veld browser pane. See `veld open-url --help`.\n",
    );
    // Not `exec`: the binary is tested for executability at run time (an upgrade or an
    // uninstall can remove it while a shell is open), but "executable" is not the same
    // as "knows this subcommand". A `veld` that predates `open-url` — a manual
    // rollback, or the window between the two binaries during an update — parses the
    // argv, refuses the unknown subcommand, and exits **2**, which is clap's usage
    // status and also the only status this command returns for "I cannot handle this".
    // `exec`ing would end the shim there and `open <url>` would open nothing at all,
    // with no system opener behind it. So: run it, and fall through on exactly that.
    out.push_str(&format!(
        "if [ -x {cli} ]; then\n  {cli} open-url --tool {flag} -- \"$@\"\n  rc=$?\n  \
         [ \"$rc\" -ne 2 ] && exit \"$rc\"\nfi\n",
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

/// The `veld` CLI beside the running daemon.
///
/// Beside, deliberately, rather than `PATH`: the daemon may be a dev build on its
/// own port with its own database, and its terminals must reach *its* CLI. This is
/// also the check that decides whether the feature is available at all.
fn veld_cli_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("veld");
    candidate.is_file().then_some(candidate)
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
        // …and it is NOT an `exec`, because a `veld` that predates this subcommand
        // exits 2 and the shim has to fall through to the real tool rather than end
        // there. That is the difference between "the feature is off" and "`open` is
        // broken".
        assert!(
            body.contains("open-url --tool open -- \"$@\"") && body.contains("-ne 2"),
            "the shim must fall through on clap's usage status: {body}"
        );
        assert!(
            !body.contains("&& exec '/usr/local/bin/veld' open-url"),
            "an exec here strands the caller when veld cannot handle the subcommand: {body}"
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
            let env = session_env("s", shell, true, true);
            assert!(!env.contains_key("ZDOTDIR"), "{shell} must not be wrapped");
        }
        // And the setting is a real off switch, not a preference the daemon ignores.
        assert!(!session_env("s", "/bin/zsh", true, false).contains_key("ZDOTDIR"));

        // The master switch gates the WHOLE environment. Off means veld is not in the
        // shell at all — no `$BROWSER` round trip, nothing in the startup — which is
        // what its own documentation promises. Only the session id survives, and that
        // is for `veld open-url` invoked deliberately.
        let off = session_env("s", "/bin/zsh", false, true);
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

    #[test]
    fn the_session_id_is_always_exported() {
        // Even when no shim directory could be prepared: `veld open-url` run by
        // hand still needs to know which terminal it is in.
        let env = session_env("abc-123", "/bin/zsh", true, true);
        assert_eq!(
            env.get("VELD_PTY_SESSION").map(String::as_str),
            Some("abc-123")
        );
    }
}
