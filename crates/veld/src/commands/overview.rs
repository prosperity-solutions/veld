//! Bare `veld`: the OS at a glance — which services are up, what is running,
//! which agents are working and which need the human.
//!
//! **Passive.** Every other face reaches the daemon through `EngineClient`, which
//! starts one when none answers; a look at the machine must not change it. So a
//! daemon that is down is reported as down, and the runs and agents it would
//! have listed are reported as unknown rather than as none.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use veld_engine::api::{Attention, OverviewAgent, RunningRun};

use crate::output;

/// The parts `veld --help` lists. A test pins this against the clap tree (both
/// ways: `help_lists_parts_only` in `main.rs`), so the overview cannot advertise a
/// part that does not exist or leave one out.
pub const PARTS: &[&str] = &["orchestrator", "ide", "agent", "network", "self"];

#[derive(Serialize)]
struct Overview {
    version: &'static str,
    daemon: Daemon,
    helper: Helper,
    caddy: Up,
    /// `None` when the daemon cannot say: unknown, not empty.
    runs: Option<Vec<RunningRun>>,
    /// `None` when the daemon cannot say: unknown, not empty.
    agents: Option<Vec<OverviewAgent>>,
    parts: &'static [&'static str],
}

#[derive(Serialize)]
struct Daemon {
    up: bool,
    version: Option<String>,
    /// When the daemon started (RFC 3339). Agent state is kept in its memory, so
    /// `agents` lists what was reported since then — an agent that was already
    /// blocked across a restart reports nothing until the human answers it.
    started_at: Option<String>,
    /// Why `runs` and `agents` are unknown although the daemon is up.
    problem: Option<String>,
}

#[derive(Serialize)]
struct Helper {
    up: bool,
    /// `privileged`, `unprivileged` or `auto`, from `~/.veld/setup.json`.
    mode: Option<String>,
}

#[derive(Serialize)]
struct Up {
    up: bool,
}

pub async fn run(json: bool) -> i32 {
    let (answer, setup, caddy_up) = tokio::join!(
        ask_the_daemon(),
        veld_core::setup::check_setup(),
        veld_core::setup::is_caddy_running(),
    );
    let down = Daemon {
        up: false,
        version: None,
        started_at: None,
        problem: None,
    };
    let (daemon, runs, agents) = match answer {
        Answer::Overview(o) => (
            Daemon {
                up: true,
                version: Some(o.version),
                started_at: Some(o.started_at),
                problem: None,
            },
            Some(o.runs),
            Some(o.agents),
        ),
        Answer::Problem { version, problem } => (
            Daemon {
                up: true,
                version,
                problem: Some(problem),
                ..down
            },
            None,
            None,
        ),
        Answer::Down => (down, None, None),
    };
    let overview = Overview {
        version: env!("CARGO_PKG_VERSION"),
        daemon,
        helper: Helper {
            up: setup.helper_running,
            mode: veld_core::setup::read_setup_mode(),
        },
        caddy: Up { up: caddy_up },
        runs,
        agents,
        parts: PARTS,
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&overview).unwrap_or_default()
        );
    } else {
        print_human(&overview);
    }
    0
}

enum Answer {
    Overview(veld_engine::api::Overview),
    /// Up, but no overview: too old to have the route (a 404 — the normal state for
    /// the minutes between installing a CLI and restarting its daemon), or the route
    /// failed. Either way "down" would send somebody to fix a daemon that is fine,
    /// and "older" would send them to update one that is current, so the reason is
    /// carried as measured.
    Problem {
        version: Option<String>,
        problem: String,
    },
    Down,
}

#[derive(Deserialize)]
struct Health {
    version: String,
}

/// No `EngineClient`: it starts a daemon that is not running (see the module
/// doc). Short GETs only: the overview, and when that is refused, the health
/// route every daemon has, to tell "up" from "down" and say which version.
async fn ask_the_daemon() -> Answer {
    let Ok(http) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return Answer::Down;
    };
    let base = veld_core::instance::daemon_base();
    let problem = match http.get(format!("{base}/api/engine/overview")).send().await {
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
            "it is older than this CLI and has no overview".to_owned()
        }
        Ok(resp) if !resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let reason = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
                .unwrap_or_else(|| status.to_string());
            format!("its overview failed: {reason}")
        }
        Ok(resp) => match resp.json::<veld_engine::api::Overview>().await {
            Ok(o) => return Answer::Overview(o),
            Err(e) => format!("its overview could not be read: {e}"),
        },
        Err(e) if e.is_timeout() => "its overview timed out".to_owned(),
        Err(_) => return Answer::Down,
    };
    let version = match http.get(format!("{base}/api/health")).send().await {
        Ok(resp) => resp.json::<Health>().await.ok().map(|h| h.version),
        Err(_) => None,
    };
    Answer::Problem { version, problem }
}

fn dot(up: bool) -> String {
    if up {
        output::green("●")
    } else {
        output::red("●")
    }
}

fn up_word(up: bool) -> &'static str {
    if up { "up" } else { "down" }
}

