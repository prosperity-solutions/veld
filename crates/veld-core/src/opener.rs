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
//! a day. So the rule is deliberately narrow: **exactly one argument, carrying no
//! flag and no non-web URI scheme.** Everything else is [`Decision::Passthrough`],
//! and the caller `exec`s the real tool with the original argv untouched.
//!
//! Such an argument is an `http(s)` URL ([`Decision::Url`]) or a candidate path
//! ([`Decision::Path`]). The second is a *question*, not a verdict — most of them
//! are `.` — and it is answered in two further steps: by the CLI (does this name a
//! regular file?) and then by the daemon (is this a file the user wants in a pane?).
//! Every step answering "no" lands back on the untouched-argv passthrough, so the
//! guarantee above survives the widening.
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
    /// A single bare argument that could name a file a pane can show.
    ///
    /// Deliberately *not* a verdict. Whether the string names an existing regular
    /// file is a filesystem question, and whether that file is one the user wants
    /// opened in a pane is a policy question answered from settings — so this
    /// module, which is pure and is compiled into both the daemon and the CLI,
    /// answers neither. It reports only "one argument, no flag, no URI scheme",
    /// and both remaining questions are asked downstream, in that order.
    ///
    /// The overwhelmingly common values here are `.` and a directory name, which
    /// resolve to [`Decision::Passthrough`] one step later. That is why this
    /// variant may never print anything on its own.
    ///
    /// **`_tool` is ignored, so this reaches `$BROWSER` too**, and that is a real
    /// behaviour change rather than a side effect: `$BROWSER ./htmlcov/index.html` —
    /// Python's `webbrowser`, `git web--browse`, a coverage tool opening its own report
    /// — used to reach the system browser and now lands in a pane. It is wanted (that
    /// report is exactly what this feature is for) and it is bounded by the same two
    /// gates as the shims: the path must be inside the session's worktree, and the file
    /// must be one the view policy accepts.
    Path(String),
    /// Not a plain web page: hand the original argv to the real tool.
    Passthrough,
}

/// What an invocation is: a web page, something that might be a viewable file, or
/// none of Veld's business.
///
/// Narrow on purpose — see the module docs. Note what is *never* either of the
/// first two:
///
/// - more than one argument (`open a.pdf b.pdf`, `open -a Safari url`),
/// - anything beginning with `-` (a flag, which changes what `open` does),
/// - anything carrying a URI scheme that is not `http`/`https` (`vscode://`,
///   `file://`, `slack://`, `mailto:`): a pane cannot show those, and handing them
///   anywhere but the OS would break a deep link.
///
/// `file://` stays a passthrough even though the bytes behind it are exactly what
/// [`Decision::Path`] is about. Accepting it would widen what the shim swallows for
/// a spelling nobody types by hand, and `open file:///…` already does the right
/// thing without Veld in the picture.
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
        return Decision::Url((*only).to_owned());
    }
    if only.starts_with('-') || only.is_empty() || has_uri_scheme(only) {
        return Decision::Passthrough;
    }
    Decision::Path((*only).to_owned())
}

/// Whether a string starts with something a URL parser would read as a scheme.
///
/// The grammar is RFC 3986's: an ASCII letter, then letters, digits, `+`, `-`, `.`,
/// then a colon. Used only to keep a deep link (`vscode://file/x`, `slack://`,
/// `mailto:a@b`) out of [`Decision::Path`], so it errs toward calling something a
/// scheme: a relative path containing a colon before its first `/` is legal on Unix
/// and vanishingly rare, and the cost of misjudging it is that `open` behaves
/// exactly as it did before Veld existed.
fn has_uri_scheme(s: &str) -> bool {
    let Some(colon) = s.find(':') else {
        return false;
    };
    let (scheme, _) = s.split_at(colon);
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
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

            // Everything a wrapper must not swallow. Note what is *not* in this
            // list any more: a lone bare word. `report.pdf` and `.` are now
            // `Path`, and it is the CLI's `stat` and the daemon's policy that send
            // them back here — see the case below.
            for args in [
                vec![],
                vec!["-a", "Safari", "https://example.com"],
                vec!["-R", "/tmp/x"],
                vec!["--args"],
                vec!["https://a.example", "https://b.example"],
                vec!["file:///etc/passwd"],
                vec!["vscode://file/tmp/x"],
                vec!["slack://channel"],
                vec!["mailto:a@b.c"],
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

    /// A lone bare argument is *offered* as a path — not accepted as one.
    ///
    /// The distinction is the whole point of the variant: `.` and `example.com` are
    /// both `Path` here and both end up at the real tool, because neither names a
    /// regular file. This test pins the classification, not the outcome.
    #[test]
    fn a_lone_bare_argument_is_offered_as_a_path() {
        for tool in Tool::ALL.iter().copied() {
            for arg in [
                "report.pdf",
                "./deck.html",
                "docs/slides.html",
                "/tmp/analysis.html",
                "~/notes/deck.html",
                // Not a file, and not this module's job to know that.
                ".",
                "example.com",
                // A colon that is not a scheme: no letter before it.
                "9:30.html",
            ] {
                assert_eq!(
                    decide(tool, &[arg]),
                    Decision::Path(arg.to_owned()),
                    "{tool:?} {arg:?}"
                );
            }
            // The shims' separator is dropped here too, so a path beginning with a
            // dash still arrives as one argument.
            assert_eq!(
                decide(tool, &["--", "weird-name.html"]),
                Decision::Path("weird-name.html".to_owned()),
                "{tool:?}"
            );
        }
    }

    #[test]
    fn a_uri_scheme_is_never_mistaken_for_a_path() {
        for s in [
            "vscode://file/tmp/x",
            "mailto:a@b.c",
            "file:///etc/passwd",
            "slack://channel",
            "x-devonthink-item://abc",
            "a+b.c-d:whatever",
        ] {
            assert!(has_uri_scheme(s), "{s:?} carries a scheme");
        }
        for s in [
            "report.pdf",
            "./deck.html",
            "/tmp/x",
            "9:30.html",
            ":leading-colon",
            "",
        ] {
            assert!(!has_uri_scheme(s), "{s:?} carries no scheme");
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
