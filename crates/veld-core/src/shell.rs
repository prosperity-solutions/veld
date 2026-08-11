//! Which shell veld opens for a user.
//!
//! One answer, computed here, so the four places that need it cannot disagree:
//! a terminal pane's login shell, a config-declared pane's `-l -i -c` wrapper,
//! the startup-handoff decision in `pty::shims` (`ZDOTDIR` for zsh, posix-mode
//! `$ENV` for bash, so it has to know which family the shell about to run belongs
//! to), and the login shell [`crate::user_path`] spawns to learn the user's `PATH`.
//!
//! # Why this is a preference at all
//!
//! `$SHELL` is the shell the system logs the user in with, and for most people it
//! is the shell they work in. For a sizeable minority it is not: macOS has shipped
//! zsh as the login shell since Catalina, and someone who moved from bash but kept
//! ten years of aliases, completions and tool integrations in `~/.bashrc` gets a
//! terminal that loads none of it. `chsh` is the system-wide fix and is a bigger
//! hammer than "the terminals inside this one app" — so veld carries the choice.
//!
//! # What "auto" does, and what it deliberately does not
//!
//! [`auto_shell`] is `$SHELL`, then the user's `passwd` entry, then `/bin/sh`. The
//! `passwd` step is not redundant: the daemon runs under launchd or systemd, whose
//! environments are not a login session's, so `$SHELL` can simply be absent there
//! — and the previous fallback (`/bin/sh`) is a shell that reads none of anyone's
//! rc files.
//!
//! It does **not** guess from rc files. "`~/.bashrc` exists, so you must want
//! bash" reads as clever and is wrong on most machines: a stale `~/.bashrc` from a
//! decade ago sits in nearly every macOS home directory, and acting on it would
//! switch a contented zsh user's terminals to bash with nothing said. Guessing
//! against a shell the user declared is worse than the bug this module exists to
//! fix. The list [`discover`] returns is offered, never applied.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

/// The stored value meaning "work it out" — see [`auto_shell`]. Also the default,
/// so a user who never opens the setting gets exactly the previous behaviour plus
/// the `passwd` fallback.
pub const AUTO: &str = "auto";

/// Longest accepted shell path. `PATH_MAX` is 1024 on macOS and 4096 on Linux;
/// this is a bound against a settings row holding a document, not a filesystem
/// limit to enforce.
const MAX_SHELL_PATH_LEN: usize = 1024;

/// Shell names worth probing for on `PATH`, beyond whatever `/etc/shells` lists.
///
/// `/etc/shells` is the canonical register of login shells and is what `chsh`
/// consults, but a Homebrew or Nix install is only added to it if the user ran the
/// documented `echo … | sudo tee -a /etc/shells` step — which the shell they
/// actually use every day may well have skipped. So the register is a source, not
/// the source.
///
/// `sh` is deliberately absent: it is on every machine, it is not a shell anyone
/// chooses to work in, and on Linux it is usually a symlink to `dash` or to `bash`
/// running in POSIX mode (which reads *neither* `~/.bashrc` nor `~/.zshrc`) — so
/// offering it in a picker whose whole purpose is "load my rc files" would offer
/// the one entry that guarantees none load. It remains reachable as a custom path
/// and as the last-resort fallback.
const PROBED_SHELLS: &[&str] = &[
    "bash", "zsh", "fish", "nu", "ksh", "tcsh", "csh", "dash", "elvish", "xonsh",
];

/// A shell offered in the picker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Discovered {
    /// Absolute path, exactly as it will be stored and spawned.
    pub path: String,
    /// The basename, for the label. Kept separate so the UI never has to parse a
    /// path — and so the two spellings of one binary (`/bin/sh` and `/bin/bash`
    /// where `sh` *is* bash) stay distinguishable, which they must: bash invoked
    /// as `sh` runs in POSIX mode and reads none of the user's bash startup.
    pub name: String,
}

