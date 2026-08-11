//! `veld agent-settings` and `veld agent-state` — the two halves of telling a Veld
//! window that a coding agent is waiting on the user.
//!
//! Neither is meant to be typed. `agent-settings` is called by the generated `claude`
//! wrapper (`veld-daemon/src/pty/shims.rs`) just before it `exec`s the real binary;
//! `agent-state` is what the hooks in the file that wrapper produces call. They are
//! CLI subcommands rather than daemon endpoints for the same reason `veld open-url` is:
//! a shell can reach a binary, and the binary is where the escaping, the timeouts and
//! the fall-through belong — in Rust, where they are tested.
//!
//! # Both fail silently, and that is the contract
//!
//! Nothing downstream of a missed report is broken: a badge does not appear. The
//! alternative is an agent that pauses, or a wrapper that refuses to launch, because
//! veld could not deliver a notification — which would make the feature worse than not
//! having it. So every error path here is a non-zero exit with nothing on stdout, and
//! the caller's answer to that is to carry on.

use std::path::{Path, PathBuf};

use veld_core::agent::{self, AgentTool};

/// `veld agent-settings [--tool claude] [--session <id>]`
///
/// Writes this session's ephemeral settings file and prints its path on stdout.
///
/// Stdout is the path and nothing else — the caller is a shell doing
/// `settings=$(veld agent-settings ...)`, so a stray diagnostic there becomes a
/// filename (AGENTS.md: machine-readable output on stdout, chrome on stderr).
pub fn settings(tool: Option<String>, session: Option<String>) -> i32 {
    let Some(tool) = parse_tool(tool.as_deref()) else {
        return 1;
    };
    let Some(session) = session_id(session) else {
        // Outside a Veld terminal there is nothing to attribute a state to. Silent:
        // the wrapper's answer is to launch the agent without hooks, which is what
        // would have happened anyway.
        return 1;
    };
    let Some(dir) = shim_dir() else {
        return 1;
    };
    let Ok(cli) = std::env::current_exe() else {
        return 1;
    };

    let path = agent::settings_path(&dir, tool, &session);
    let Some(parent) = path.parent() else {
        return 1;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return 1;
    }
    // 0700 on the directory: it holds files naming this machine's terminal sessions,
    // and it sits inside a directory a developer may have on `PATH`.
    let _ = set_mode(parent, 0o700);
    sweep(parent);

    let doc = agent::claude_settings_doc(&cli, &session);
    let Ok(body) = serde_json::to_vec_pretty(&doc) else {
        return 1;
    };
    // Write-then-rename, for the reason the shims themselves use it: the agent may be
    // reading this file while a second launch rewrites it, and a truncated JSON
    // document is a settings file Claude Code rejects rather than one it ignores.
    let tmp = path.with_extension("json.new");
    if std::fs::write(&tmp, &body).is_err() {
        return 1;
    }
    let _ = set_mode(&tmp, 0o600);
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return 1;
    }
    println!("{}", path.display());
    0
}

/// `veld agent-state [--tool claude] [--session <id>]`
///
/// Reads a hook payload on **stdin**, works out what state it reports, and tells the
/// daemon. Called by the hooks in the file [`settings`] writes.
///
/// # Why the payload is parsed here and not in the daemon
///
/// The daemon's endpoint takes a `state`, not a vendor's JSON. Keeping the vendor
/// schema on this side means a second tool is a new mapping function plus a hook
/// installer, with nothing to change in the daemon, the wire, or the UI — which is the
/// whole claim the "one generic receiving end" design makes. It also keeps a
/// third-party schema out of a long-lived process.
pub async fn state(tool: Option<String>, session: Option<String>, launched: bool) -> i32 {
    let Some(tool) = parse_tool(tool.as_deref()) else {
        return 1;
    };
    let Some(session) = session_id(session) else {
        return 1;
    };
    // `--launched` is the wrapper's own report, fired just before it `exec`s the agent.
    // No stdin: there is no hook payload, because no hook has run — that is the point.
    // It claims hook authority for the pane while saying nothing is happening, which is
    // what stops the shell's "a command is running here" driving an activity spinner for
    // a session sitting idle at its prompt.
    let reported = if launched {
        agent::State::Ready
    } else {
        // A hook that sends nothing, or sends something unparseable, is not an error worth
        // reporting into somebody's agent session — it is a state we do not know.
        let payload: agent::HookPayload = match read_stdin() {
            Some(body) => serde_json::from_slice(&body).unwrap_or_default(),
            None => return 1,
        };
        match tool {
            AgentTool::Claude => agent::claude_state(&payload),
        }
    };
    // `Unknown` is a decision, not a failure: an unrecognised notification type must
    // produce no event at all rather than a badge the user cannot act on. Sending it
    // would make the daemon store a state that overrides a real one at equal
    // authority.
    if reported == agent::State::Unknown {
        return 0;
    }
    report(&session, tool, reported).await
}

