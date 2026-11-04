//! `veld ide link` — the links an agent pins to the worktree it works for: the
//! pull requests it opened, the preview it deployed, the issue it works on, a page
//! the human keeps coming back to.
//!
//! The Veld window shows them in the top bar, grouped by kind, and pull requests
//! beside the worktree's name in the rail. Veld stores what it is told and checks
//! nothing against a forge, so a link is only as current as its last `set`.
//!
//! # Exit status
//!
//! - `0` — done.
//! - `1` — failed: the daemon is not running or did not answer.
//! - `2` — not a usable link: a bad URL, kind, tone, icon or id, a label that is
//!   too long, a control or invisible character, a URL on the origin of one of
//!   the worktree's live runs (the top bar already opens those), or the worktree
//!   already carries the maximum number of links.
//! - `3` — not inside a worktree Veld knows.
//! - `4` — `rm`: the worktree has no such link.

use clap::{Args, Subcommand};

use super::worktree::{Refusal, send, whereabouts};

#[derive(Subcommand)]
pub enum LinkCommand {
    /// Pin a link to the worktree this runs in, or update one already pinned.
    ///
    /// For what the human will keep coming back to while this work is open — the
    /// pull requests you opened (across repositories too), the preview environment
    /// you deployed, the issue or spec the work is about. Not for pages you read
    /// while researching.
    ///
    /// Setting the same link again updates it: a flag you leave out keeps its
    /// value, and an empty value (`--label ""`) resets it to its default. A link is
    /// the same link when it has the same `--id`; without one, it is filed under its
    /// URL, ignoring the query, fragment and a trailing slash. Give a preview whose
    /// URL changes with every deploy an id per service (`--id web`, `--id api`), so
    /// the next deploy replaces it instead of adding a second one.
    ///
    /// Re-run it when the state changes, e.g. once a pull request is merged:
    /// `veld ide link set <url> --tone success --icon git-merge`.
    ///
    /// Don't pin what the top bar already shows: a URL on the origin of one of this
    /// worktree's live runs is refused, and the answer (like `list`'s) names the
    /// project's own top-bar entries with what a status badge shows now. Pin your
    /// pull requests regardless: the rail shows only pinned ones.
    ///
    /// Exit status: 0 set, 1 failed, 2 not a usable link (or one the top bar
    /// already shows, or the worktree is full), 3 not inside a worktree Veld knows.
    Set(SetArgs),

    /// List the links pinned to the worktree this runs in, and what else its top
    /// bar shows — the project's own badges and buttons, and the live runs' URLs —
    /// so a link does not repeat one of them.
    ///
    /// Exit status: 0 listed (even when there are none), 1 failed, 3 not inside a
    /// worktree Veld knows.
    List {
        /// Print `{"worktree": {id, path, branch}, "links": [..], "also_in_top_bar":
        /// {"project_entries": [{id, label, type, shown: {text, href} | null}],
        /// "run_urls": [..]}}` on stdout.
        #[arg(long)]
        json: bool,

        /// Terminal session to resolve from. Defaults to `$VELD_PTY_SESSION`.
        #[arg(long, hide = true)]
        session: Option<String>,
    },

    /// Remove a link by its id or its URL, or every link with `--all`.
    ///
    /// Exit status: 0 removed, 1 failed, 2 neither a link nor `--all` given, 3 not
    /// inside a worktree Veld knows, 4 no such link.
    Rm {
        /// The link's id (`veld ide link list` shows it), or its URL.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        link: Option<String>,

        /// Remove every link of this worktree.
        #[arg(long)]
        all: bool,

        /// Print `{"worktree": {..}, "removed": [id, ..]}` on stdout.
        #[arg(long)]
        json: bool,

        /// Terminal session to resolve from. Defaults to `$VELD_PTY_SESSION`.
        #[arg(long, hide = true)]
        session: Option<String>,
    },
}

#[derive(Args)]
pub struct SetArgs {
    /// The http(s) URL the link opens.
    pub url: String,

    /// What it is: `pr`, `preview`, `issue`, `doc` or `link` (the default for a new
    /// link). Decides its glyph and how the top bar groups it; `pr` links also
    /// show in the rail beside the worktree's name.
    #[arg(long)]
    pub kind: Option<String>,