/// The shell to spawn, given the stored preference.
///
/// `None` or [`AUTO`] means [`auto_shell`]. Anything else is used as-is **if it is
/// still executable**, and falls back to [`auto_shell`] with a warning if it is
/// not: the alternative is a terminal that cannot open at all because a shell was
/// uninstalled, in an app whose settings surface is reached through that same UI.
/// A preference is not deleted by a failed resolution — the machine may get its
/// shell back, and the value is the user's, not ours.
#[must_use]
pub fn resolve(preference: Option<&str>) -> String {
    let Some(path) = preference.map(str::trim).filter(|p| !p.is_empty()) else {
        return auto_shell();
    };
    if path == AUTO {
        return auto_shell();
    }
    if is_executable(Path::new(path)) {
        return path.to_owned();
    }
    let fallback = auto_shell();
    warn!(
        preferred = %path,
        using = %fallback,
        "the configured terminal shell is not an executable file — falling back"
    );
    fallback
}

/// `$SHELL`, then the `passwd` entry, then `/bin/sh`. Never empty.
///
/// The `passwd` step matters where this is called from: a daemon under launchd or
/// systemd has a service environment, not a login session's, so `$SHELL` may be
/// absent — and `/bin/sh` alone would hand that user a shell that reads none of
/// their startup files.
///
/// Each candidate is checked for executability, so `/bin/sh` is a real last resort
/// rather than a nominal one. Without that, the guarantee [`resolve`] documents —
/// a shell that was uninstalled can never leave a user unable to open the terminal
/// they would fix the setting from — held only for an explicitly configured path
/// and not for `auto`, which is what almost everyone is on: someone who `chsh`'d to
/// a Homebrew shell and later removed it has a stale `$SHELL` and a stale `passwd`
/// entry, and every terminal would name a program that is not there.
#[must_use]
pub fn auto_shell() -> String {
    let from_env = std::env::var("SHELL")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    for candidate in [from_env, passwd_shell()].into_iter().flatten() {
        if is_executable(Path::new(&candidate)) {
            return candidate;
        }
        warn!(
            shell = %candidate,
            "the login shell on record is not an executable file — trying the next source"
        );
    }
    "/bin/sh".to_owned()
}

/// The login shell recorded for this uid in the user database.
///
/// `getpwuid` rather than reading `/etc/passwd`, because on macOS the answer lives
/// in Open Directory and on a corporate Linux box in LDAP or SSSD — neither of
/// which appears in that file.
#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current()).ok()??;
    let shell = user.shell.to_str()?.trim().to_owned();
    (!shell.is_empty()).then_some(shell)
}

#[cfg(not(unix))]
fn passwd_shell() -> Option<String> {
    None
}

/// Whether a stored preference is *shaped* like one — checked on write.
///
/// Shape only, never existence: a settings write must not fail because a shell is
/// mid-install or because the value was typed before the binary landed, and
/// [`resolve`] already degrades safely at spawn time. Absolute because a bare name
/// would be resolved against whatever `PATH` the spawning process happened to have
/// — the daemon's bare service `PATH` — which is a different shell from the one
/// the same name finds in the user's terminal.
#[must_use]
pub fn is_valid_preference(value: &str) -> bool {
    let value = value.trim();
    if value == AUTO {
        return true;
    }
    value.starts_with('/')
        && value.len() <= MAX_SHELL_PATH_LEN
        && !value.contains(['\0', '\n', '\r'])
}

