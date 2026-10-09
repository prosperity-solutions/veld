//! `veld worktree new` — hand a task off to a new worktree for a human to take
//! over.
//!
//! The way a coding agent branches work off without filing an issue somebody has
//! to pick up later, and without a sub-agent working in a checkout nobody can see:
//! Veld creates the worktree, files it into the rail's "From agents" section, and
//! the first window that shows it starts an agent pane there with the prompt
//! already sent. From then on the human drives it; nothing reports back here.
//!
//! # Exit status
//!
//! - `0` — the worktree exists (and, with a prompt, its agent pane is waiting).
//! - `1` — the request failed: the branch exists, the daemon is not running, the
//!   prompt is too long.
//! - `2` — usage: the prompt could not be read.
//! - `3` — not inside a worktree Veld knows, so there is no project to branch.
//! - `5` — the agent named is not one this project declares, or it declares none.

use std::io::Read as _;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum WorktreeCommand {
    /// Create a worktree of this project and hand it to the human, optionally with
    /// an agent waiting to start in it on a prompt.
    ///
    /// The project is the one the current terminal or directory belongs to. With a
    /// prompt, the worktree lands in the rail's "From agents" section and the first
    /// Veld window that shows it starts an agent pane there with the prompt as its
    /// first message; the human takes it from there. Nothing reports back to the
    /// caller.
    ///
    /// Exit status: 0 created, 1 failed, 2 usage, 3 not inside a worktree Veld
    /// knows, 5 no such agent pane.
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

        /// File it into this lane instead of "From agents".
        #[arg(long)]
        lane: Option<String>,

        /// Print `{"id", "path", "branch", "alias", "name", "waiting", "agent"}` on
        /// stdout. `waiting` is whether an agent pane is waiting to start; `agent` is
        /// the pane named, or null for the human's usual one.
        #[arg(long)]
        json: bool,

        /// Terminal session to hand off from. Defaults to `$VELD_PTY_SESSION`.
        #[arg(long, hide = true)]
        session: Option<String>,
    },
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
            lane,
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
            let session = session.or_else(|| {
                std::env::var("VELD_PTY_SESSION")
                    .ok()
                    .filter(|s| !s.is_empty())
            });
            let cwd = std::env::current_dir()
                .map(|d| d.to_string_lossy().into_owned())
                .ok();
            let request = serde_json::json!({
                "session_id": session,
                "cwd": cwd,
                "branch": branch,
                "from_here": from_here,
                "name": name,
                "lane": lane,
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
        Some("no_agent" | "unknown_agent") => 5,
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

/// One `POST /api/handoffs`.
///
/// A generous timeout, unlike the CLI's other daemon calls: creating a checkout
/// can fetch from the remote first (`git.createFrom = origin`).
async fn create(request: &serde_json::Value) -> Result<serde_json::Value, Refusal> {
    let refusal = |message: String| Refusal {
        message,
        code: None,
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| refusal(format!("could not reach the daemon: {e}")))?;
    let resp = client
        .post(format!(
            "{}/api/handoffs",
            veld_core::instance::daemon_base()
        ))
        .header("X-Veld-Request", "1")
        .json(request)
        .send()
        .await
        .map_err(|e| {
            refusal(format!(
                "could not reach the daemon ({e}) — is it running? Try `veld doctor`."
            ))
        })?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if status.is_success() {
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
        assert_eq!(exit_for(Some("anything_else")), 1);
        assert_eq!(exit_for(None), 1);
    }
}
