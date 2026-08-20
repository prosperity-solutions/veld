//! `veld open-url` — open a web page in the Veld window that owns this terminal.
//!
//! Three callers, one code path:
//!
//! 1. A **person or an agent** typing `veld open-url https://…` in a Veld
//!    terminal.
//! 2. Anything honouring **`$BROWSER`**, which a Veld terminal points at the
//!    generated `veld-open` shim: Claude Code, `gh`, `git web--browse`, Python's
//!    `webbrowser`, vite, next.
//! 3. The **`open` / `xdg-open` shims**, which for zsh are on `PATH`
//!    automatically — the daemon arranges that from a `.zshenv` of its own (see
//!    `veld-daemon/src/pty/shims.rs`) — and which a user of another shell reaches by
//!    putting `$VELD_SHIM_DIR` on `PATH` themselves.
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
/// resolves the real tool — but only the directory named by `$VELD_SHIM_DIR`, so a
/// shim reachable on `PATH` while that variable is unset or stale (a snapshotted
/// `PATH`, `env -u VELD_SHIM_DIR`, a hand-written entry) resolves *itself*.
///
/// This is the second layer, and it has to be a **counter**: the first version set
/// the variable and then took the passthrough branch, which re-resolves the opener
/// and `exec`s it — with the variable already set, so the guard could not fire
/// again. A review measured that at ~3,800 `exec`s in five seconds, stopped only by
/// an rlimit. A guard whose failure mode is a fork bomb has to count.
const DEPTH_VAR: &str = "VELD_OPEN_URL_DEPTH";

/// How many nested `veld open-url` invocations to tolerate before refusing.
///
/// One: a shim that reaches this command, whose passthrough reaches it again, is
/// already a loop. Anything legitimate is depth zero.
const MAX_DEPTH: u32 = 1;

/// This invocation's nesting depth, from the environment. Unparseable or absent is
/// zero — a broken value must not read as "infinitely deep" and refuse a legitimate
/// open, nor as "reset" and license another lap.
fn depth_from_env(raw: Option<String>) -> u32 {
    raw.and_then(|v| v.parse::<u32>().ok()).unwrap_or(0)
}

/// Whether this invocation has to stop rather than pass through again.
fn too_deep(depth: u32) -> bool {
    depth > MAX_DEPTH
}

/// What the child gets. **Incremented, never set:** a flat value is what let the
/// first version's guard latch on and then never fire again.
fn child_depth(depth: u32) -> u32 {
    depth.saturating_add(1)
}

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

    // Already inside one of these. One level is handed straight to the real tool
    // without looking at the arguments again; a second means the passthrough came
    // back here, which is a loop rather than a fallback, so it stops.
    let depth = depth_from_env(std::env::var(DEPTH_VAR).ok());
    if too_deep(depth) {
        eprintln!(
            "veld: refusing to open a URL {depth} levels deep — the shim directory is \
             on PATH but $VELD_SHIM_DIR does not name it, so the shim is resolving \
             itself. Unset the stale PATH entry or restart the terminal."
        );
        return 127;
    }
    if depth > 0 {
        return passthrough(
            tool,
            &args,
            depth,
            Some("recursion guard"),
            Fallback::Browser,
        );
    }

    let url = match veld_core::opener::decide(tool, &args) {
        Decision::Url(url) => url,
        // A bare argument that might name a viewable file. Answering that is the
        // filesystem's job and then the daemon's — see `Decision::Path`.
        Decision::Path(raw) => {
            return open_path(tool, &args, depth, &raw, session.clone()).await;
        }
        // Not a web page. This is the common case for the `open` shim and it must
        // be silent — a note on stderr for every `open .` would be noise in
        // somebody's shell.
        Decision::Passthrough => return passthrough(tool, &args, depth, None, Fallback::Browser),
    };

    let Some(session) = session.or_else(session_id) else {
        // Run outside a Veld terminal — a plain shell, or a tool that scrubbed its
        // children's environment. There is no window to attribute the URL to, so
        // the system browser is the only honest answer.
        return passthrough(
            tool,
            &args,
            depth,
            Some("not inside a Veld terminal"),
            Fallback::Browser,
        );
    };

    match ask_daemon(&session, &url).await {
        Ok(Answer::Pane) => 0,
        Ok(Answer::System(reason)) => {
            passthrough(tool, &args, depth, reason.as_deref(), Fallback::Browser)
        }
        Err(e) => passthrough(tool, &args, depth, Some(&e), Fallback::Browser),
    }
}