/// Every shell this machine appears to have, for the picker.
///
/// Two sources, unioned: `/etc/shells` and a probe of [`PROBED_SHELLS`] across
/// `path`. Entries that are not executable files are dropped — a stale
/// `/etc/shells` line for a shell that was uninstalled is common, and offering it
/// would let someone pick a shell whose only outcome is the fallback.
///
/// **`path` is the *user's* `PATH`, passed in, never `std::env::var("PATH")`.** A
/// daemon under launchd has a bare service `PATH` (measured: `launchctl getenv
/// PATH` is empty on macOS), and `/etc/shells` lists only what the OS shipped —
/// Homebrew's install notes tell you to append to it and nobody does. Read from the
/// process environment, this offered `/bin/bash` (3.2, which cannot take the `$ENV`
/// handoff) and never `/opt/homebrew/bin/bash` (5.x, which can): a picker whose
/// whole purpose is "use my bash" listing every bash except the working one. This
/// is the AGENTS.md daemon-`PATH` convention, and it applies to a directory scan as
/// much as to a spawn.
///
/// Deduplicated by **(canonical path, basename)** rather than by canonical path
/// alone. On Linux `/bin/bash` and `/usr/bin/bash` are one file through the
/// `/bin → usr/bin` symlink and collapsing them is right; `/bin/sh` and
/// `/bin/bash` may also be one file, and collapsing *those* would be wrong,
/// because a shell's `argv[0]` decides which startup files it reads.
#[must_use]
pub fn discover(path: &str) -> Vec<Discovered> {
    let mut out: Vec<Discovered> = Vec::new();
    let mut seen: Vec<(PathBuf, String)> = Vec::new();

    let mut consider = |path: PathBuf| {
        if !is_executable(&path) {
            return;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            return;
        };
        // `sh` is filtered here and not only left out of `PROBED_SHELLS`, because
        // `/etc/shells` lists it on every machine. See that constant for why the
        // one entry that reads nobody's rc files has no place in this picker.
        if name == "sh" {
            return;
        }
        let Some(text) = path.to_str().map(str::to_owned) else {
            return;
        };
        // Canonical, so two spellings of one binary are one entry — but paired
        // with the basename, so two *names* for one binary stay two entries.
        let key = (
            path.canonicalize().unwrap_or_else(|_| path.clone()),
            name.clone(),
        );
        if seen.contains(&key) {
            return;
        }
        seen.push(key);
        out.push(Discovered { path: text, name });
    };

    for line in std::fs::read_to_string("/etc/shells")
        .unwrap_or_default()
        .lines()
    {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('/') {
            consider(PathBuf::from(line));
        }
    }
    {
        for dir in std::env::split_paths(path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            for name in PROBED_SHELLS {
                consider(dir.join(name));
            }
        }
    }

    // By name first, so the list reads as a list of shells rather than of
    // directories, and two installs of one shell sit together.
    out.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
    out
}

/// Which shell family a path names, for the two mechanisms that are shell-specific.
///
/// By basename, and that is not a shortcut: a shell's `argv[0]` is what decides
/// which startup files it reads, so `/bin/sh` is not bash even when it *is* the
/// bash binary — it reads none of the user's bash startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zsh,
    Bash,
    /// fish, nushell, ksh, or anything else. veld has no startup hook for these;
    /// [`crate::shell`]'s callers fall back to `$BROWSER` plus the documented
    /// one-line `PATH` opt-in.
    Other,
}

/// Classify a shell path. One function, so `pty::shims` and the settings surface
/// cannot disagree about what "is bash" means.
#[must_use]
pub fn kind(shell: &str) -> Kind {
    match Path::new(shell).file_name().and_then(|n| n.to_str()) {
        Some("zsh") => Kind::Zsh,
        Some("bash") => Kind::Bash,
        _ => Kind::Other,
    }
}

