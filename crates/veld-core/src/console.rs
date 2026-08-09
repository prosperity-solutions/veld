//! Open a real terminal window and run a command in it.
//!
//! Written for exactly one caller — `veld update`, when Veld Desktop handed the
//! update over — and the two reasons it exists are worth stating, because both
//! are properties of a *terminal*, not of a subprocess:
//!
//! 1. **A user can see it.** The handed-off route quits the app and then spends
//!    1–4 minutes downloading, installing and restarting services with no
//!    surface to render on. There is no update IPC channel in the Electron app,
//!    and there cannot usefully be one: the app is deliberately dead for most of
//!    the run so its own bundle can be replaced.
//! 2. **`sudo` can ask.** On a privileged install the helper is a root service,
//!    so restarting it onto the new binary needs privilege. A detached child has
//!    no controlling terminal, so it only ever gets `sudo -n` — silently failing
//!    whenever the credential is not already cached. A terminal window is a
//!    controlling terminal, and the password prompt lands where a human is
//!    looking.
//!
//! ## Which terminal
//!
//! The user's, not ours. macOS has no API for "the default terminal
//! application", but it does have LaunchServices: a `.command` file opened with
//! no `-a` goes to whatever the user has registered for it, which is Terminal.app
//! unless they chose otherwise. Linux has the same idea spelled three ways —
//! `$TERMINAL`, Debian's `x-terminal-emulator` alternative, and then the zoo.
//!
//! ## What "it worked" means
//!
//! Not much, and that is the important caveat. `open` and every Linux emulator
//! are fire-and-forget: a zero exit status says a launcher was started, not that
//! a window appeared or that anything ran in it. So a caller must **not** treat
//! `Ok` as done. `veld update` confirms the real thing instead — it waits for the
//! update lock to be taken by a different pid, which only the command inside the
//! window can do — and falls back to running headless when that never happens.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Why no window opened.
#[derive(Debug)]
pub enum ConsoleError {
    /// Not a platform with a desktop terminal to open (Windows is not a veld
    /// target at all; this is the catch-all).
    Unsupported,
    /// Linux with no session to put a window in — a container, a bare SSH login,
    /// a CI runner.
    NoDisplay,
    /// A desktop, but nothing that could be launched as a terminal.
    NoTerminal,
    Io(io::Error),
}

impl std::fmt::Display for ConsoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsoleError::Unsupported => write!(f, "this platform has no terminal to open"),
            ConsoleError::NoDisplay => write!(f, "no graphical session to open a terminal in"),
            ConsoleError::NoTerminal => write!(f, "no terminal emulator found"),
            ConsoleError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for ConsoleError {
    fn from(e: io::Error) -> Self {
        ConsoleError::Io(e)
    }
}

/// Run `exe args…` in a terminal window of the user's choosing.
///
/// `title` names the window; `done_hint` is printed after the command exits so a
/// window that stays open says something useful. `env` is exported inside the
/// script rather than passed through the launcher, because the launcher is a
/// desktop service (`open`, `x-terminal-emulator`) whose environment does not
/// reliably reach the window. Returns the launcher that was used, for the log.
pub fn launch(
    exe: &Path,
    args: &[String],
    env: &[(&str, &str)],
    title: &str,
    done_hint: &str,
) -> Result<String, ConsoleError> {
    let script = write_script(exe, args, env, title, done_hint)?;
    match std::env::consts::OS {
        "macos" => launch_macos(&script),
        "linux" => launch_linux(&script),
        _ => Err(ConsoleError::Unsupported),
    }
}

/// Where the generated script lives.
///
/// A stable path rather than a temp file, and owner-only: it is regenerated on
/// every launch, it is one more thing `veld uninstall` already removes with
/// `~/.veld`, and a fixed name means a user who finds a stray window can see what
/// produced it. The extension matters on macOS — LaunchServices routes `.command`
/// to a terminal and nothing else does.
fn script_path() -> Option<PathBuf> {
    let name = if std::env::consts::OS == "macos" {
        "update-console.command"
    } else {
        "update-console.sh"
    };
    Some(dirs::home_dir()?.join(".veld").join(name))
}

