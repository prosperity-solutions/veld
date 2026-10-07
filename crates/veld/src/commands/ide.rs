//! `veld ide open` — put a file or a page in front of the human, beside the
//! terminal that asked.
//!
//! The deliberate counterpart to `veld open-url`. That one stands in for `open` and
//! `$BROWSER`, so every path it does not handle falls through to the system opener
//! — the only acceptable behaviour for a wrapper around a command people run all
//! day. This one is asked for **by name**, by an agent that wants a human to look
//! at something, so falling through would be a lie: it either opens a pane or
//! exits non-zero with a sentence.
//!
//! # Exit status
//!
//! - `0` — the pane is open (or was activated, if that file already had one).
//! - `1` — the request itself failed: no such file, a kind Veld cannot show, a
//!   daemon that is not running.
//! - `2` — usage: neither `--notify` nor `--quiet`.
//! - `3` — no worktree to open it in: not in a Veld terminal, and the working
//!   directory is not inside a worktree Veld knows.
//! - `4` — no Veld window is showing that worktree.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum IdeCommand {
    /// Show a file or a web page in the Veld window, beside this terminal.
    ///
    /// Markdown, CSV and TSV, source code and other text open in a file pane;
    /// HTML, PDFs, images and http(s) URLs open in a browser pane. Run from a Veld
    /// terminal it opens beside that terminal; anywhere else, in the worktree that
    /// contains the current directory. Never falls back to the system opener and
    /// never switches which worktree the window is showing.
    ///
    /// Say which kind of open this is: `--notify` when the human should stop and
    /// read it (an unread mark on the worktree, a notification), `--quiet` to just
    /// open the tab.
    ///
    /// Exit status: 0 opened, 1 failed, 2 usage, 3 not inside a worktree Veld
    /// knows, 4 no Veld window is showing that worktree.
    #[command(group(clap::ArgGroup::new("mode").args(["notify", "quiet"]).multiple(false)))]
    Open {
        /// A path (optionally `path:line`) or an http(s) URL.
        #[arg(value_name = "PATH-OR-URL")]
        target: String,

        /// Open it and tell the human: an unread mark on the worktree and a
        /// notification.
        #[arg(long)]
        notify: bool,

        /// Just open the tab.
        #[arg(long)]
        quiet: bool,

        /// Print `{"opened", "worktree", "path"|"url"}` on stdout.
        #[arg(long)]
        json: bool,

        /// Terminal session to open beside. Defaults to `$VELD_PTY_SESSION`.
        #[arg(long, hide = true)]
        session: Option<String>,
    },
}

/// The sentence for a missing mode. Its own constant so the test can pin it — an
/// agent reads it, and it is the whole of the usage help that agent gets.
const MODE_REQUIRED: &str = "veld: say --notify (ping the human) or --quiet (just open the tab)";

pub async fn run(command: IdeCommand) -> i32 {
    match command {
        IdeCommand::Open {
            target,
            notify,
            quiet,
            json,
            session,
        } => open(&target, notify, quiet, json, session).await,
    }
}

/// What the argument names, once split from any `:line`.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    Url(String),
    Path { path: String, line: Option<u32> },
}

/// Read the positional argument.
///
/// A URL is taken whole — `http://localhost:5173` has a colon and a number and is
/// not a line. For a path, `notes.md:12` names line 12 only when `notes.md:12` is
/// not itself a file: a name with a colon in it is legal, and the file that exists
/// wins. The two other spellings tools print are read too, and keep only the line:
/// `notes.md:12:5` (a column, which a file pane has no use for) and `notes.md:12-20`
/// (a range, opened at its start).
fn parse_target(raw: &str, exists: impl Fn(&str) -> bool) -> Target {
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Target::Url(raw.to_owned());
    }
    if !exists(raw)
        && let Some((path, line)) = split_line(raw, &exists)
    {
        return Target::Path {
            path: path.to_owned(),
            line: Some(line),
        };
    }
    Target::Path {
        path: raw.to_owned(),
        line: None,
    }
}

/// `path:N`, `path:N-M` or `path:N:M` split into the path and `N`, or `None` when
/// the suffix is none of those. `path:N:M` is read as a column only when `path:N`
/// is not itself a file — the same "the file that exists wins" as the plain form.
fn split_line<'a>(raw: &'a str, exists: &impl Fn(&str) -> bool) -> Option<(&'a str, u32)> {
    let number = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
    };
    let (path, suffix) = raw.rsplit_once(':')?;
    if path.is_empty() {
        return None;
    }
    if let Some((start, end)) = suffix.split_once('-') {
        return number(end).and(number(start)).map(|line| (path, line));
    }
    let last = number(suffix)?;
    if !exists(path)
        && let Some((file, line)) = path.rsplit_once(':')
        && let Some(line) = number(line)
        && !file.is_empty()
    {
        return Some((file, line));
    }
    Some((path, last))
}