/// How long a shell probe may take. These spawns read **no** rc files (`-c` with
/// an empty command, or `--posix` with only our probe file), so they are ~10ms in
/// practice; this bound exists for a wedged binary, not for a slow `.bashrc`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a probe that *could not run* is remembered before it is tried again.
///
/// Not a cache of an answer — a bound on how often a broken probe may cost
/// [`PROBE_TIMEOUT`] on the ticket-mint path. See [`supports_posix_env_handoff`].
const PROBE_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Whether this bash honours the `--posix` + `$ENV` startup handoff.
///
/// **Probed, never inferred from a version number.** bash's manual says an
/// interactive shell in posix mode reads `$ENV` and *no other startup file*, which
/// is the seam `pty::shims` uses to run one line after the user's own startup. It
/// is true from bash 4 onward and **false on bash 3.2**, which macOS still ships
/// as `/bin/bash` — there `--posix` is accepted, `$ENV` is ignored, and the only
/// effect is a session stuck in posix mode for nothing. A version table would also
/// have to be right about every distro's patched build, so this asks the binary
/// instead: it is the same "probe the capability, don't guess it" rule the shims
/// already follow for `veld open-url --help`.
///
/// Cached per path for the life of the process, because the answer changes only
/// when the binary does. A shell upgraded under a running daemon keeps the old
/// answer until the daemon restarts — which is the wrong answer for at most one
/// daemon lifetime, in the direction of doing nothing rather than doing something
/// broken.
pub async fn supports_posix_env_handoff(shell: &str) -> bool {
    // `None` is "the probe could not run", remembered with the time it failed —
    // neither cached forever nor retried on every call. See below.
    type Probed = HashMap<String, (Option<bool>, Instant)>;
    static CACHE: OnceLock<Mutex<Probed>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((known, at)) = cache.lock().ok().and_then(|c| c.get(shell).copied()) {
        match known {
            Some(answer) => return answer,
            // A remembered failure, still fresh: answer "no handoff" without
            // paying for the probe again.
            None if at.elapsed() < PROBE_RETRY_AFTER => return false,
            None => {}
        }
    }
    let answer = probe_posix_env_handoff(shell).await;
    match answer {
        Some(answer) => debug!(shell, answer, "probed the bash posix/ENV startup handoff"),
        // The probe did not run — a spawn failure, or the timeout. That is not
        // evidence about the shell, and memoizing it as a definitive `false` would
        // switch the handoff off for the daemon's whole lifetime on one transient
        // hiccup, while `veld doctor` (a separate process, probing afresh) reported
        // the opposite with no way to tell which was right. Same rule
        // `user_path::publish_value` states: a resolution that learned nothing never
        // displaces one that did.
        //
        // **But not retried on every call either.** This is awaited inline on the
        // ticket-mint path, once per new terminal, so a persistently wedged bash (a
        // full disk failing the tempdir, a spawn that never returns) would otherwise
        // add up to `PROBE_TIMEOUT` to *every* terminal open, for ever. Remembering
        // the failure for `PROBE_RETRY_AFTER` bounds that at one slow open a minute
        // while still letting a transient failure heal.
        None => debug!(
            shell,
            "the bash posix/ENV probe could not run — retrying later"
        ),
    }
    if let Ok(mut c) = cache.lock() {
        c.insert(shell.to_owned(), (answer, Instant::now()));
    }
    answer.unwrap_or(false)
}

/// The uncached probe: write a marker file, point `$ENV` at it, and see whether
/// the shell printed the marker.
///
/// `-c ':'` rather than an interactive session with a pty — posix mode reads
/// `$ENV` for an interactive shell, and `-i` is what makes it one, so this needs
/// no terminal. Nothing of the user's is read on this path: in posix mode `$ENV`
/// is the *only* startup file, and on a shell that ignores `$ENV` the `-c` form
/// reads none either.
/// `None` means the question could not be asked — distinct from `Some(false)`,
/// "this shell does not honour it", which is the only answer worth caching.
async fn probe_posix_env_handoff(shell: &str) -> Option<bool> {
    if kind(shell) != Kind::Bash || !is_executable(Path::new(shell)) {
        // A definitive no: not a bash, so there is nothing to honour.
        return Some(false);
    }
    const MARKER: &str = "VELD_POSIX_ENV_OK";
    let Ok(dir) = tempfile::TempDir::new() else {
        return None;
    };
    let probe = dir.path().join("probe.sh");
    if std::fs::write(&probe, format!("printf {MARKER}\n")).is_err() {
        return None;
    }
    let mut cmd = tokio::process::Command::new(shell);
    // **The long option must come first.** `bash -l -i --posix` is a usage error —
    // bash parses GNU long options only ahead of the short ones — and it exits
    // printing its usage, which would make every probe (and every terminal) fail
    // for a reason nothing here would explain.
    cmd.arg("--posix")
        .arg("-i")
        .arg("-c")
        .arg(":")
        .env("ENV", &probe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // Same reason `user_path::login_shell_path` does it: an interactive shell can
    // open /dev/tty and seize the foreground process group of whatever terminal
    // this process happens to have, then exit without restoring it.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            let _ = nix::unistd::setsid();
            Ok(())
        });
    }
    match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => Some(String::from_utf8_lossy(&out.stdout).contains(MARKER)),
        // A timeout or a spawn error: the shell never answered, so there is no
        // answer to remember.
        _ => None,
    }
}

