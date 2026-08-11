//! Turning "something in a terminal wants to open a URL" into an argv.
//!
//! Two callers, and they have to agree exactly:
//!
//! - The **daemon** generates the little scripts a terminal session gets in its
//!   environment (`veld-daemon/src/pty/shims.rs`), and bakes the real opener's
//!   path into each one as its own fallback.
//! - The **CLI** (`veld open-url`) decides whether an invocation is a plain
//!   "open this web page" — in which case Veld may route it to a browser pane —
//!   or something else entirely, which must reach the real tool unchanged.
//!
//! # Why the falling-through matters more than the routing
//!
//! `open` on macOS is not a browser launcher. It opens files, directories,
//! applications and `-a`/`-R`/`-t` forms that have nothing to do with the web, and
//! a wrapper that swallowed those would break a command people use dozens of times
//! a day for a feature about URLs. So the rule is deliberately narrow: **exactly
//! one argument, and it is an `http(s)` URL.** Everything else is
//! [`Decision::Passthrough`], and the caller `exec`s the real tool with the
//! original argv untouched.
//!
//! # Two mechanisms, because one is not enough
//!
//! `$BROWSER` covers the well-behaved majority — Claude Code's own login flow (it
//! spawns `$BROWSER <url>` directly), `gh`, `git`, Python's `webbrowser`, vite,
//! next. It cannot cover a program that calls the system opener itself, which is
//! exactly what an agent's shell tool does (`Bash(open "https://…")`), so the shims
//! also have to be reachable on `PATH`.
//!
//! Getting them there is the daemon's problem rather than this module's, and it is
//! not the obvious one: measured, not assumed, macOS `/etc/zprofile` runs
//! `path_helper`, which rebuilds `PATH` with the system directories **first** and
//! appends what was there before — so a directory prepended before spawning
//! `$SHELL -l` ends up behind `/usr/bin` and `open` still resolves to
//! `/usr/bin/open`. Debian's `/etc/profile` overwrites `PATH` outright (the same
//! asymmetry AGENTS.md records for daemon-spawned commands). The fix is a `precmd`
//! hook installed from a `.zshenv` veld owns, which runs after every rc file —
//! see `veld-daemon/src/pty/shims.rs`.

use std::path::{Path, PathBuf};

/// Which of the generated shims was invoked — or, for [`Tool::Browser`], that the
/// caller came through `$BROWSER`, whose sole convention is "a command that takes
/// a URL".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `$BROWSER`, and `veld open-url` invoked by hand. Every argument is expected
    /// to be a URL, because that is all this convention carries.
    Browser,
    /// macOS `open`, which is a general-purpose "open this thing" command.
    Open,
    /// `xdg-open`, the same on freedesktop systems.
    XdgOpen,
}

impl Tool {
    /// Every tool a shim is generated for.
    ///
    /// Exists so the generator and its tests iterate the *enum* rather than a
    /// hand-written array. `shim_name`, `flag` and `real_opener` are exhaustive
    /// matches, so a new variant forces those to be updated and looks safe — while the
    /// generation loop and its regression test both walked a literal array, so a fourth
    /// tool would compile, pass, and simply never get a shim written. Same reason
    /// `SettingKey::ALL` exists in `veld_core::db::settings`.
    pub const ALL: &'static [Tool] = &[Self::Browser, Self::Open, Self::XdgOpen];

    /// The name the shim is written under, which is also how the CLI is told which
    /// one ran.
    #[must_use]
    pub fn shim_name(self) -> &'static str {
        match self {
            Self::Browser => "veld-open",
            Self::Open => "open",
            Self::XdgOpen => "xdg-open",
        }
    }

    /// The value `--tool` takes.
    #[must_use]
    pub fn flag(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Open => "open",
            Self::XdgOpen => "xdg-open",
        }
    }

    #[must_use]
    pub fn parse(flag: &str) -> Option<Self> {
        match flag {
            "browser" => Some(Self::Browser),
            "open" => Some(Self::Open),
            "xdg-open" => Some(Self::XdgOpen),
            _ => None,
        }
    }
}

