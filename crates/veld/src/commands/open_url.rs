//! `veld open-url` — open a web page in the Veld window that owns this terminal.
//!
//! Three callers, one code path:
//!
//! 1. A **person or an agent** typing `veld open-url https://…` in a Veld
//!    terminal.
//! 2. Anything honouring **`$BROWSER`**, which a Veld terminal points at the
//!    generated `veld-open` shim: Claude Code, `gh`, `git web--browse`, Python's
//!    `webbrowser`, vite, next.
//! 3. The **`open` / `xdg-open` shims**, for a user who has put `$VELD_SHIM_DIR`
//!    on `PATH` (see `veld_core::opener` for why that is opt-in rather than
//!    automatic).
//!
//! # Falling through is the default, not the error path
//!
//! Every branch that is not "a single web page, in a terminal, that the daemon
//! agreed to show in a pane" ends in `exec`ing the real tool with the **original**
//! argv. `open .`, `open -a Safari url`, `xdg-open report.pdf`, a URL on the exempt
//! list, a daemon that is not running, a terminal nobody is looking at — all of
//! them behave exactly as they did before veld was in the picture. A wrapper around
//! a command people use dozens of times a day has no other acceptable failure mode.
//!
//! `exec` rather than spawn-and-wait: the real tool inherits this process's pid,
//! stdio, exit status and signals, so nothing can tell the difference. It also
//! means there is no veld process left waiting on a browser that stays open for
//! hours.

use veld_core::opener::{Decision, Tool, real_opener};

/// Guard against a shim that ends up finding itself.
///
/// `veld_core::opener::real_opener` already excludes the shim directory when it
/// resolves the real tool, so this is the second layer — and it is worth having,
/// because the failure mode of the first layer being wrong is not a wrong answer
/// but a fork bomb on a developer's machine.
const DEPTH_VAR: &str = "VELD_OPEN_URL_DEPTH";

/// `veld open-url [--tool <t>] [--session <id>] -- <args>...`
pub async fn run(tool: Option<String>, session: Option<String>, args: Vec<String>) -> i32 {
    let tool = match tool.as_deref() {
        None => Tool::Browser,
        Some(flag) => match Tool::parse(flag) {
            Some(t) => t,
            None => {
                eprintln!("veld: unknown --tool {flag:?}");
                return 2;
            }
        },
    };

    // Already inside one of these: hand the arguments straight to the real tool
    // and do not look at them again.
    if std::env::var_os(DEPTH_VAR).is_some() {
        return passthrough(tool, &args, Some("recursion guard"));
    }

    let url = match veld_core::opener::decide(tool, &args) {
        Decision::Url(url) => url,
        // Not a web page. This is the common case for the `open` shim and it must
        // be silent — a note on stderr for every `open .` would be noise in
        // somebody's shell.
        Decision::Passthrough => return passthrough(tool, &args, None),
    };

    let Some(session) = session.or_else(|| {
        std::env::var("VELD_PTY_SESSION")
            .ok()
            .filter(|s| !s.is_empty())
    }) else {
        // Run outside a Veld terminal — a plain shell, or a tool that scrubbed its
        // children's environment. There is no window to attribute the URL to, so
        // the system browser is the only honest answer.
        return passthrough(tool, &args, Some("not inside a Veld terminal"));
    };

    match ask_daemon(&session, &url).await {
        Ok(Answer::Pane) => 0,
        Ok(Answer::System(reason)) => passthrough(tool, &args, Some(&reason)),
        Err(e) => passthrough(tool, &args, Some(&e)),
    }
}

enum Answer {
    Pane,
    System(String),
}

#[derive(serde::Deserialize)]
struct OpenUrlResponse {
    target: veld_core::ide::UrlTarget,
    reason: Option<String>,
}

/// Ask this instance's daemon where the URL should open.
///
/// The daemon owns the decision because the exempt list is half a global setting
/// and half the project's `veld.json` — see `veld_core::ide::route_url`. A short
/// timeout, because a wedged daemon must not hold up a browser: the fallback is the
/// system browser, which is where the URL would have gone anyway.
async fn ask_daemon(session: &str, url: &str) -> Result<Answer, String> {
    let base = veld_core::instance::daemon_base();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("could not reach the daemon: {e}"))?;
    let resp = client
        .post(format!("{base}/api/pty/sessions/{session}/open-url"))
        .header("X-Veld-Request", "1")
        .json(&serde_json::json!({ "url": url }))
        .send()
        .await
        .map_err(|e| format!("could not reach the daemon: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("the daemon refused ({status}): {}", body.trim()));
    }
    let parsed: OpenUrlResponse = resp
        .json()
        .await
        .map_err(|e| format!("could not read the daemon's answer: {e}"))?;
    match parsed.target {
        veld_core::ide::UrlTarget::Pane => Ok(Answer::Pane),
        veld_core::ide::UrlTarget::System => Ok(Answer::System(
            parsed
                .reason
                .unwrap_or_else(|| "not routed to a pane".to_owned()),
        )),
    }
}

/// Hand the original arguments to the real tool, replacing this process.
///
/// Returns only on failure — on success this process *is* the real tool.
fn passthrough(tool: Tool, args: &[String], reason: Option<&str>) -> i32 {
    // A lone `--` is the shims' separator, not an argument.
    let args: &[String] = match args.split_first() {
        Some((first, rest)) if first == "--" => rest,
        _ => args,
    };
    let shim_dir = std::env::var_os("VELD_SHIM_DIR").map(std::path::PathBuf::from);
    let Some(real) = real_opener(tool, shim_dir.as_deref()) else {
        eprintln!(
            "veld: no system opener found for {} — nothing to fall back to",
            tool.shim_name()
        );
        return 127;
    };
    if let Some(reason) = reason {
        // On stderr, and only when there is something to explain: "why did that
        // open in Safari" is otherwise unanswerable. Never on stdout — a shim runs
        // inside other tools' pipelines (AGENTS.md).
        eprintln!("veld: opening in the system browser ({reason})");
    }

    let mut cmd = std::process::Command::new(&real);
    cmd.args(args);
    cmd.env(DEPTH_VAR, "1");
    // The child must not inherit a `$BROWSER` pointing back at the shim: a
    // fall-through opener that consults it (Python's `webbrowser`, `gio`) would
    // come straight back here. The user's own value is restored when the terminal
    // saved one.
    match std::env::var_os("VELD_BROWSER_ORIGINAL") {
        Some(original) => cmd.env("BROWSER", original),
        None => cmd.env_remove("BROWSER"),
    };

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only returns on failure.
        let e = cmd.exec();
        eprintln!("veld: could not run {}: {e}", real.display());
        127
    }
    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("veld: could not run {}: {e}", real.display());
                127
            }
        }
    }
}