/// For a target that named no file: when it reads like `host:port` — what an agent
/// types for a dev server and forgets the scheme of — the URL it probably meant.
///
/// Only a bare `host:port` (no `/`), and only a host that is `localhost`, an IP, or
/// a dotted name Veld would not route as a file (`text_kind`/`servable_type`) — so
/// `notes.md:12` that does not exist stays "no such file", not a guess at
/// `http://notes.md`. A missing file of a kind Veld does not know (`Cargo.lock:3`)
/// still gets the hint; it is only a suggestion in an error.
fn url_hint(raw: &str) -> Option<String> {
    let (host, port) = raw.rsplit_once(':')?;
    if raw.contains('/') || host.is_empty() || port.is_empty() {
        return None;
    }
    if !port.bytes().all(|b| b.is_ascii_digit()) || port.parse::<u16>().is_err() {
        return None;
    }
    let host_like = host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::Ipv4Addr>().is_ok()
        || (host.contains('.')
            && veld_core::files::text_kind(host).is_none()
            && veld_core::files::servable_type(host).is_none());
    host_like.then(|| format!("http://{raw}"))
}

/// What a refusal from the daemon means for the exit status.
fn exit_for(code: Option<&str>) -> i32 {
    match code {
        Some("not_in_worktree") => 3,
        Some("no_window") => 4,
        _ => 1,
    }
}

async fn open(raw: &str, notify: bool, quiet: bool, json: bool, session: Option<String>) -> i32 {
    if !notify && !quiet {
        eprintln!("{MODE_REQUIRED}");
        return 2;
    }
    let target = match parse_target(raw, |p| std::path::Path::new(p).exists()) {
        Target::Url(url) => (url, None),
        // Absolute but **not** resolved: the daemon resolves it itself, and judges
        // the spelling as well as the file it lands on — `~/.docker/config.json`
        // linked into a dotfiles repo is a login by its name, not by its target's.
        // `std::path::absolute` keeps `..`, which only the filesystem can apply
        // correctly past a symlink.
        Target::Path { path, line } => match std::path::absolute(&path) {
            Ok(full) if full.exists() => (full.to_string_lossy().into_owned(), line),
            _ => {
                match url_hint(raw) {
                    Some(url) => eprintln!("veld: no such file: {path} — did you mean {url}?"),
                    None => eprintln!("veld: no such file: {path}"),
                }
                return 1;
            }
        },
    };

    let cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let session = session.or_else(|| {
        std::env::var("VELD_PTY_SESSION")
            .ok()
            .filter(|s| !s.is_empty())
    });

    // The terminal first, because that is where the pane belongs. A session the
    // daemon no longer has — a shell that outlived its daemon's records — is not a
    // reason to give up: the directory still says which worktree this is.
    let mut answer = match &session {
        Some(id) => ask(serde_json::json!({ "sessionId": id }), &target, notify).await,
        None => ask(serde_json::json!({ "cwd": cwd }), &target, notify).await,
    };
    if session.is_some()
        && let Err(Refusal {
            code: Some(code), ..
        }) = &answer
        && code == "no_session"
    {
        answer = ask(serde_json::json!({ "cwd": cwd }), &target, notify).await;
    }

    match answer {
        Ok(body) => {
            let what = body["path"]
                .as_str()
                .or_else(|| body["url"].as_str())
                .unwrap_or(raw);
            let pane = if body["opened"] == "file" {
                "a file pane"
            } else {
                "a browser pane"
            };
            let worktree = body["worktree"].as_str().unwrap_or("the worktree");
            eprintln!("Opened {what} in {pane} ({worktree}).");
            if json {
                let mut out = serde_json::json!({
                    "opened": body["opened"],
                    "worktree": body["worktree"],
                });
                for key in ["path", "url"] {
                    if !body[key].is_null() {
                        out[key] = body[key].clone();
                    }
                }
                println!("{out}");
            }
            0
        }
        Err(refusal) => {
            eprintln!("veld: {}", refusal.message);
            exit_for(refusal.code.as_deref())
        }
    }
}

struct Refusal {
    message: String,
    code: Option<String>,
}

