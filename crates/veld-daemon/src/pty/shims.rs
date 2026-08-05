//! The environment a terminal session is handed so that a process inside it can
//! open a URL *in Veld*.
//!
//! One small directory per daemon instance ([`veld_core::instance::shim_dir`])
//! holding three generated `sh` scripts, and four variables in the shell's
//! environment:
//!
//! | Variable | Why |
//! |---|---|
//! | `BROWSER` | The automatic half. It points at `veld-open`, and it is what Claude Code, `gh`, `git`, Python's `webbrowser`, vite and next all consult. |
//! | `VELD_PTY_SESSION` | Which terminal asked, which is how the daemon knows *which window* the pane belongs in. |
//! | `VELD_SHIM_DIR` | The opt-in half: a user who puts this on `PATH` also gets `open`/`xdg-open` routed. |
//! | `VELD_BROWSER_ORIGINAL` | Whatever `$BROWSER` was before veld took it over, so the fall-through path can restore it instead of handing a child the shim again. |
//!
//! # `BROWSER` is the automatic mechanism because `PATH` cannot be
//!
//! Measured on macOS: `/etc/zprofile` runs `path_helper`, which rebuilds `PATH`
//! with the system directories first and appends the previous contents, so a
//! directory prepended before spawning `$SHELL -l` lands *behind* `/usr/bin` and
//! `open` still resolves to `/usr/bin/open`. Debian's `/etc/profile` overwrites
//! `PATH` outright. The shell we spawn is a login shell by design (it is how a
//! terminal gets the user's real environment), so it is entitled to do this — and
//! veld does not wrap anybody's rc files to win the argument. Environment variables
//! survive; `PATH` order does not.
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

/// The variables a terminal session gets, given its session id.
///
/// Empty except for `VELD_PTY_SESSION` when the shim directory could not be
/// prepared — the session id alone is still worth having, because `veld open-url`
/// run by hand (or by an agent) uses it.
pub fn session_env(session_id: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("VELD_PTY_SESSION".to_owned(), session_id.to_owned());
    let Some(dir) = dir() else {
        return env;
    };
    env.insert("VELD_SHIM_DIR".to_owned(), dir.display().to_string());
    // Saved before it is replaced, so the fall-through in `veld open-url` can give
    // a child the browser the user actually configured. Without this, a tool that
    // reads `$BROWSER` on the far side of a passthrough would be handed the shim
    // again — which is a loop, not a fallback.
    if let Some(previous) = std::env::var_os("BROWSER")
        .and_then(|v| v.into_string().ok())
        .filter(|v| !v.is_empty())
    {
        env.insert("VELD_BROWSER_ORIGINAL".to_owned(), previous);
    }
    env.insert(
        "BROWSER".to_owned(),
        dir.join(Tool::Browser.shim_name()).display().to_string(),
    );
    env
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
    for tool in [Tool::Browser, Tool::Open, Tool::XdgOpen] {
        let path = dir.join(tool.shim_name());
        let body = script(tool, cli, dir);
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
fn script(tool: Tool, cli: &Path, shim_dir: &Path) -> String {
    let mut out = String::new();
    out.push_str("#!/bin/sh\n");
    out.push_str("# Generated by veld — rewritten on every daemon start; edits are lost.\n");
    out.push_str(
        "# Routes a single http(s) URL to a Veld browser pane. See `veld open-url --help`.\n",
    );
    out.push_str(&format!(
        "[ -x {cli} ] && exec {cli} open-url --tool {flag} -- \"$@\"\n",
        cli = quote(cli),
        flag = tool.flag(),
    ));
    match veld_core::opener::real_opener(tool, Some(shim_dir)) {
        Some(real) => out.push_str(&format!("exec {} \"$@\"\n", quote(&real))),
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
        let dir = PathBuf::from("/home/dev/.veld/shim-19899");
        let body = script(Tool::Open, Path::new("/usr/local/bin/veld"), &dir);
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[0], "#!/bin/sh");
        // The guard is `-x`, evaluated when the shim runs: an uninstall while a
        // shell is open must not break `open`.
        assert!(
            lines[3].starts_with("[ -x '/usr/local/bin/veld' ] && exec '/usr/local/bin/veld' open-url --tool open -- \"$@\""),
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
    fn the_shims_are_executable_and_rewritten_in_place() {
        let dir = tempfile::TempDir::new().unwrap();
        let shims = dir.path().join("shim-19899");
        prepare_in(&shims, Path::new("/opt/veld/bin/veld")).unwrap();

        for name in ["veld-open", "open", "xdg-open"] {
            let path = shims.join(name);
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(body.contains("/opt/veld/bin/veld"), "{name}: {body}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o755, "{name} must be executable");
            }
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
        let body = std::fs::read_to_string(shims.join("open")).unwrap();
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
    fn the_session_id_is_always_exported() {
        // Even when no shim directory could be prepared: `veld open-url` run by
        // hand still needs to know which terminal it is in.
        let env = session_env("abc-123");
        assert_eq!(
            env.get("VELD_PTY_SESSION").map(String::as_str),
            Some("abc-123")
        );
    }
}