fn write_script(
    exe: &Path,
    args: &[String],
    env: &[(&str, &str)],
    title: &str,
    done_hint: &str,
) -> Result<PathBuf, ConsoleError> {
    let path = script_path().ok_or(ConsoleError::Unsupported)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut argv = String::new();
    argv.push_str(&quote(&exe.to_string_lossy()));
    for arg in args {
        argv.push(' ');
        argv.push_str(&quote(arg));
    }

    let mut exports = String::new();
    for (key, value) in env {
        // The name is a compile-time constant at every call site; only the value
        // can carry surprises, and it is quoted.
        exports.push_str(&format!("export {key}={}\n", quote(value)));
    }

    // `exec` is deliberately absent: the exit status is needed after the command
    // finishes, to decide whether to hold the window open.
    let body = format!(
        r#"#!/bin/bash
# Generated by veld. Regenerated on every update; safe to delete.
printf '\033]0;%s\007' {title}
{exports}{argv}
status=$?
echo
if [ "$status" -eq 0 ]; then
  echo {done_hint}
else
  echo "veld update exited with status $status."
  # Only on failure, and bounded. A window that closes takes the diagnosis with
  # it — but a window that waits forever for a user who has walked away is a
  # process sitting on nothing, so the prompt times out and closes on its own.
  read -r -t 300 -p "Press Return to close this window. " _ || true
fi
exit "$status"
"#,
        title = quote(title),
        exports = exports,
        argv = argv,
        done_hint = quote(done_hint),
    );

    // Owner-only *and* executable: it names paths from this user's machine and is
    // exec'd by a launcher, so 0700 rather than the 0600 everything else in
    // `~/.veld` gets.
    //
    // The mode is set **at create time**, not with a `set_permissions` after a
    // plain `fs::write`. That sequence lands the file at the umask default —
    // 0644 on a stock macOS account, where every local user is in `staff` — and
    // leaves it world-readable for the window between the two calls. Small, and
    // the contents are only a path and an argv, but there is no reason to have a
    // window at all when `OpenOptions` closes it.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o700)
            .open(&path)?;
        file.write_all(body.as_bytes())?;
        // `mode` applies only when the file is *created*, so a leftover from an
        // earlier release (or an earlier umask) keeps its old permissions
        // without this.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, body)?;
    Ok(path)
}

/// LaunchServices first, Terminal.app as the floor.
///
/// `open <file>` honours whatever the user registered for `.command` — iTerm,
/// Ghostty, WezTerm and friends all register — which is as close as macOS gets to
/// "the default terminal". `-a Terminal` is the fallback for a machine whose
/// association is missing or broken, and Terminal.app is guaranteed present.
fn launch_macos(script: &Path) -> Result<String, ConsoleError> {
    if run(
        &PathBuf::from("/usr/bin/open"),
        &[script.to_string_lossy().to_string()],
    )? {
        return Ok("the default terminal".to_string());
    }
    if run(
        &PathBuf::from("/usr/bin/open"),
        &[
            "-a".to_string(),
            "Terminal".to_string(),
            script.to_string_lossy().to_string(),
        ],
    )? {
        return Ok("Terminal".to_string());
    }
    Err(ConsoleError::NoTerminal)
}

/// `open`-style launchers exit immediately; their status is the only signal
/// available about whether a handler was found at all.
fn run(bin: &Path, args: &[String]) -> Result<bool, ConsoleError> {
    let status = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) => Ok(s.success()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(ConsoleError::Io(e)),
    }
}

