//! `veld worktree new` — hand a task off to a new worktree for a human to take
//! over.
//!
//! The way a coding agent branches work off without filing an issue somebody has
//! to pick up later, and without a sub-agent working in a checkout nobody can see:
//! Veld creates the worktree, files it into the rail's "Waiting for you" section, and
//! the first window that shows it starts an agent pane there with the prompt
//! already sent. From then on the human drives it; nothing reports back here.
//!
//! `veld worktree groups` lists the groups a hand-off can be filed into instead.
//!
//! # Exit status
//!
//! - `0` — the worktree exists (and, with a prompt, its agent pane is waiting).
//! - `1` — the request was refused, so nothing was created: the branch exists,
//!   the daemon is not running, the prompt is too long.
//! - `2` — usage: bad flags, or the prompt could not be read.
//! - `3` — not inside a worktree Veld knows, so there is no project to branch.
//! - `4` — no group by that name in this project.
//! - `5` — the agent named is not one this project declares, or it declares none.
//! - `6` — the worktree may exist without everything it was asked for: no answer
//!   arrived in time, or the hand-off failed partway. Check the rail, do not run
//!   it again.

use std::io::Read as _;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum WorktreeCommand {
    /// Create a worktree of this project and hand it to the human, optionally with
    /// an agent waiting to start in it on a prompt.
    ///
    /// The project is the one the current directory belongs to. With a prompt, the
    /// worktree lands in the rail's "Waiting for you" section (or the group named by
    /// `--group`) and the first Veld window that shows it starts an agent pane there
    /// with the prompt as its first message; the human takes it from there.
    /// Nothing reports back to the caller.
    ///
    /// Exit status: 0 created, 1 refused (nothing created), 2 usage, 3 not inside a
    /// worktree Veld knows, 4 no such group, 5 no such agent pane, 6 it may exist
    /// without everything asked for — check the rail rather than retrying.
    #[command(group(clap::ArgGroup::new("first").args(["prompt", "prompt_file"]).multiple(false)))]
    New {
        /// The branch to create.
        #[arg(long)]
        branch: String,

        /// The agent's first message.
        #[arg(long, value_name = "TEXT")]
        prompt: Option<String>,

        /// Read the first message from a file, or from stdin with `-`.
        #[arg(long, value_name = "PATH")]
        prompt_file: Option<std::path::PathBuf>,

        /// The agent pane to start (an `ide.panes` id). Defaults to the one the
        /// human usually picks in this project.
        #[arg(long, value_name = "PANE")]
        agent: Option<String>,

        /// The name the rail shows. Defaults to one generated from the prompt when
        /// the project declares `ide.worktreeName`, else the branch.
        #[arg(long)]
        name: Option<String>,

        /// Start the branch at this checkout's HEAD, instead of where a new branch
        /// normally starts. Uncommitted changes stay here.
        #[arg(long)]
        from_here: bool,

        /// File it into this existing group instead of "Waiting for you" (or, with
        /// no prompt, instead of the ungrouped section). See `veld worktree groups`
        /// for the names.
        #[arg(long, visible_alias = "lane", value_name = "NAME")]
        group: Option<String>,

        /// Print `{"id", "path", "branch", "alias", "name", "waiting", "agent"}` on
        /// stdout. `waiting` is whether an agent pane is waiting to start; `agent` is
        /// the pane named, or null for the human's usual one.
        #[arg(long)]
        json: bool,

        /// Terminal session to hand off from, used only when the working directory
        /// cannot be read. Defaults to `$VELD_PTY_SESSION`.
        #[arg(long, hide = true)]
        session: Option<String>,
    },

    /// List the groups of this project's rail that `veld worktree new --group` can
    /// file into.
    ///
    /// Exit status: 0 listed, 1 failed, 3 not inside a worktree Veld knows.
    Groups {
        /// Print `{"project", "groups": [..]}` on stdout.
        #[arg(long)]
        json: bool,

        /// Terminal session to resolve from when the working directory cannot be
        /// read. Defaults to `$VELD_PTY_SESSION`.
        #[arg(long, hide = true)]
        session: Option<String>,
    },
}

/// Where this caller is: its working directory, and its terminal session for a
/// daemon to fall back on when the directory cannot be read.
fn whereabouts(session: Option<String>) -> (Option<String>, Option<String>) {
    let session = session.or_else(|| {
        std::env::var("VELD_PTY_SESSION")
            .ok()
            .filter(|s| !s.is_empty())
    });
    let cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .ok();
    (cwd, session)
}