/// A lone bare argument: show it in a pane if it is a file Veld can show.
///
/// **Silent in every failure path**, which is the difference from the URL case above.
/// The shim asks about every bare word somebody types — `open .`, `open notes.zip`,
/// `open some-directory` — so anything printed here is printed constantly. Only the
/// daemon can say a sentence is warranted, and it does that by attaching a reason;
/// see `open_file` in `veld-daemon/src/pty.rs`.
///
/// The `stat` happens here rather than in the daemon because this process is the one
/// standing in the terminal's working directory: `./deck.html` is meaningless by the
/// time the request arrives, so what travels is always an absolute, canonical path.
async fn open_path(
    tool: Tool,
    args: &[String],
    depth: u32,
    raw: &str,
    session: Option<String>,
) -> i32 {
    let Some(path) = canonical_file(raw) else {
        return passthrough(tool, args, depth, None, Fallback::Opener);
    };
    // Filtered here, before the round trip, for the kinds no setting could ever make
    // viewable. `servable_type` is a *capability* question — "is there a content type
    // for this at all" — not policy, so answering it locally does not split the policy
    // owner: the daemon still decides everything a setting can change.
    //
    // Without this, `open archive.zip` and `open installer.dmg` each cost a POST with a
    // five-second timeout before falling through, on a command people run dozens of
    // times a day.
    if veld_core::files::servable_type(&path).is_none() {
        return passthrough(tool, args, depth, None, Fallback::Opener);
    }
    // `--session` wins over the environment, exactly as it does for a URL: it is how
    // the flag is testable and how a caller outside a terminal names one.
    let Some(session) = session.or_else(session_id) else {
        return passthrough(tool, args, depth, None, Fallback::Opener);
    };
    match ask_daemon_file(&session, &path).await {
        Ok(Answer::Pane) => 0,
        Ok(Answer::System(reason)) => {
            passthrough(tool, args, depth, reason.as_deref(), Fallback::Opener)
        }
        // A daemon that is down, wedged or answering nonsense is not something to
        // narrate on a command people run dozens of times a day. The file opens the
        // way it did before veld existed.
        Err(_) => passthrough(tool, args, depth, None, Fallback::Opener),
    }
}