/// What to do with an invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// A single web page. Veld may route it — subject to the exempt list, which is
    /// the daemon's call, not this module's.
    Url(String),
    /// Not a plain web page: hand the original argv to the real tool.
    Passthrough,
}

/// Whether an invocation is a plain "open this web page".
///
/// Narrow on purpose — see the module docs. Note what is *not* accepted:
///
/// - more than one argument (`open a.pdf b.pdf`, `open -a Safari url`),
/// - anything beginning with `-` (a flag, which changes what `open` does),
/// - a scheme other than `http`/`https` (`vscode://`, `file://`, `slack://`): a
///   pane cannot show those, and handing them anywhere but the OS would break a
///   deep link.
///
/// A lone `--` is dropped first, because that is how the generated shims pass an
/// argument list that may begin with a dash.
#[must_use]
pub fn decide<S: AsRef<str>>(_tool: Tool, args: &[S]) -> Decision {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let args: &[&str] = match args.split_first() {
        Some((&"--", rest)) => rest,
        _ => &args,
    };
    let [only] = args else {
        return Decision::Passthrough;
    };
    if is_web_url(only) {
        Decision::Url((*only).to_owned())
    } else {
        Decision::Passthrough
    }
}

/// Whether a string is an `http(s)` URL — the only thing a browser pane accepts.
///
/// Case-insensitive on the scheme (`HTTPS://` is legal), and it must have *some*
/// host: `http://` alone is not a page. The authoritative check for what a pane
/// will load is [`crate::ide::parse_url_origin`]; this is the cheap "is this even a
/// web address" gate in front of it.
#[must_use]
pub fn is_web_url(s: &str) -> bool {
    crate::ide::parse_url_origin(s).is_some()
}

/// Find the real executable a shim stands in front of.
///
/// `exclude` is the shim directory, and excluding it is not optional: a shim named
/// `open` that resolved `open` on `PATH` would find itself, and "opening a URL"
/// would be a fork bomb. Compared by canonical path, so a symlinked or
/// differently-spelled `PATH` entry cannot slip past it.
///
/// `/usr/bin/open` is checked directly on macOS rather than looked up, because that
/// is where it is and a `PATH` that does not contain it is still a machine where
/// the fallback has to work.
#[must_use]
pub fn real_opener(tool: Tool, exclude: Option<&Path>) -> Option<PathBuf> {
    let names: &[&str] = match tool {
        Tool::Open => &["open"],
        Tool::XdgOpen => &["xdg-open"],
        // The generic "open a web page" chain. `$BROWSER` itself is never consulted
        // — it is the variable pointing at the shim, so following it would loop.
        Tool::Browser => {
            if cfg!(target_os = "macos") {
                &["open"]
            } else {
                // Deliberately no `gio`: it takes a subcommand (`gio open <uri>`),
                // so `exec gio <url>` prints its usage and opens nothing — verified.
                // Everything in this list must accept a URL as its single argument,
                // because that is the whole contract the shims and the passthrough
                // are built on.
                &["xdg-open", "sensible-browser", "x-www-browser"]
            }
        }
    };
    if cfg!(target_os = "macos") && matches!(tool, Tool::Open | Tool::Browser) {
        let system = PathBuf::from("/usr/bin/open");
        if system.is_file() {
            return Some(system);
        }
    }
    resolve_in(names, exclude, &std::env::var_os("PATH")?)
}