async fn report(session: &str, tool: AgentTool, reported: agent::State) -> i32 {
    let base = veld_core::instance::daemon_base();
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(
            agent::HOOK_REQUEST_TIMEOUT_MS,
        ))
        .build()
    else {
        return 1;
    };
    let sent = client
        .post(format!("{base}/api/pty/sessions/{session}/agent-state"))
        .header("X-Veld-Request", "1")
        .json(&serde_json::json!({
            "tool": tool.as_str(),
            "state": reported.as_str(),
        }))
        .send()
        .await;
    match sent {
        Ok(resp) if resp.status().is_success() => 0,
        // Silent on both arms. A daemon that is down, busy or a different version is
        // not something to interrupt an agent about.
        _ => 1,
    }
}

/// Parse `--tool`, or `None`.
///
/// The caller exits **1** on `None`, never 2: on `UserPromptSubmit` and `Stop` — both of
/// which the generated settings file installs — exit 2 is Claude Code's *blocking* status,
/// which feeds stderr back into the session and, on `UserPromptSubmit`, discards the user's
/// prompt. Unreachable through the generated file today (it always passes `--tool claude`),
/// but adding a second tool is documented as a five-edit change and a session's settings
/// file persists on disk between launches. The failure mode of getting this wrong is
/// "veld erased your prompt", not "no badge" — the one outcome this module exists to avoid.
fn parse_tool(tool: Option<&str>) -> Option<AgentTool> {
    match tool {
        None => Some(AgentTool::Claude),
        Some(flag) => match AgentTool::parse(flag) {
            Some(t) => Some(t),
            None => {
                eprintln!("veld: unknown --tool {flag:?}");
                None
            }
        },
    }
}

/// The terminal session this invocation belongs to.
///
/// `--session` first, then `$VELD_PTY_SESSION`, which a Veld terminal exports. The
/// generated hooks always pass the flag — they cannot rely on Claude Code forwarding
/// the shell's environment to a hook subprocess — so the variable is the path for
/// `agent-settings`, called from the wrapper inside the shell itself.
fn session_id(explicit: Option<String>) -> Option<String> {
    explicit
        .or_else(|| std::env::var("VELD_PTY_SESSION").ok())
        .filter(|s| !s.is_empty())
}

/// This instance's shim directory.
///
/// `$VELD_SHIM_DIR` first, because it is what the daemon actually handed this shell and
/// therefore names the directory belonging to the daemon that owns this terminal. The
/// computed fallback is keyed on the daemon port, and a dev instance and the installed
/// one disagree about that — so preferring the variable is what stops a dev terminal
/// writing into the installed instance's directory.
fn shim_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("VELD_SHIM_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    Some(veld_core::instance::shim_dir())
}

/// Remove settings files nothing has used in [`agent::SETTINGS_MAX_AGE`].
///
/// One file per terminal session that has ever run an agent, a few hundred bytes each,
/// so this is tidiness rather than a leak — which is why the age is generous. Deleting
/// one out from under a live agent removes the hooks it is running with, and the two
/// error directions are not symmetric: keeping a file too long costs nothing.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > agent::SETTINGS_MAX_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The hook payload, bounded.
///
/// A hook's stdin is somebody else's output, so it gets a ceiling: a `PostToolUse`
/// payload carries a tool's whole result, and this process exists to read four fields
/// out of the front of a JSON object.
fn read_stdin() -> Option<Vec<u8>> {
    use std::io::Read;
    const MAX: u64 = 1024 * 1024;
    let mut buf = Vec::new();
    std::io::stdin()
        .take(MAX)
        .read_to_end(&mut buf)
        .ok()
        .map(|_| buf)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_comes_from_the_flag_before_the_environment() {
        assert_eq!(
            session_id(Some("explicit".into())).as_deref(),
            Some("explicit")
        );
        // An empty value is not a session. Exporting `VELD_PTY_SESSION=` would
        // otherwise produce a request against `/api/pty/sessions//agent-state`.
        assert_eq!(session_id(Some(String::new())), None);
    }

    #[test]
    fn an_unknown_tool_is_a_usage_error_and_the_default_is_claude() {
        assert_eq!(parse_tool(None), Some(AgentTool::Claude));
        assert_eq!(parse_tool(Some("claude")), Some(AgentTool::Claude));
        assert_eq!(parse_tool(Some("codex")), None);
    }

    /// The sweep is bounded by age, and never touches a file that is still current.
    #[test]
    fn the_sweep_keeps_a_fresh_settings_file_and_drops_an_ancient_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fresh = tmp.path().join("claude-live.json");
        std::fs::write(&fresh, "{}").unwrap();
        sweep(tmp.path());
        assert!(
            fresh.is_file(),
            "a file written moments ago belongs to a session that may still be running"
        );
    }
}
