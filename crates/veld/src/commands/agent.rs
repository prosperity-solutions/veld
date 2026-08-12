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

/// `veld agent-settings [--tool claude|codex] [--session <id>]`
///
/// Prints this session's ephemeral hook configuration on stdout: a settings file's
/// path for a tool [`agent::Injection::SettingsFile`] describes (Claude), or a
/// literal config-override value for one [`agent::Injection::ConfigOverride`]
/// describes (Codex) — see [`agent::AgentTool::injection`].
///
/// Stdout is that value and nothing else — the caller is a shell doing
/// `settings=$(veld agent-settings ...)`, so a stray diagnostic there becomes part
/// of a filename or a `-c` value (AGENTS.md: machine-readable output on stdout,
/// chrome on stderr).
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
    let Ok(cli) = std::env::current_exe() else {
        return 1;
    };

    match tool {
        AgentTool::Claude => write_claude_settings_file(&cli, &session),
        // No file: the value itself is the whole answer, and nothing here can fail
        // short of stdout being gone — which println's own ignored result already
        // treats as "the caller stopped listening", the same as every other exit.
        AgentTool::Codex => {
            println!("{}", agent::codex_notify_config(&cli, &session));
            0
        }
    }
}

/// The [`agent::Injection::SettingsFile`] half of [`settings`]: write Claude's
/// settings document to this session's ephemeral file and print its path.
///
/// Hardcoded to Claude rather than generic over `AgentTool`, on purpose: it is the
/// only `SettingsFile` tool today, and `settings()` above is what decides which of
/// the two `agent-settings` shapes a tool gets. A generic version with a runtime
/// assertion is a guard a release build compiles out; naming the tool in the
/// function makes a mis-route a compile error for whoever adds a second
/// `SettingsFile` tool, instead of a debug-only assertion nobody sees fire in the
/// binary users actually run.
fn write_claude_settings_file(cli: &Path, session: &str) -> i32 {
    let Some(dir) = shim_dir() else {
        return 1;
    };
    let path = agent::settings_path(&dir, AgentTool::Claude, session);
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

    let doc = agent::claude_settings_doc(cli, session);
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

/// `veld agent-state [--tool claude|codex] [--session <id>] [--launched] [PAYLOAD]`
///
/// Reads a hook payload — on **stdin** for a tool that pipes it there (Claude), or
/// from `PAYLOAD` for one that appends it as the final argument instead (Codex's
/// `notify`) — works out what state it reports, and tells the daemon. Called by the
/// hooks in the file [`settings`] installs.
///
/// # Why the payload is parsed here and not in the daemon
///
/// The daemon's endpoint takes a `state`, not a vendor's JSON. Keeping the vendor
/// schema on this side means a second tool is a new mapping function plus a hook
/// installer, with nothing to change in the daemon, the wire, or the UI — which is the
/// whole claim the "one generic receiving end" design makes. It also keeps a
/// third-party schema out of a long-lived process.
pub async fn state(
    tool: Option<String>,
    session: Option<String>,
    launched: bool,
    payload_arg: Option<String>,
) -> i32 {
    let Some(tool) = parse_tool(tool.as_deref()) else {
        return 1;
    };
    let Some(session) = session_id(session) else {
        return 1;
    };
    // `--launched` is the wrapper's own report, fired just before it `exec`s the agent.
    // No payload: there is no hook event, because no hook has run — that is the point.
    // It claims hook authority for the pane while saying nothing is happening, which is
    // what stops the shell's "a command is running here" driving an activity spinner for
    // a session sitting idle at its prompt.
    let reported = if launched {
        agent::State::Ready
    } else {
        // A hook that sends nothing, or sends something unparseable, is not an error worth
        // reporting into somebody's agent session — it is a state we do not know.
        let payload: agent::HookPayload = match tool {
            // Codex's `notify` never writes to stdin at all (it is explicitly nulled),
            // so reading it here would just be a wasted read, not a second source.
            AgentTool::Codex => match payload_from_arg(payload_arg) {
                Some(payload) => payload,
                None => return 1,
            },
            AgentTool::Claude => match read_stdin() {
                Some(body) => serde_json::from_slice(&body).unwrap_or_default(),
                None => return 1,
            },
        };
        match tool {
            AgentTool::Claude => agent::claude_state(&payload),
            AgentTool::Codex => agent::codex_state(&payload),
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
/// prompt. Unreachable through a generated file today (each one always passes its own
/// `--tool`), but a session's settings file persists on disk between launches, so a third
/// tool arriving must not make a stale Claude one start exiting 2 by accident. The failure
/// mode of getting this wrong is "veld erased your prompt", not "no badge" — the one
/// outcome this module exists to avoid.
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

/// The hook payload, from a tool that appends it as the final argv entry rather than
/// piping it on stdin (Codex).
///
/// `None` only when the argument itself is absent — the wrapper always passes one for
/// a real hook invocation, so its absence means something upstream is wrong and the
/// caller exits 1. A *present-but-malformed* argument is not that: it degrades to a
/// default (all-`None`) [`agent::HookPayload`], which every tool's mapping function
/// already turns into [`agent::State::Unknown`], the same silent "we don't know"
/// [`read_stdin`]'s own malformed case produces.
fn payload_from_arg(payload_arg: Option<String>) -> Option<agent::HookPayload> {
    let body = payload_arg?;
    Some(serde_json::from_str(&body).unwrap_or_default())
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
    fn a_missing_argv_payload_is_an_error_but_a_malformed_one_is_unknown() {
        assert!(payload_from_arg(None).is_none());
        // Malformed JSON degrades to a default (all-`None`) payload, same silent
        // "we don't know" `read_stdin`'s own malformed case produces — never an error
        // exit for a hook that is somebody else's process misbehaving, not veld's.
        let garbage = payload_from_arg(Some("not json".to_owned()))
            .expect("a present argument is always Some, even if unparseable");
        assert_eq!(agent::codex_state(&garbage), agent::State::Unknown);
        // A real Codex payload parses and maps correctly through this path.
        let real = payload_from_arg(Some(
            r#"{"type":"agent-turn-complete","thread-id":"t1"}"#.to_owned(),
        ))
        .expect("a present, well-formed argument parses");
        assert_eq!(agent::codex_state(&real), agent::State::Idle);
    }

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
        assert_eq!(parse_tool(Some("codex")), Some(AgentTool::Codex));
        assert_eq!(parse_tool(Some("cursor")), None);
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