/// What `open` resolves to inside a real session of this shell — the verifier.
///
/// Answers the question no amount of reasoning about startup files can: **did the
/// shim actually win on this machine?** Spawned exactly as a terminal is
/// (`-l -i`), with the session environment the daemon would hand it, so a
/// `.zshrc` that clears `precmd_functions`, a `.bashrc` that rebuilds `PATH` after
/// us, or a shell veld has no hook for all show up as the real answer instead of
/// silently doing nothing.
///
/// `None` when the shell could not be asked (a stall, a spawn failure, a shell
/// whose syntax this question is not valid in). That is deliberately different from
/// `Some(path)`: "we do not know" must not be reported to a user as "it is broken".
///
/// # Driven through **stdin**, never `-c`, and that is the whole correctness of it
///
/// zsh's half of the feature is a `precmd` hook, which fires *before a prompt*.
/// `zsh -l -i -c '<command>'` prints no prompt, so the hook never runs — the probe
/// then reports `/usr/bin/open` for a machine where a real terminal resolves the
/// shim perfectly well, i.e. it invents a fault and tells the user to edit their
/// `.zshrc` to fix nothing. Writing to stdin makes it print a prompt first, which
/// is exactly why `the_generated_zshenv_wins_against_an_rc_that_rebuilds_path`
/// drives zsh the same way; that test's comment is where this was already written
/// down, and the first version of this function ignored it.
///
/// The answer is wrapped in sentinels because a prompt, an rc file's greeting and a
/// title escape sequence all land on stdout beside it.
pub async fn resolved_open(
    shell: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    use tokio::io::AsyncWriteExt;

    if !is_executable(Path::new(shell)) {
        return None;
    }
    const OPEN: &str = "@@VELD-OPEN@@";
    let mut cmd = tokio::process::Command::new(shell);
    for flag in interactive_flags(shell, env) {
        cmd.arg(flag);
    }
    // `-l -i`, the exact shape `spawn_shell` and `resolve_pane` use, because a
    // login shell is where `path_helper` and `/etc/profile` get their say — the
    // two things the shim has to survive.
    cmd.arg("-l").arg("-i");
    for (key, value) in env {
        cmd.env(key, value);
    }
    // **The same `TERM` a real session gets** (`holder.rs`'s `spawn_shell`), not
    // the `dumb` that would keep the output tidy. `[[ $TERM == dumb ]] && return`
    // at the top of an rc file is a near-universal guard (it is what Emacs TRAMP
    // and `scp` mode rely on), so probing with `dumb` skips the very lines this is
    // here to judge — and answers "the shim wins" for a machine whose terminals it
    // loses on. A probe that does not reproduce the session is not a probe.
    // The escape noise that buys is handled by [`strip_controls`].
    cmd.env("TERM", "xterm-256color");
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            let _ = nix::unistd::setsid();
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else {
        return None;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // `command -v` rather than `which`: a builtin, so it needs nothing on
        // `PATH` and reports a function or alias too — which is what a caller
        // typing `open` would actually reach. A shell in which this line is not
        // valid syntax (fish before 3.4, which has no `$(…)`) prints no sentinel
        // and is reported as "unknown" rather than as broken.
        let script = format!("printf '{OPEN}%s{OPEN}\\n' \"$(command -v open)\"\nexit\n");
        let _ = stdin.write_all(script.as_bytes()).await;
        let _ = stdin.flush().await;
        drop(stdin);
    }
    // A full login shell, so this one really can take the 10s a wedged rc file
    // costs — hence the longer bound than [`PROBE_TIMEOUT`].
    match tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let text = String::from_utf8_lossy(&out.stdout);
            // The *last* pair of sentinels: the shell echoes the line it was fed
            // before running it, so the literal `printf` command containing the
            // markers appears on stdout first.
            let answer = text
                .rsplit_once(OPEN)
                .and_then(|(before, _)| before.rsplit_once(OPEN).map(|(_, found)| found))?;
            let answer = strip_controls(answer);
            // Empty means `command -v` found nothing at all, which is a real
            // answer ("no `open` on this machine") and not a resolution.
            (!answer.is_empty()).then_some(answer)
        }
        _ => None,
    }
}