/// How a given emulator wants to be told "run this program".
///
/// Split out and public to tests because the conventions genuinely differ and
/// getting one wrong opens a window running the user's shell instead of the
/// update — which looks like success and updates nothing.
pub fn emulator_args(bin: &str, script: &str) -> Vec<String> {
    let name = Path::new(bin)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| bin.to_string());
    match name.as_str() {
        // GNOME's family took `--` when `-e` was deprecated; `-e` on a modern
        // gnome-terminal is either ignored or re-parsed as a single string.
        "gnome-terminal" | "ptyxis" | "kgx" => vec!["--".to_string(), script.to_string()],
        // These take the program as a bare positional.
        "kitty" | "foot" => vec![script.to_string()],
        "wezterm" => vec!["start".to_string(), "--".to_string(), script.to_string()],
        // `-e` is the Debian `x-terminal-emulator` interface and what everything
        // else here still accepts.
        _ => vec!["-e".to_string(), script.to_string()],
    }
}

/// The user's terminal first, then the distribution's, then the field.
///
/// `x-terminal-emulator` sits third on purpose: it *is* the Debian answer to
/// "the system default", but it is a symlink an admin may have pointed at
/// something odd, so an explicit `$TERMINAL` outranks it.
fn linux_candidates() -> Vec<String> {
    let mut out = Vec::new();
    for var in ["VELD_TERMINAL", "TERMINAL"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                out.push(v.trim().to_string());
            }
        }
    }
    out.extend(
        [
            "x-terminal-emulator",
            "gnome-terminal",
            "konsole",
            "ptyxis",
            "kgx",
            "xfce4-terminal",
            "mate-terminal",
            "tilix",
            "alacritty",
            "kitty",
            "wezterm",
            "foot",
            "urxvt",
            "xterm",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    out
}

fn launch_linux(script: &Path) -> Result<String, ConsoleError> {
    // No session, no window. Worth failing on explicitly rather than letting
    // fourteen emulators each fail to connect to a display: this is the normal
    // state of a container or an SSH login, and the caller's fallback is correct
    // there.
    let headless = std::env::var("DISPLAY")
        .map(|v| v.is_empty())
        .unwrap_or(true)
        && std::env::var("WAYLAND_DISPLAY")
            .map(|v| v.is_empty())
            .unwrap_or(true);
    if headless {
        return Err(ConsoleError::NoDisplay);
    }

    let script = script.to_string_lossy().to_string();
    for bin in linux_candidates() {
        // Unlike macOS's `open`, an emulator stays alive for as long as its
        // window does — so this must not wait on it. `spawn` reporting `Ok` does
        // mean `execvp` succeeded (Rust surfaces exec failure through its own
        // pipe), which is the "is it installed" question being asked here.
        match Command::new(&bin)
            .args(emulator_args(&bin, &script))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => return Ok(bin),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => continue,
        }
    }
    Err(ConsoleError::NoTerminal)
}