/// The absolute, canonical path of `raw`, if it names a regular file.
///
/// A directory is deliberately `None`: `open .` is the single most common invocation
/// of this shim and it must reach the real tool untouched.
fn canonical_file(raw: &str) -> Option<String> {
    let path = std::fs::canonicalize(raw).ok()?;
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

/// The terminal session this process is running inside, if any.
fn session_id() -> Option<String> {
    std::env::var("VELD_PTY_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
}

enum Answer {
    Pane,
    /// Open it yourself. The reason is `Some` only when there is something worth
    /// telling the user — see [`passthrough`], which prints it, and `open_path`,
    /// which is silent by default.
    System(Option<String>),
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
    let answer = ask(
        session,
        "open-url",
        serde_json::json!({ "url": url }),
        std::time::Duration::from_secs(5),
    )
    .await?;
    // A URL always gets a sentence. The daemon attaches one to every `system`
    // answer here, and the fallback exists so an older daemon that did not cannot
    // produce a silent redirect to the system browser.
    Ok(match answer {
        Answer::System(reason) => Answer::System(Some(
            reason.unwrap_or_else(|| "not routed to a pane".to_owned()),
        )),
        pane => pane,
    })
}

/// Ask the daemon to show a local file. Reasons are passed through as they arrive —
/// most of them are absent on purpose. See [`open_path`].
async fn ask_daemon_file(session: &str, path: &str) -> Result<Answer, String> {
    ask(
        session,
        "open-file",
        serde_json::json!({ "path": path }),
        // A *second*, not the URL path's five. The two differ in what waiting buys: a
        // URL that falls through opens somewhere the user did not want, so it is worth
        // waiting for the daemon's answer. A file that falls through opens in the app
        // that kind is registered to, which is what would have happened before veld
        // existed — so the cost of waiting is pure, and it is paid on every `open
        // README.md` and `open app.js`, since those are servable (so the local
        // pre-filter passes them) but not viewable under the shipped defaults.
        std::time::Duration::from_secs(1),
    )
    .await
}

/// One request, one reply shape, for both of the above.
///
/// Split out when the file route arrived: two copies of the timeout, the CSRF header
/// and the error wording would be two things to keep in step, and the reply type is
/// identical by design.
async fn ask(
    session: &str,
    action: &str,
    body: serde_json::Value,
    timeout: std::time::Duration,
) -> Result<Answer, String> {
    let base = veld_core::instance::daemon_base();
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("could not reach the daemon: {e}"))?;
    let resp = client
        .post(format!("{base}/api/pty/sessions/{session}/{action}"))
        .header("X-Veld-Request", "1")
        .json(&body)
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
    Ok(match parsed.target {
        veld_core::ide::UrlTarget::Pane => Answer::Pane,
        veld_core::ide::UrlTarget::System => Answer::System(parsed.reason),
    })
}

/// The argv the real tool should receive: the original, minus the shims' separator.
///
/// A lone leading `--` is how the generated scripts pass an argument list that may
/// begin with a dash; it is not an argument. Split out of [`passthrough`] so it can
/// be tested — the version inside `exec` could only be checked by running it.
fn real_argv(args: &[String]) -> &[String] {
    match args.split_first() {
        Some((first, rest)) if first == "--" => rest,
        _ => args,
    }
}

/// What the child's `$BROWSER` must be.
///
/// Never the shim: a fall-through opener that consults `$BROWSER` (Python's
/// `webbrowser`, a desktop helper) would come straight back here, which is a loop
/// rather than a fallback. The user's own value is restored when a terminal captured
/// one — at spawn from the daemon's environment, or by the `veld_browser` hook when
/// their rc file set it.
#[derive(Debug, PartialEq, Eq)]
enum ChildBrowser {
    Restore(std::ffi::OsString),
    Remove,
}

fn child_browser(original: Option<std::ffi::OsString>) -> ChildBrowser {
    match original.filter(|v| !v.is_empty()) {
        Some(v) => ChildBrowser::Restore(v),
        None => ChildBrowser::Remove,
    }
}

/// What the real tool will do with the argument, for the one sentence this prints.
///
/// A URL handed to `/usr/bin/open` opens a browser; a file handed to the same binary
/// opens whatever that kind is registered to — an editor for `.md`, Preview for a PDF.
/// One noun for both was wrong for whichever path it was not written for, and it is a
/// user-facing string, so it gets a type rather than a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    Browser,
    Opener,
}

impl Fallback {
    fn noun(self) -> &'static str {
        match self {
            Self::Browser => "system browser",
            Self::Opener => "system opener",
        }
    }
}