/// Drop terminal escape sequences and control characters, then trim.
///
/// The probe runs with a real `TERM` (see [`resolved_open`]), so a prompt theme can
/// emit CSI colour runs and OSC title/cwd sequences on the same stream as the
/// answer. The sentinels bound where the answer is; this decides what inside those
/// bounds is part of it. A path never legitimately contains one of these bytes.
fn strip_controls(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() {
                out.push(c);
            }
            continue;
        }
        // ESC. Skip to the sequence's terminator: a final byte in `@`..=`~` for
        // CSI/SS3, or BEL / ESC-\ for the string sequences (OSC, DCS, APC).
        match chars.next() {
            Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if c == '\u{7}' || (prev == '\u{1b}' && c == '\\') {
                        break;
                    }
                    prev = c;
                }
            }
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // A two-character escape; its second character was already consumed.
            _ => {}
        }
    }
    out.trim().to_owned()
}

/// The line that puts veld's shim directory on `PATH` by hand, and the file to put
/// it in — for a shell veld has no startup handoff for, or one where the handoff
/// lost.
///
/// The daemon exports `$VELD_SHIM_DIR` into **every** shell precisely so this is
/// one line rather than a path someone has to be told. Returned as (file, line) so
/// a caller can render it as a code block with somewhere to put it.
#[must_use]
pub fn path_hint(shell: &str) -> (&'static str, &'static str) {
    match kind(shell) {
        // Reached when the handoff lost — an rc file that rebuilds `PATH` after
        // veld's hook, or a `.zshrc` that clears `precmd_functions` outright.
        Kind::Zsh => (
            "~/.zshrc",
            r#"[ -n "$VELD_SHIM_DIR" ] && PATH="$VELD_SHIM_DIR:$PATH""#,
        ),
        // The common case on macOS, whose `/bin/bash` is 3.2 and ignores `$ENV`.
        // `.bashrc` rather than `.bash_profile`: veld spawns a login shell, so the
        // profile is what bash reads — but the near-universal convention is that
        // the profile sources `.bashrc`, and `.bashrc` is where a bash user's
        // aliases already live, which is the file they will actually open.
        Kind::Bash => (
            "~/.bashrc",
            r#"[ -n "$VELD_SHIM_DIR" ] && PATH="$VELD_SHIM_DIR:$PATH""#,
        ),
        // fish's own idiom. `fish_add_path -p` prepends and is idempotent, which is
        // what makes it safe in a file that runs on every shell.
        Kind::Other if shell.ends_with("fish") => (
            "~/.config/fish/config.fish",
            "if set -q VELD_SHIM_DIR; fish_add_path -p $VELD_SHIM_DIR; end",
        ),
        Kind::Other => (
            "your shell's startup file",
            r#"[ -n "$VELD_SHIM_DIR" ] && PATH="$VELD_SHIM_DIR:$PATH""#,
        ),
    }
}