pub async fn run(command: WorktreeCommand) -> i32 {
    match command {
        WorktreeCommand::New {
            branch,
            prompt,
            prompt_file,
            agent,
            name,
            from_here,
            group,
            json,
            session,
        } => {
            let prompt = match read_prompt(prompt, prompt_file.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("veld: {e}");
                    return 2;
                }
            };
            let (cwd, session) = whereabouts(session);
            let request = serde_json::json!({
                "session_id": session,
                "cwd": cwd,
                "branch": branch,
                "from_here": from_here,
                "name": name,
                "lane": group,
                "agent": agent,
                "prompt": prompt,
            });
            match create(&request).await {
                Ok(body) => {
                    report(&body, json);
                    0
                }
                Err(refusal) => {
                    eprintln!("veld: {}", refusal.message);
                    exit_for(refusal.code.as_deref())
                }
            }
        }
        WorktreeCommand::Groups { json, session } => {
            let (cwd, session) = whereabouts(session);
            let mut query = vec![];
            if let Some(cwd) = &cwd {
                query.push(("cwd", cwd.as_str()));
            }
            if let Some(session) = &session {
                query.push(("session_id", session.as_str()));
            }
            match send(
                reqwest::Client::new()
                    .get(format!(
                        "{}/api/handoffs/groups",
                        veld_core::instance::daemon_base()
                    ))
                    .query(&query),
                std::time::Duration::from_secs(10),
            )
            .await
            {
                Ok(body) => {
                    let groups: Vec<&str> = body["groups"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|g| g.as_str()).collect())
                        .unwrap_or_default();
                    if json {
                        println!("{body}");
                    } else if groups.is_empty() {
                        eprintln!(
                            "This project has no groups; hand-offs land in \"Waiting for you\"."
                        );
                    } else {
                        for g in groups {
                            println!("{g}");
                        }
                    }
                    0
                }
                Err(refusal) => {
                    eprintln!("veld: {}", refusal.message);
                    exit_for(refusal.code.as_deref())
                }
            }
        }
    }
}

/// The prompt from `--prompt` or `--prompt-file`, `None` for neither. Blank counts
/// as none, so `--prompt ""` from a script is a plain create rather than an error.
fn read_prompt(
    inline: Option<String>,
    file: Option<&std::path::Path>,
) -> Result<Option<String>, String> {
    let text = match (inline, file) {
        (Some(text), _) => text,
        (None, Some(path)) if path.as_os_str() == "-" => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| format!("could not read the prompt from stdin: {e}"))?;
            text
        }
        (None, Some(path)) => std::fs::read_to_string(path)
            .map_err(|e| format!("could not read the prompt from {}: {e}", path.display()))?,
        (None, None) => return Ok(None),
    };
    Ok(Some(text).filter(|t| !t.trim().is_empty()))
}

/// What a refusal from the daemon means for the exit status.
fn exit_for(code: Option<&str>) -> i32 {
    match code {
        Some("not_in_worktree") => 3,
        Some("unknown_group") => 4,
        Some("no_agent" | "unknown_agent") => 5,
        Some(NO_ANSWER | "created_unrecorded") => 6,
        _ => 1,
    }
}