/// One `POST /api/ide/open`. `who` is the one field naming the worktree.
async fn ask(
    mut who: serde_json::Value,
    target: &(String, Option<u32>),
    notify: bool,
) -> Result<serde_json::Value, Refusal> {
    who["target"] = target.0.clone().into();
    if let Some(line) = target.1 {
        who["line"] = line.into();
    }
    who["notify"] = notify.into();
    let refusal = |message: String| Refusal {
        message,
        code: None,
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| refusal(format!("could not reach the daemon: {e}")))?;
    let resp = client
        .post(format!(
            "{}/api/ide/open",
            veld_core::instance::daemon_base()
        ))
        .header("X-Veld-Request", "1")
        .json(&who)
        .send()
        .await
        .map_err(|e| {
            refusal(format!(
                "could not reach the daemon ({e}) — is it running? Try `veld doctor`."
            ))
        })?;
    let ok = resp.status().is_success();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if ok {
        return Ok(body);
    }
    Err(Refusal {
        message: body["error"]
            .as_str()
            .map_or_else(|| format!("the daemon refused ({status})"), str::to_owned),
        code: body["code"].as_str().map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_suffix_is_split_only_when_the_whole_name_is_not_a_file() {
        let none = |_: &str| false;
        assert_eq!(
            parse_target("docs/plan.md:12", none),
            Target::Path {
                path: "docs/plan.md".into(),
                line: Some(12)
            }
        );
        assert_eq!(
            parse_target("docs/plan.md", none),
            Target::Path {
                path: "docs/plan.md".into(),
                line: None
            }
        );
        // A file whose name really ends in `:12` is that file.
        assert_eq!(
            parse_target("odd:12", |p| p == "odd:12"),
            Target::Path {
                path: "odd:12".into(),
                line: None
            }
        );
        // A port is not a line.
        assert_eq!(
            parse_target("http://localhost:5173", none),
            Target::Url("http://localhost:5173".into())
        );
    }

    #[test]
    fn a_column_or_a_range_keeps_only_the_line() {
        let none = |_: &str| false;
        let at = |path: &str, line| Target::Path {
            path: path.into(),
            line: Some(line),
        };
        assert_eq!(parse_target("src/a.rs:12:5", none), at("src/a.rs", 12));
        assert_eq!(parse_target("src/a.rs:12-20", none), at("src/a.rs", 12));
        // `odd:12` is a file, so `:5` is its line, not a column.
        assert_eq!(parse_target("odd:12:5", |p| p == "odd:12"), at("odd:12", 5));
        // A file really called `x:1-2` is that file.
        assert_eq!(
            parse_target("x:1-2", |p| p == "x:1-2"),
            Target::Path {
                path: "x:1-2".into(),
                line: None
            }
        );
        // Anything else after the colon is part of the name.
        for raw in ["a.rs:12-", "a.rs:-3", "a.rs:x", "a.rs:1-x"] {
            assert_eq!(
                parse_target(raw, none),
                Target::Path {
                    path: raw.into(),
                    line: None
                },
                "{raw}"
            );
        }
    }

    #[test]
    fn a_host_and_port_without_a_scheme_gets_a_url_hint() {
        assert_eq!(
            url_hint("localhost:3000").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            url_hint("127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            url_hint("app.test:5173").as_deref(),
            Some("http://app.test:5173")
        );
        for not in [
            "notes.md:12",
            "src/a.rs:3",
            "localhost",
            "localhost:99999",
            "README:4",
        ] {
            assert_eq!(url_hint(not), None, "{not}");
        }
    }

    #[test]
    fn refusals_map_to_the_documented_exit_statuses() {
        assert_eq!(exit_for(Some("not_in_worktree")), 3);
        assert_eq!(exit_for(Some("no_window")), 4);
        assert_eq!(exit_for(Some("no_session")), 1);
        assert_eq!(exit_for(None), 1);
    }

    #[tokio::test]
    async fn neither_mode_is_a_usage_error_with_the_documented_sentence() {
        assert_eq!(open("x.md", false, false, false, None).await, 2);
        assert!(MODE_REQUIRED.contains("--notify") && MODE_REQUIRED.contains("--quiet"));
    }

    /// Both at once is refused by clap itself, before `run` sees it.
    #[test]
    fn notify_and_quiet_together_are_refused() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: IdeCommand,
        }
        assert!(Cli::try_parse_from(["t", "open", "x.md", "--notify", "--quiet"]).is_err());
        assert!(Cli::try_parse_from(["t", "open", "x.md", "--quiet"]).is_ok());
    }
}