/// Wrap a string so `bash` sees exactly these bytes.
///
/// Single quotes disable every expansion there is, which is the point: a path or
/// a version string reaching a generated script must never be able to become a
/// second command. The `'\''` dance is the only escape single quoting needs.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// `script_path` resolves through `HOME`, which is process-wide.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_home(dir: &Path) -> MutexGuard<'static, ()> {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("HOME", dir) };
        guard
    }

    /// Run the generated script the way a terminal window would, and report what
    /// it printed and what it exited with.
    fn run_script(script: &Path) -> (String, i32) {
        let out = Command::new("/bin/bash")
            .arg(script)
            .output()
            .expect("bash must run the generated script");
        (
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            out.status.code().unwrap_or(-1),
        )
    }

    /// The whole point of the console route is that a real terminal really runs
    /// the real command. Everything else here tests a string; this tests the
    /// artifact, by executing it.
    #[test]
    fn the_generated_script_runs_the_command_and_propagates_its_status() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = with_home(tmp.path());

        let script = write_script(
            Path::new("/bin/echo"),
            &[
                "update".to_string(),
                "--target-version".to_string(),
                "16.12.0".to_string(),
            ],
            &[("VELD_UPDATE_ORIGIN", "console")],
            "Updating veld",
            "Update finished. You can close this window.",
        )
        .unwrap();

        let (output, status) = run_script(&script);
        assert_eq!(status, 0);
        // The argv arrived as three separate arguments, not as one string and
        // not re-split — `echo` prints exactly what it was given.
        assert!(
            output.contains("update --target-version 16.12.0"),
            "argv did not survive: {output}"
        );
        assert!(output.contains("Update finished."), "{output}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&script).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "the script names local paths");
        }
    }

    #[test]
    fn a_failing_command_keeps_its_exit_status() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = with_home(tmp.path());

        // `false` exits 1. The window's own exit status has to be the update's,
        // or the app-side handshake and any script wrapping this would read a
        // failed update as a successful one. The `read` on the failure branch has
        // no tty here, so it returns immediately rather than waiting out its 300s.
        let script = write_script(
            Path::new("/usr/bin/false"),
            &[],
            &[],
            "Updating veld",
            "done",
        )
        .unwrap();
        let (_, status) = run_script(&script);
        assert_eq!(status, 1);
    }

    #[test]
    fn an_argument_that_looks_like_shell_stays_an_argument() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = with_home(tmp.path());

        // A version string can only come from GitHub or from `--target-version`,
        // but the script is generated from values that crossed a process
        // boundary, so this is the property to hold rather than to argue about.
        let marker = tmp.path().join("pwned");
        let script = write_script(
            Path::new("/bin/echo"),
            &[format!("'; touch {} ; echo '", marker.display())],
            &[("VELD_UPDATE_ORIGIN", "$(touch /tmp/veld-console-test-env)")],
            "Updating veld",
            "done",
        )
        .unwrap();

        let (output, status) = run_script(&script);
        assert_eq!(status, 0);
        assert!(!marker.exists(), "an argument executed as a command");
        assert!(!Path::new("/tmp/veld-console-test-env").exists());
        assert!(
            output.contains("touch"),
            "the argument was still passed: {output}"
        );
    }

    #[test]
    fn quoting_neutralises_a_shell_metacharacter() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("with space"), "'with space'");
        assert_eq!(quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        // The one that matters: a closing quote in the input must not be able to
        // end the quoting and start a command.
        assert_eq!(quote("'; rm -rf ~; '"), r"''\''; rm -rf ~; '\'''");
    }

    #[test]
    fn gnome_family_gets_the_double_dash_not_dash_e() {
        // `-e` on a modern gnome-terminal opens a shell and runs nothing, which
        // reads as a successful launch and updates nothing at all.
        assert_eq!(
            emulator_args("gnome-terminal", "/s.sh"),
            vec!["--".to_string(), "/s.sh".to_string()]
        );
        assert_eq!(
            emulator_args("/usr/bin/kgx", "/s.sh"),
            vec!["--".to_string(), "/s.sh".to_string()]
        );
    }

    #[test]
    fn positional_and_subcommand_emulators_are_not_given_dash_e() {
        assert_eq!(emulator_args("kitty", "/s.sh"), vec!["/s.sh".to_string()]);
        assert_eq!(emulator_args("foot", "/s.sh"), vec!["/s.sh".to_string()]);
        assert_eq!(
            emulator_args("wezterm", "/s.sh"),
            vec!["start".to_string(), "--".to_string(), "/s.sh".to_string()]
        );
    }

    #[test]
    fn everything_else_takes_the_debian_dash_e_interface() {
        for bin in ["x-terminal-emulator", "xterm", "konsole", "alacritty"] {
            assert_eq!(
                emulator_args(bin, "/s.sh"),
                vec!["-e".to_string(), "/s.sh".to_string()],
                "{bin}"
            );
        }
    }

    #[test]
    fn an_explicit_terminal_outranks_the_distribution_default() {
        // Not asserted against the live environment — just the ordering rule,
        // which is what a reader of `linux_candidates` needs pinned.
        let all = linux_candidates();
        let xte = all.iter().position(|b| b == "x-terminal-emulator");
        let xterm = all.iter().position(|b| b == "xterm");
        assert!(xte < xterm, "the distribution default outranks the zoo");
    }
}