/// Hand the original arguments to the real tool, replacing this process.
///
/// Returns only on failure — on success this process *is* the real tool.
fn passthrough(
    tool: Tool,
    args: &[String],
    depth: u32,
    reason: Option<&str>,
    fallback: Fallback,
) -> i32 {
    let args = real_argv(args);
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
        eprintln!("veld: opening in the {} ({reason})", fallback.noun());
    }

    let mut cmd = std::process::Command::new(&real);
    cmd.args(args);
    // Incremented, not set: see `DEPTH_VAR`. A flat "1" made the guard unable to
    // fire on the invocation it was guarding against.
    cmd.env(DEPTH_VAR, child_depth(depth).to_string());
    // The child must not inherit a `$BROWSER` pointing back at the shim: a
    // fall-through opener that consults it (Python's `webbrowser`, `gio`) would
    // come straight back here. The user's own value is restored when the terminal
    // saved one.
    match child_browser(std::env::var_os("VELD_BROWSER_ORIGINAL")) {
        ChildBrowser::Restore(original) => cmd.env("BROWSER", original),
        ChildBrowser::Remove => cmd.env_remove("BROWSER"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_shims_separator_is_not_an_argument() {
        // The real tool must receive exactly what it was called with. A `--` that
        // reached `/usr/bin/open` would make `open -- .` fail on an argument the user
        // never typed.
        assert_eq!(
            real_argv(&argv(&["--", "-a", "Safari"])),
            argv(&["-a", "Safari"])
        );
        assert_eq!(real_argv(&argv(&["."])), argv(&["."]));
        assert_eq!(real_argv(&argv(&[])), Vec::<String>::new());
        // Only a *leading* one, and only one: `open -- --` passes the second through,
        // and a `--` in the middle belongs to the tool.
        assert_eq!(real_argv(&argv(&["--", "--"])), argv(&["--"]));
        assert_eq!(real_argv(&argv(&["-a", "--"])), argv(&["-a", "--"]));
    }

    #[test]
    fn the_child_never_inherits_a_browser_pointing_back_here() {
        use std::ffi::OsString;
        // Restored when a terminal captured the user's own browser…
        assert_eq!(
            child_browser(Some(OsString::from("firefox"))),
            ChildBrowser::Restore(OsString::from("firefox"))
        );
        // …removed otherwise, rather than left pointing at the shim. An empty value
        // counts as none: exporting `BROWSER=` is not a browser, and `Restore("")`
        // would hand a child an empty command.
        assert_eq!(child_browser(None), ChildBrowser::Remove);
        assert_eq!(child_browser(Some(OsString::new())), ChildBrowser::Remove);
    }

    #[test]
    fn the_depth_guard_terminates_a_shim_that_resolves_itself() {
        // The bound this protects: a shim reachable on PATH while `$VELD_SHIM_DIR`
        // does not name it resolves *itself*. The first version set the variable to a
        // flat "1" and then took a branch that re-resolved and re-exec'd, so the guard
        // could never fire again — measured at ~3,800 execs in five seconds, stopped
        // only by an rlimit.
        //
        // Walk the loop it could not stop, and assert it now terminates: each lap
        // hands the child `child_depth`, and the child re-reads it as its own depth.
        let mut depth = depth_from_env(None);
        let mut laps = 0;
        while !too_deep(depth) {
            depth = depth_from_env(Some(child_depth(depth).to_string()));
            laps += 1;
            assert!(
                laps < 10,
                "the guard does not terminate: depth stuck at {depth}"
            );
        }
        assert_eq!(
            laps, 2,
            "one passthrough is allowed; the one after it is a loop"
        );

        // A garbled value is zero, not "infinitely deep" (which would refuse a
        // legitimate open) — and not a licence for another lap either, since the
        // child's value is derived from it by increment.
        assert_eq!(depth_from_env(Some("banana".into())), 0);
        assert_eq!(depth_from_env(Some(String::new())), 0);
        assert!(!too_deep(0) && !too_deep(1) && too_deep(2));
        // No wrap at the ceiling of the type: saturating, so a hostile value cannot
        // roll the counter back to zero and reopen the loop.
        assert_eq!(child_depth(u32::MAX), u32::MAX);
        assert!(too_deep(u32::MAX));
    }
}