    /// The text shown, at most 60 characters — e.g. `api#123 Add login` for a pull
    /// request. Defaults to the URL's host and the end of its path.
    #[arg(long)]
    pub label: Option<String>,

    /// Its colour: `neutral` (default), `info`, `success`, `warning` or `danger` —
    /// e.g. `danger` for failing checks, `success` once merged.
    #[arg(long)]
    pub tone: Option<String>,

    /// A glyph instead of the kind's own: a Tabler icon name Veld knows (e.g.
    /// `git-merge`), or an emoji.
    #[arg(long)]
    pub icon: Option<String>,

    /// Extra text on hover, at most 400 characters — e.g. the pull request's title
    /// and check status.
    #[arg(long)]
    pub tooltip: Option<String>,

    /// The id to file it under, so a later `set` with the same id replaces its
    /// URL — e.g. `web` and `api` for the previews of two services.
    #[arg(long)]
    pub id: Option<String>,

    /// Print `{"worktree": {..}, "link": {..}, "created": bool, "also_in_top_bar":
    /// {..}}` on stdout.
    #[arg(long)]
    pub json: bool,

    /// Terminal session to resolve from. Defaults to `$VELD_PTY_SESSION`.
    #[arg(long, hide = true)]
    pub session: Option<String>,
}

/// What a refusal from the daemon means for the exit status.
fn exit_for(code: Option<&str>) -> i32 {
    match code {
        Some("bad_link" | "links_full" | "already_shown") => 2,
        Some("not_in_worktree") => 3,
        Some("no_such_link") => 4,
        _ => 1,
    }
}

async fn call(path: &str, request: serde_json::Value) -> Result<serde_json::Value, Refusal> {
    send(
        reqwest::Client::new()
            .post(format!(
                "{}/api/ide/links/{path}",
                veld_core::instance::daemon_base()
            ))
            .json(&request),
        std::time::Duration::from_secs(10),
    )
    .await
}

pub async fn run(command: LinkCommand) -> i32 {
    let (path, request, json) = match command {
        LinkCommand::Set(args) => {
            let (cwd, session) = whereabouts(args.session);
            let mut link = serde_json::json!({ "url": args.url });
            for (key, value) in [
                ("id", args.id),
                ("kind", args.kind),
                ("label", args.label),
                ("icon", args.icon),
                ("tone", args.tone),
                ("tooltip", args.tooltip),
            ] {
                if let Some(value) = value {
                    link[key] = value.into();
                }
            }
            (
                "set",
                serde_json::json!({ "session_id": session, "cwd": cwd, "link": link }),
                args.json,
            )
        }
        LinkCommand::List { json, session } => {
            let (cwd, session) = whereabouts(session);
            (
                "list",
                serde_json::json!({ "session_id": session, "cwd": cwd }),
                json,
            )
        }
        LinkCommand::Rm {
            link,
            all,
            json,
            session,
        } => {
            let (cwd, session) = whereabouts(session);
            (
                "remove",
                serde_json::json!({ "session_id": session, "cwd": cwd, "id": link, "all": all }),
                json,
            )
        }
    };
    match call(path, request).await {
        Ok(body) => {
            if json {
                println!("{body}");
            } else {
                report(path, &body);
            }
            0
        }
        Err(refusal) => {
            eprintln!("veld: {}", refusal.message);
            exit_for(refusal.code.as_deref())
        }
    }
}