/// Find the real executable named `name` on `PATH`, skipping `exclude`.
///
/// [`real_opener`] generalised for a shim that is not an opener — today the `claude`
/// wrapper that installs a coding agent's lifecycle hooks
/// (`crate::agent`). The exclusion carries the same weight it does there and for the
/// same reason: a shim named `claude` that resolved `claude` on `PATH` would find
/// itself, and the exec loop that follows is bounded only by an rlimit.
///
/// No hardcoded system fallback, unlike [`real_opener`]'s `/usr/bin/open`: there is
/// no canonical location for a third-party CLI, and inventing one would put a shim in
/// front of nothing. `None` means "not installed", and the caller's answer to that is
/// to pass the invocation through untouched.
#[must_use]
pub fn real_on_path(name: &str, exclude: Option<&Path>) -> Option<PathBuf> {
    resolve_in(&[name], exclude, &std::env::var_os("PATH")?)
}

/// [`real_opener`]'s search, with `PATH` passed in so a test does not have to
/// mutate the process environment to exercise the exclusion.
fn resolve_in(names: &[&str], exclude: Option<&Path>, path: &std::ffi::OsStr) -> Option<PathBuf> {
    let excluded = exclude.and_then(|p| p.canonicalize().ok());
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if let (Some(excluded), Ok(here)) = (excluded.as_ref(), dir.canonicalize())
            && &here == excluded
        {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
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

    #[test]
    fn only_a_lone_web_url_is_routed() {
        for tool in Tool::ALL.iter().copied() {
            assert_eq!(
                decide(tool, &["https://example.com/x"]),
                Decision::Url("https://example.com/x".to_owned()),
                "{tool:?}"
            );
            // A leading `--` is the shims' way of passing an argv that may start
            // with a dash, and is not part of the argument list.
            assert_eq!(
                decide(tool, &["--", "http://localhost:3000"]),
                Decision::Url("http://localhost:3000".to_owned()),
                "{tool:?}"
            );

            // Everything a wrapper must not swallow.
            for args in [
                vec![],
                vec!["."],
                vec!["report.pdf"],
                vec!["-a", "Safari", "https://example.com"],
                vec!["-R", "/tmp/x"],
                vec!["--args"],
                vec!["https://a.example", "https://b.example"],
                vec!["file:///etc/passwd"],
                vec!["vscode://file/tmp/x"],
                vec!["slack://channel"],
                vec!["mailto:a@b.c"],
                vec!["example.com"],
                vec!["http://"],
                vec!["-"],
            ] {
                assert_eq!(
                    decide(tool, &args),
                    Decision::Passthrough,
                    "{tool:?} {args:?} must reach the real tool untouched"
                );
            }
        }
    }

    #[test]
    fn tool_flags_round_trip() {
        for tool in Tool::ALL.iter().copied() {
            assert_eq!(Tool::parse(tool.flag()), Some(tool));
        }
        assert_eq!(Tool::parse("firefox"), None);
        // The shim's filename is the name the thing it replaces is called by, which
        // is the whole reason a shim directory works.
        assert_eq!(Tool::Open.shim_name(), "open");
        assert_eq!(Tool::XdgOpen.shim_name(), "xdg-open");
    }

    #[test]
    fn the_shim_directory_is_never_its_own_fallback() {
        // The failure this prevents is not a wrong answer, it is a fork bomb: a
        // shim called `open` that resolved `open` on PATH would exec itself.
        let dir = tempfile::TempDir::new().unwrap();
        let shim = dir.path().join("xdg-open");
        std::fs::write(&shim, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The process's own PATH is deliberately not touched: `user_path` tests in
        // this same binary read it, and a global mutation here would race them.
        let path = dir.path().as_os_str();
        assert_eq!(resolve_in(&["xdg-open"], Some(dir.path()), path), None);
        // …and without the exclusion it would have found exactly that file.
        assert_eq!(resolve_in(&["xdg-open"], None, path), Some(shim));
    }

    #[test]
    fn the_first_name_in_the_chain_wins() {
        let dir = tempfile::TempDir::new().unwrap();
        for name in ["gio", "xdg-open"] {
            let p = dir.path().join(name);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        assert_eq!(
            resolve_in(&["xdg-open", "gio"], None, dir.path().as_os_str()),
            Some(dir.path().join("xdg-open"))
        );
    }
}