/// The flags a login shell is spawned with, ahead of `-l`.
///
/// Only bash has any, and only when the daemon put an `ENV` handoff in its
/// environment — see `pty::shims`. Long options come **first**, which bash
/// enforces.
#[must_use]
pub fn interactive_flags(
    shell: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    if kind(shell) == Kind::Bash && env.contains_key("ENV") {
        return vec!["--posix".to_owned()];
    }
    Vec::new()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell that exists on every machine these tests run on, and is executable.
    fn a_real_shell() -> String {
        ["/bin/sh", "/bin/bash", "/bin/zsh"]
            .into_iter()
            .find(|p| is_executable(Path::new(p)))
            .expect("no shell on this machine")
            .to_owned()
    }

    #[test]
    fn a_missing_shell_falls_back_instead_of_making_terminals_unopenable() {
        // The failure this guards is circular: the only way to fix a bad value is
        // the settings UI, and with no fallback the app it lives in has no working
        // terminal to explain what went wrong.
        let resolved = resolve(Some("/nonexistent/shell/that/was/uninstalled"));
        assert_eq!(resolved, auto_shell());
        // A directory is not a shell either, and neither is a non-executable file.
        assert_eq!(resolve(Some("/tmp")), auto_shell());
    }

    #[test]
    fn an_explicit_executable_is_used_verbatim() {
        let shell = a_real_shell();
        assert_eq!(resolve(Some(&shell)), shell);
        // Whitespace from a hand-typed custom path does not make it a new shell.
        assert_eq!(resolve(Some(&format!("  {shell} "))), shell);
    }

    #[test]
    fn auto_and_absent_mean_the_same_thing() {
        assert_eq!(resolve(Some(AUTO)), auto_shell());
        assert_eq!(resolve(None), auto_shell());
        assert_eq!(resolve(Some("")), auto_shell());
        assert_eq!(resolve(Some("   ")), auto_shell());
    }

    #[test]
    fn auto_is_never_empty_and_is_a_path() {
        // `$SHELL` is set in a test process, so this asserts the common branch;
        // the `passwd` and `/bin/sh` branches are what keep it non-empty on a
        // daemon, and every branch returns an absolute path.
        let auto = auto_shell();
        assert!(!auto.is_empty());
        assert!(auto.starts_with('/'), "{auto}");
    }

    #[test]
    fn a_preference_is_validated_by_shape_only() {
        assert!(is_valid_preference(AUTO));
        assert!(is_valid_preference("/bin/bash"));
        // Existence is deliberately not checked — a value typed while a shell is
        // being installed must be storable, and `resolve` degrades safely.
        assert!(is_valid_preference("/opt/homebrew/bin/fish"));
        assert!(is_valid_preference("/nonexistent/shell"));

        // A bare name resolves against the spawning process's PATH, which for the
        // daemon is the bare service one — a different binary from the one the
        // same name finds in the user's terminal.
        assert!(!is_valid_preference("bash"));
        assert!(!is_valid_preference(""));
        assert!(!is_valid_preference("Automatic"));
        assert!(!is_valid_preference("/bin/bash\nrm -rf /"));
        assert!(!is_valid_preference("/bin/ba\0sh"));
        assert!(!is_valid_preference(&format!(
            "/{}",
            "a".repeat(MAX_SHELL_PATH_LEN)
        )));
    }

    /// The verifier sees a `PATH` entry that a `precmd` hook installs.
    ///
    /// The regression this pins is precise and was shipped: zsh's half of the
    /// shim feature is a `precmd` hook, which runs *before a prompt*, and
    /// `zsh -l -i -c '<command>'` prints no prompt. A `-c`-based verifier therefore
    /// reported `/usr/bin/open` on a machine whose terminals resolve the shim
    /// perfectly — inventing a fault and telling the user to edit `.zshrc` to fix
    /// nothing. Driving the shell through **stdin** is what makes it prompt.
    #[tokio::test]
    async fn the_verifier_sees_a_path_a_precmd_hook_installs() {
        let Some(zsh) = ["/bin/zsh", "/usr/bin/zsh", "/opt/homebrew/bin/zsh"]
            .into_iter()
            .find(|p| is_executable(Path::new(p)))
        else {
            assert!(
                std::env::var_os("CI").is_none(),
                "zsh is missing in CI — this test is the only thing pinning the \
                 verifier against the -c regression"
            );
            eprintln!("no zsh on this machine — skipping");
            return;
        };

        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let shim = tmp.path().join("shim");
        let zdir = tmp.path().join("zdotdir");
        for d in [&home, &shim, &zdir] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(shim.join("open"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(shim.join("open"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        // The shape of the real handoff, reduced to the part that matters: the
        // entry is added by a hook that only runs before a prompt, and an rc file
        // rebuilds `PATH` after the assignment would have happened.
        std::fs::write(
            zdir.join(".zshenv"),
            // The first line is the point of the fixture as much as the hook is:
            // `[[ $TERM == dumb ]] && return` at the top of an rc file is a
            // near-universal guard, so a probe that spawns with `TERM=dumb` — for
            // tidier output, say — skips everything below it and reports that the
            // shim wins on a machine where it loses. Reverting `resolved_open`'s
            // TERM must fail here rather than in a user's terminal.
            "[[ $TERM == dumb ]] && return\n\
             unset ZDOTDIR\n\
             PATH=/usr/bin:/bin\n\
             veld_shim_path() { case \":$PATH:\" in *\":$VELD_SHIM_DIR:\"*) ;; \
             *) PATH=\"$VELD_SHIM_DIR:$PATH\" ;; esac }\n\
             typeset -ga precmd_functions\n\
             precmd_functions+=(veld_shim_path)\n",
        )
        .unwrap();

        let env = std::collections::BTreeMap::from([
            ("HOME".to_owned(), home.display().to_string()),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("ZDOTDIR".to_owned(), zdir.display().to_string()),
            ("VELD_SHIM_DIR".to_owned(), shim.display().to_string()),
        ]);
        let resolved = resolved_open(zsh, &env).await;
        assert_eq!(
            resolved.as_deref(),
            Some(shim.join("open").display().to_string().as_str()),
            "the verifier must see what a real terminal sees, not what `-c` sees"
        );
    }

    #[test]
    fn discovery_returns_executables_only_and_no_duplicates() {
        // The test process's own PATH stands in for the user's here; the point of
        // the argument is that a *daemon* must not use its own.
        let found = discover(&std::env::var("PATH").unwrap_or_default());
        assert!(
            !found.is_empty(),
            "no shell discovered at all — /etc/shells and PATH both empty?"
        );
        for shell in &found {
            assert!(shell.path.starts_with('/'), "{shell:?}");
            assert!(is_executable(Path::new(&shell.path)), "{shell:?}");
            assert!(!shell.name.is_empty(), "{shell:?}");
            // Whatever is offered must survive a round trip through the store.
            assert!(is_valid_preference(&shell.path), "{shell:?}");
            assert_eq!(resolve(Some(&shell.path)), shell.path, "{shell:?}");
        }
        let mut paths: Vec<&String> = found.iter().map(|s| &s.path).collect();
        let before = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), before, "the same path was offered twice");
        // `sh` is not offered: on Linux it is usually bash or dash under another
        // name, and bash-as-sh reads none of the user's bash startup — the exact
        // thing this picker exists to load. (It stays reachable as a custom path.)
        assert!(
            !found.iter().any(|s| s.name == "sh"),
            "`sh` must not be offered: {found:?}"
        );
    }
}