/// The answer without `--json`: on stderr for a change, on stdout for `list`.
fn report(path: &str, body: &serde_json::Value) {
    let where_ = body["worktree"]["path"].as_str().unwrap_or("the worktree");
    match path {
        "set" => {
            let link = &body["link"];
            let verb = if body["created"].as_bool() == Some(true) {
                "Pinned"
            } else {
                "Updated"
            };
            eprintln!(
                "{verb} {} \"{}\" (id {}) on {where_}.",
                link["kind"].as_str().unwrap_or("link"),
                link["label"].as_str().unwrap_or_default(),
                link["id"].as_str().unwrap_or_default(),
            );
            for line in also_shown_lines(&body["also_in_top_bar"]) {
                eprintln!("{line}");
            }
        }
        "remove" => {
            let n = body["removed"].as_array().map_or(0, Vec::len);
            eprintln!(
                "Removed {n} link{} from {where_}.",
                if n == 1 { "" } else { "s" }
            );
        }
        _ => {
            for line in also_shown_lines(&body["also_in_top_bar"]) {
                println!("{line}");
            }
            let links = body["links"].as_array().cloned().unwrap_or_default();
            if links.is_empty() {
                eprintln!("No links on {where_}.");
                return;
            }
            let branch = body["worktree"]["branch"].as_str().unwrap_or_default();
            for l in &links {
                let url = l["url"].as_str().unwrap_or_default();
                let id = l["id"].as_str().unwrap_or_default();
                let set_on = l["branch"].as_str().unwrap_or_default();
                let mut extra = String::new();
                if id != veld_core::db::normalize_link_url(url) {
                    extra.push_str(&format!(" id={id}"));
                }
                if !set_on.is_empty()
                    && !branch.is_empty()
                    && branch != "(detached)"
                    && set_on != branch
                {
                    extra.push_str(&format!(" (set on {set_on})"));
                }
                println!(
                    "{} {} \"{}\" {url}{extra}",
                    l["kind"].as_str().unwrap_or_default(),
                    l["tone"].as_str().unwrap_or_default(),
                    l["label"].as_str().unwrap_or_default(),
                );
            }
        }
    }
}

/// What else the top bar shows, as one line, plus the pull-request rule when a
/// project badge could be mistaken for the agent's own link. Empty when nothing.
fn also_shown_lines(also: &serde_json::Value) -> Vec<String> {
    let mut parts = Vec::new();
    let mut badge = false;
    // A badge's text is a project command's output, often fetched from the
    // network; it reaches the agent's terminal here, so it is printed as plain text.
    let plain = |v: &serde_json::Value| -> String {
        v.as_str()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .collect()
    };
    for e in also["project_entries"].as_array().into_iter().flatten() {
        let label = plain(&e["label"]);
        let kind = e["type"].as_str().unwrap_or("entry");
        badge |= kind == "status";
        parts.push(
            match (e["shown"]["text"].as_str(), e["shown"]["href"].as_str()) {
                (Some(_), Some(_)) => format!(
                    "{kind} \"{label}\" = \"{}\" {}",
                    plain(&e["shown"]["text"]),
                    plain(&e["shown"]["href"])
                ),
                (Some(_), None) => {
                    format!("{kind} \"{label}\" = \"{}\"", plain(&e["shown"]["text"]))
                }
                _ => format!("{kind} \"{label}\""),
            },
        );
    }
    for url in also["run_urls"].as_array().into_iter().flatten() {
        parts.push(format!("run {}", url.as_str().unwrap_or_default()));
    }
    let mut lines = Vec::new();
    if !parts.is_empty() {
        lines.push(format!("Top bar already shows: {}", parts.join("; ")));
    }
    if badge {
        lines.push("Still pin your PRs: the rail shows only pinned ones.".to_owned());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: LinkCommand,
    }

    #[test]
    fn rm_takes_a_link_or_all_but_not_both() {
        assert!(Cli::try_parse_from(["veld", "rm"]).is_err());
        assert!(Cli::try_parse_from(["veld", "rm", "x", "--all"]).is_err());
        assert!(Cli::try_parse_from(["veld", "rm", "--all"]).is_ok());
        assert!(Cli::try_parse_from(["veld", "rm", "preview"]).is_ok());
    }

    #[test]
    fn refusals_map_to_the_documented_exit_codes() {
        assert_eq!(exit_for(Some("bad_link")), 2);
        assert_eq!(exit_for(Some("links_full")), 2);
        assert_eq!(exit_for(Some("already_shown")), 2);
        assert_eq!(exit_for(Some("not_in_worktree")), 3);
        assert_eq!(exit_for(Some("no_such_link")), 4);
        assert_eq!(exit_for(None), 1);
    }
}