/// How an agent's [`Attention`] reads to the human. The daemon decides which one
/// it is; this only words it.
fn attention_label(a: Attention) -> &'static str {
    match a {
        Attention::NeedsYou => "needs you",
        Attention::YourTurn => "your turn",
        Attention::Working => "working",
        Attention::Quiet => "quiet",
        Attention::Other => "other",
    }
}

fn print_human(o: &Overview) {
    println!("{} {}", output::bold("Veld"), o.version);
    println!();

    let helper_mode = o
        .helper
        .mode
        .as_deref()
        .map(|m| format!(" ({m})"))
        .unwrap_or_default();
    let daemon_version = match o.daemon.version.as_deref() {
        Some(v) if v != o.version => format!(" {}", output::dim(&format!("({v})"))),
        _ => String::new(),
    };
    println!(
        "  {}  {} {}{}    helper {} {}{}    caddy {} {}",
        output::pad_right("daemon", 8),
        dot(o.daemon.up),
        up_word(o.daemon.up),
        daemon_version,
        dot(o.helper.up),
        up_word(o.helper.up),
        helper_mode,
        dot(o.caddy.up),
        up_word(o.caddy.up),
    );

    let unknown = || {
        output::dim(&match &o.daemon.problem {
            Some(problem) => format!("unknown — the daemon is up, but {problem}."),
            None => "unknown — the daemon is down. `veld self doctor` says why.".to_owned(),
        })
    };

    match &o.runs {
        None => println!("  {}  {}", output::pad_right("runs", 8), unknown()),
        Some(runs) if runs.is_empty() => {
            println!(
                "  {}  {}",
                output::pad_right("runs", 8),
                output::dim("none")
            )
        }
        Some(runs) => {
            let names: Vec<String> = runs
                .iter()
                .map(|r| format!("{}:{}", r.project_name, r.name))
                .collect();
            println!(
                "  {}  {} running    {}",
                output::pad_right("runs", 8),
                runs.len(),
                output::dim(&names.join(" · "))
            );
        }
    }

    match &o.agents {
        None => println!(
            "  {}  {}",
            output::pad_right("agents", 8),
            output::dim("unknown")
        ),
        Some(agents) if agents.is_empty() => println!(
            "  {}  {}",
            output::pad_right("agents", 8),
            output::dim(&format!(
                "none reported since the daemon started{}",
                started_at_local(o)
            ))
        ),
        Some(agents) => {
            let count = |a: Attention| agents.iter().filter(|x| x.attention == a).count();
            let mut summary = Vec::new();
            // Every kind that is present, so the summary adds up to the rows below.
            for a in [
                Attention::NeedsYou,
                Attention::YourTurn,
                Attention::Working,
                Attention::Quiet,
                Attention::Other,
            ] {
                let n = count(a);
                if n > 0 {
                    summary.push(format!("{n} {}", attention_label(a)));
                }
            }
            println!(
                "  {}  {}",
                output::pad_right("agents", 8),
                summary.join(", ")
            );
            let width = agents
                .iter()
                .map(|a| output::display_width(&agent_name(a)))
                .max()
                .unwrap_or(0);
            // Already in attention order: the daemon sorts.
            for a in agents {
                let label = attention_label(a.attention);
                let label = match a.attention {
                    Attention::NeedsYou => output::yellow(label),
                    Attention::YourTurn => output::cyan(label),
                    Attention::Working => label.to_owned(),
                    Attention::Quiet | Attention::Other => output::dim(label),
                };
                println!(
                    "  {}  {} {}  {}",
                    " ".repeat(8),
                    dot(a.attention != Attention::NeedsYou),
                    output::pad_right(&agent_name(a), width),
                    label
                );
            }
        }
    }

    println!();
    println!(
        "  Parts:  {}",
        PARTS
            .join(" · ")
            .replace("orchestrator", "orchestrator (orc)")
    );
    println!(
        "  More:   {}",
        output::dim("veld <part> --help · veld --help-all")
    );
}

/// " (2026-10-15 09:41)" in local time, or nothing when the daemon did not say.
fn started_at_local(o: &Overview) -> String {
    o.daemon
        .started_at
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| {
            format!(
                " ({})",
                t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
            )
        })
        .unwrap_or_default()
}

/// The worktree's rail title when it has one, else the project, else the
/// session — always something a human can match to a pane.
fn agent_name(a: &OverviewAgent) -> String {
    a.worktree_name
        .clone()
        .or_else(|| a.project_name.clone())
        .unwrap_or_else(|| a.session_id.clone())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    /// The overview names parts; every one must be a real, visible subcommand.
    #[test]
    fn the_overview_names_the_parts_clap_has() {
        let cli = crate::Cli::command();
        let visible: Vec<&str> = cli
            .get_subcommands()
            .filter(|s| !s.is_hide_set())
            .map(|s| s.get_name())
            .collect();
        for part in super::PARTS {
            assert!(visible.contains(part), "{part} is not a visible subcommand");
        }
    }
}