/// The human line on stderr, and with `--json` the machine one on stdout.
fn report(body: &serde_json::Value, json: bool) {
    let name = body["display_name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .or_else(|| body["alias"].as_str())
        .unwrap_or("the worktree");
    let path = body["path"].as_str().unwrap_or_default();
    let waiting = body["handoff"].is_object();
    if waiting {
        eprintln!(
            "Created {name} at {path}. Its agent starts when the worktree is first opened in Veld."
        );
    } else {
        eprintln!("Created {name} at {path}.");
    }
    if json {
        let agent = body["handoff"]["pane"]
            .as_str()
            .filter(|p| !p.is_empty())
            .map_or(serde_json::Value::Null, |p| p.into());
        let out = serde_json::json!({
            "id": body["id"],
            "path": body["path"],
            "branch": body["branch"],
            "alias": body["alias"],
            "name": name,
            "waiting": waiting,
            "agent": agent,
        });
        println!("{out}");
    }
}

struct Refusal {
    message: String,
    code: Option<String>,
}

/// The [`Refusal`] code for a request that was sent and got no answer: the daemon
/// may well be finishing it.
const NO_ANSWER: &str = "no_answer";

/// One `POST /api/handoffs`.
///
/// A generous timeout, unlike the CLI's other daemon calls: creating a checkout
/// can fetch from the remote first (`git.createFrom = origin`), and writing a
/// large tree takes a while.
async fn create(request: &serde_json::Value) -> Result<serde_json::Value, Refusal> {
    send(
        reqwest::Client::new()
            .post(format!(
                "{}/api/handoffs",
                veld_core::instance::daemon_base()
            ))
            .json(request),
        std::time::Duration::from_secs(120),
    )
    .await
}

/// Send one request to the daemon and read its JSON answer, or a [`Refusal`]
/// carrying the daemon's sentence and `code`.
async fn send(
    request: reqwest::RequestBuilder,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, Refusal> {
    let resp = request
        .header("X-Veld-Request", "1")
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| {
            // Only a refused connection means nothing happened. Anything after the
            // request went out — a timeout, a dropped connection — can be a create
            // the daemon is still finishing, and running it again would collide
            // with its branch.
            if e.is_connect() {
                Refusal {
                    message: format!(
                        "could not reach the daemon ({e}) — is it running? Try `veld doctor`."
                    ),
                    code: None,
                }
            } else {
                Refusal {
                    message: "no answer from the daemon — the worktree may still be on its \
                              way; check the Veld rail instead of running this again"
                        .to_owned(),
                    code: Some(NO_ANSWER.to_owned()),
                }
            }
        })?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if status.is_success() {
        return Ok(body);
    }
    let message = match body["error"].as_str() {
        Some(sentence) => sentence.to_owned(),
        // Every route here answers with a sentence; a bare 404 or 405 is a daemon
        // that predates them, which is what a CLI updated ahead of its daemon meets.
        None if matches!(status.as_u16(), 404 | 405) => {
            "the running Veld daemon is older than this command — restart it (or run \
             `veld doctor`) and try again"
                .to_owned()
        }
        None => format!("the daemon refused ({status})"),
    };
    Err(Refusal {
        message,
        code: body["code"].as_str().map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: WorktreeCommand,
    }

    #[test]
    fn a_prompt_comes_from_one_place_only() {
        let both = Cli::try_parse_from([
            "veld",
            "new",
            "--branch",
            "b",
            "--prompt",
            "x",
            "--prompt-file",
            "p.md",
        ]);
        assert!(both.is_err());
        assert!(Cli::try_parse_from(["veld", "new", "--branch", "b", "--prompt", "x"]).is_ok());
        assert!(Cli::try_parse_from(["veld", "new", "--branch", "b"]).is_ok());
        assert!(Cli::try_parse_from(["veld", "new", "--prompt", "x"]).is_err());
        // `--lane` is kept as the alias of `--group`, the rail's own word.
        assert!(Cli::try_parse_from(["veld", "new", "--branch", "b", "--lane", "review"]).is_ok());
        assert!(Cli::try_parse_from(["veld", "groups", "--json"]).is_ok());
    }

    #[test]
    fn a_blank_prompt_is_no_prompt() {
        assert_eq!(read_prompt(Some("  \n".into()), None).unwrap(), None);
        assert_eq!(read_prompt(None, None).unwrap(), None);
        assert_eq!(
            read_prompt(Some("fix it".into()), None).unwrap().as_deref(),
            Some("fix it")
        );
    }

    #[test]
    fn a_prompt_file_is_read_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.md");
        std::fs::write(&path, "# Task\n\nDo the thing.\n").unwrap();
        assert_eq!(
            read_prompt(None, Some(&path)).unwrap().as_deref(),
            Some("# Task\n\nDo the thing.\n")
        );
        assert!(read_prompt(None, Some(&dir.path().join("missing.md"))).is_err());
    }

    #[test]
    fn refusals_map_to_their_documented_exit_status() {
        assert_eq!(exit_for(Some("not_in_worktree")), 3);
        assert_eq!(exit_for(Some("no_agent")), 5);
        assert_eq!(exit_for(Some("unknown_agent")), 5);
        assert_eq!(exit_for(Some("unknown_group")), 4);
        assert_eq!(exit_for(Some(NO_ANSWER)), 6);
        assert_eq!(exit_for(Some("created_unrecorded")), 6);
        assert_eq!(exit_for(Some("anything_else")), 1);
        assert_eq!(exit_for(None), 1);
    }
}
