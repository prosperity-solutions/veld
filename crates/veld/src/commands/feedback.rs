use chrono::{DateTime, Utc};
use clap::Subcommand;
use serde::Serialize;
use veld_core::feedback::{
    Author, EventType, FeedbackStore, Thread, ThreadOrigin, ThreadScope, ThreadStatus, new_message,
    new_thread,
};

use crate::output;

// ---------------------------------------------------------------------------
// `next` output schema
// ---------------------------------------------------------------------------

/// Output of `veld feedback next`.
///
/// `result` is one of `"item"` | `"timeout"` | `"ended"`. When `"item"`, the
/// `thread` field carries everything the agent needs to act in a single call.
#[derive(Serialize)]
struct NextOutput {
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<NextThread>,
}

/// A queue item — the head thread with full history and context.
#[derive(Serialize)]
struct NextThread {
    id: String,
    scope: ThreadScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    component_trace: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    viewport_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    viewport_height: Option<u32>,
    messages: Vec<NextMessage>,
}

#[derive(Serialize)]
struct NextMessage {
    author: Author,
    body: String,
    /// Absolute path to the screenshot PNG, if any — the agent reads it directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot: Option<String>,
    created_at: DateTime<Utc>,
}

impl NextThread {
    fn from_thread(thread: &Thread, store: &FeedbackStore, project_root: &std::path::Path) -> Self {
        let messages = thread
            .messages
            .iter()
            .map(|m| NextMessage {
                author: m.author,
                body: m.body.clone(),
                // Screenshots live in the database — export to a temp file so
                // the agent gets an absolute path it can actually read.
                screenshot: m
                    .screenshot
                    .as_deref()
                    .and_then(|s| export_screenshot(s, store, project_root)),
                created_at: m.created_at,
            })
            .collect();
        NextThread {
            id: thread.id.clone(),
            scope: thread.scope.clone(),
            component_trace: thread.component_trace.clone(),
            viewport_width: thread.viewport_width,
            viewport_height: thread.viewport_height,
            messages,
        }
    }
}

/// Export a stored screenshot to `.veld/tmp/screenshots/{run}/{file}` and
/// return the absolute path, or `None` when the screenshot doesn't exist.
fn export_screenshot(
    screenshot: &str,
    store: &FeedbackStore,
    project_root: &std::path::Path,
) -> Option<String> {
    let id = screenshot
        .trim_end_matches(".png")
        .rsplit('/')
        .next()
        .unwrap_or(screenshot);
    let filename = format!("{id}.png");
    let data = store.get_screenshot(&filename).ok().flatten()?;
    let dir = project_root
        .join(".veld")
        .join("tmp")
        .join("screenshots")
        .join(store.run_name());
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(&filename);
    std::fs::write(&path, &data).ok()?;
    Some(path.display().to_string())
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum FeedbackCommand {
    /// Get the next feedback item to work on (agent-facing).
    ///
    /// Returns the oldest *waiting* thread: one open thread whose latest message
    /// is from a human. This is a pure read — the same item is returned on every
    /// call until you `reply` or `resolve` it. There is no cursor to track.
    ///
    /// Outcomes (`result` field): `item` (work it), `timeout` (call again),
    /// `ended` (the reviewer clicked "Done" — stop).
    Next {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Block until an item is available, the reviewer ends the session, or
        /// the timeout elapses.
        #[arg(long)]
        wait: bool,

        /// Max seconds to block when `--wait` is set.
        #[arg(long, default_value = "240")]
        timeout: u64,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Reply to a feedback thread (agent-facing).
    ///
    /// The thread becomes *blocked* — hidden from `next` — until the human
    /// responds again, at which point it re-enters the queue automatically.
    Reply {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Thread ID (a short prefix is accepted).
        thread: String,

        /// Message body.
        message: String,
    },

    /// Resolve a thread (agent-facing).
    ///
    /// Use only on explicit human approval ("looks good", "done"). When in
    /// doubt, `reply` and leave it open.
    Resolve {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Thread ID (a short prefix is accepted).
        thread: String,
    },

    /// Open a new thread with a question for the reviewer (agent-facing).
    Ask {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Page URL to scope the thread to (omit for global).
        #[arg(long)]
        page: Option<String>,

        /// Message body.
        message: String,
    },

    /// List feedback threads.
    Threads {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        /// Show only open threads.
        #[arg(long)]
        open: bool,

        /// Show only resolved threads.
        #[arg(long)]
        resolved: bool,
    },

    /// Show the event log (for debugging).
    Events {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Only show events after this sequence number.
        #[arg(long, default_value = "0")]
        after: u64,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(command: FeedbackCommand) -> i32 {
    match command {
        FeedbackCommand::Next {
            name,
            wait,
            timeout,
            json,
        } => run_next(name, wait, timeout, json).await,
        FeedbackCommand::Reply {
            name,
            thread,
            message,
        } => run_reply(name, &thread, &message).await,
        FeedbackCommand::Resolve { name, thread } => run_resolve(name, &thread).await,
        FeedbackCommand::Ask {
            name,
            page,
            message,
        } => run_ask(name, page.as_deref(), &message).await,
        FeedbackCommand::Threads {
            name,
            json,
            open,
            resolved,
        } => run_threads(name, json, open, resolved),
        FeedbackCommand::Events { name, after, json } => run_events(name, after, json),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve(
    name: Option<String>,
    json: bool,
) -> Option<(veld_core::db::Db, std::path::PathBuf, String)> {
    let (config_path, _config) = super::load_config(json)?;
    let project_root = veld_core::config::project_root(&config_path);

    let db = super::open_db(json)?;
    let project_state = match db.load_project_state(&project_root) {
        Ok(ps) => ps,
        Err(e) => {
            output::print_error(&format!("Failed to load project state: {e}"), json);
            return None;
        }
    };

    let run_name = match name {
        // Validate an explicit --name so a typo doesn't read an empty feedback
        // store and make `next` time out forever. Accept a run that is either
        // active OR has feedback data — a stopped run keeps its feedback rows,
        // and `threads`/`events` still work.
        Some(n) => {
            let active = project_state.get_run(&n).is_some();
            let has_feedback = FeedbackStore::new(db.clone(), &project_root, &n).has_data();
            if !active && !has_feedback {
                output::print_error(
                    &format!("No such run '{n}'. Run `veld runs` to list active runs."),
                    json,
                );
                return None;
            }
            n
        }
        None => super::resolve_run_name(None, &project_state, true, json)?,
    };

    Some((db, project_root, run_name))
}

// ---------------------------------------------------------------------------
// next
// ---------------------------------------------------------------------------

async fn run_next(name: Option<String>, wait: bool, timeout: u64, json: bool) -> i32 {
    let (db, project_root, run_name) = match resolve(name, json) {
        Some(t) => t,
        None => return 1,
    };

    let store = FeedbackStore::new(db, &project_root, &run_name);

    // Announce listening only on transition (browser toast + pulsing FAB).
    if !store.is_listening(60).unwrap_or(false) {
        let _ = store.append_event(EventType::AgentListening);
    }
    let _ = store.heartbeat();

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout);

    loop {
        match store.next_waiting_thread() {
            Ok(Some(thread)) => {
                emit_next("item", Some(&thread), &store, &project_root, json);
                let _ = store.heartbeat();
                return 0;
            }
            Ok(None) => {
                // Queue drained. If the reviewer clicked "Done", stop the loop.
                // Mark the session stopped (status Idle + consume the Done flag)
                // and emit AgentStopped so the browser immediately stops showing
                // "listening" — otherwise the still-fresh heartbeat would make
                // /session re-announce this now-exiting agent.
                if store.is_ended().unwrap_or(false) {
                    let _ = store.mark_stopped();
                    let _ = store.append_event(EventType::AgentStopped);
                    emit_next("ended", None, &store, &project_root, json);
                    return 0;
                }
            }
            Err(e) => {
                output::print_error(&format!("Failed to read feedback: {e}"), json);
                return 1;
            }
        }

        // Nothing waiting and not ended.
        if !wait || tokio::time::Instant::now() >= deadline {
            emit_next("timeout", None, &store, &project_root, json);
            return 0;
        }

        let _ = store.heartbeat();
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}

/// Print a `next` outcome as JSON (for agents) or human-readable text.
fn emit_next(
    result: &'static str,
    thread: Option<&Thread>,
    store: &FeedbackStore,
    project_root: &std::path::Path,
    json: bool,
) {
    if json {
        let out = NextOutput {
            result,
            thread: thread.map(|t| NextThread::from_thread(t, store, project_root)),
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return;
    }
    match result {
        "item" => {
            if let Some(t) = thread {
                println!("{} next feedback item", output::bold("Feedback"));
                print_thread_context(t, store, project_root);
            }
        }
        "ended" => output::print_info("Feedback session ended — the reviewer clicked \"Done\"."),
        _ => {
            output::print_info("No feedback waiting (timeout). Run `next` again to keep watching.")
        }
    }
}

// ---------------------------------------------------------------------------
// reply
// ---------------------------------------------------------------------------

async fn run_reply(name: Option<String>, thread_id: &str, body: &str) -> i32 {
    let (db, project_root, run_name) = match resolve(name, false) {
        Some(t) => t,
        None => return 1,
    };

    let store = FeedbackStore::new(db, &project_root, &run_name);

    let thread_id = match store.resolve_thread_id(thread_id) {
        Ok(id) => id,
        Err(e) => {
            output::print_error(&format!("Failed to resolve thread: {e}"), false);
            return 1;
        }
    };

    let clean_body = strip_shell_escapes(body);
    let msg = new_message(Author::Agent, &clean_body, None, None);

    // Emit the event before persisting the message. If the process dies between
    // the two writes, the thread's last author stays human → `next` re-surfaces
    // it → a visible, recoverable duplicate reply. The reverse order would leave
    // the thread silently blocked with no event, so the reviewer never sees the
    // reply and the thread is wedged.
    if let Err(e) = store.append_event(EventType::AgentMessage {
        thread_id: thread_id.clone(),
        message: msg.clone(),
    }) {
        output::print_error(&format!("Failed to emit event: {e}"), false);
        return 1;
    }

    if let Err(e) = store.add_message(&thread_id, &msg) {
        output::print_error(&format!("Failed to add message: {e}"), false);
        return 1;
    }

    // Keep the session alive while the agent works items, so `is_listening`
    // doesn't lapse between `next` calls and flap the "listening" indicator.
    let _ = store.heartbeat();

    output::print_info(&format!(
        "Replied to thread {thread_id} — now waiting on the reviewer."
    ));
    0
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

async fn run_resolve(name: Option<String>, thread_id: &str) -> i32 {
    let (db, project_root, run_name) = match resolve(name, false) {
        Some(t) => t,
        None => return 1,
    };

    let store = FeedbackStore::new(db, &project_root, &run_name);

    let thread_id = match store.resolve_thread_id(thread_id) {
        Ok(id) => id,
        Err(e) => {
            output::print_error(&format!("Failed to resolve thread: {e}"), false);
            return 1;
        }
    };

    if let Err(e) = store.set_thread_status(&thread_id, ThreadStatus::Resolved) {
        output::print_error(&format!("Failed to resolve thread: {e}"), false);
        return 1;
    }

    if let Err(e) = store.append_event(EventType::Resolved {
        thread_id: thread_id.clone(),
    }) {
        output::print_error(&format!("Failed to emit event: {e}"), false);
        return 1;
    }

    let _ = store.heartbeat();
    output::print_info(&format!("Resolved thread {thread_id}."));
    0
}

// ---------------------------------------------------------------------------
// ask
// ---------------------------------------------------------------------------

async fn run_ask(name: Option<String>, page: Option<&str>, body: &str) -> i32 {
    let (db, project_root, run_name) = match resolve(name, false) {
        Some(t) => t,
        None => return 1,
    };

    let store = FeedbackStore::new(db, &project_root, &run_name);

    let scope = match page {
        Some(url) => ThreadScope::Page {
            page_url: url.to_owned(),
        },
        None => ThreadScope::Global,
    };

    let clean_body = strip_shell_escapes(body);
    let msg = new_message(Author::Agent, &clean_body, None, None);
    let thread = new_thread(scope, ThreadOrigin::Agent, None, None, None, msg);

    if let Err(e) = store.save_thread(&thread) {
        output::print_error(&format!("Failed to create thread: {e}"), false);
        return 1;
    }

    if let Err(e) = store.append_event(EventType::AgentThreadCreated {
        thread: thread.clone(),
    }) {
        output::print_error(&format!("Failed to emit event: {e}"), false);
        return 1;
    }

    output::print_info(&format!("Created thread {} — question posted.", thread.id));
    0
}

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

fn run_threads(name: Option<String>, json: bool, open: bool, resolved: bool) -> i32 {
    let (db, project_root, run_name) = match resolve(name, json) {
        Some(t) => t,
        None => return 1,
    };

    let store = FeedbackStore::new(db, &project_root, &run_name);

    let filter = if open {
        Some(ThreadStatus::Open)
    } else if resolved {
        Some(ThreadStatus::Resolved)
    } else {
        None
    };

    match store.list_threads(filter) {
        Ok(threads) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&threads).unwrap());
            } else if threads.is_empty() {
                output::print_info("No feedback threads.");
            } else {
                for thread in &threads {
                    print_thread(thread);
                }
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to list threads: {e}"), json);
            1
        }
    }
}

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

fn run_events(name: Option<String>, after: u64, json: bool) -> i32 {
    let (db, project_root, run_name) = match resolve(name, json) {
        Some(t) => t,
        None => return 1,
    };

    let store = FeedbackStore::new(db, &project_root, &run_name);

    match store.get_events_after(after) {
        Ok(events) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&events).unwrap());
            } else if events.is_empty() {
                output::print_info(&format!("No events after seq {after}."));
            } else {
                for event in &events {
                    println!(
                        "  seq={} event={} ts={}",
                        event.seq,
                        event_label(&event.event_type),
                        event.timestamp.format("%H:%M:%S"),
                    );
                }
                println!();
                output::print_info(&format!(
                    "{} event(s), latest seq={}.",
                    events.len(),
                    events.last().map(|e| e.seq).unwrap_or(0),
                ));
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to read events: {e}"), json);
            1
        }
    }
}

fn event_label(et: &EventType) -> &'static str {
    match et {
        EventType::ThreadCreated { .. } => "thread_created",
        EventType::HumanMessage { .. } => "human_message",
        EventType::Resolved { .. } => "resolved",
        EventType::Reopened { .. } => "reopened",
        EventType::SessionEnded => "session_ended",
        EventType::AgentMessage { .. } => "agent_message",
        EventType::AgentThreadCreated { .. } => "agent_thread_created",
        EventType::AgentListening => "agent_listening",
        EventType::AgentStopped => "agent_stopped",
    }
}

/// Strip common shell escape artifacts from message bodies.
///
/// Agents calling `veld feedback reply` from bash often pass the message in
/// double-quotes, which causes `\!` (and sometimes `\?`) to leak through
/// literally because bash preserves the backslash before history-expansion
/// characters. We strip these so the end user sees clean text.
fn strip_shell_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('!') | Some('?') => {
                    out.push(chars.next().unwrap());
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

fn print_thread_context(thread: &Thread, store: &FeedbackStore, project_root: &std::path::Path) {
    print_scope(&thread.scope);
    if let Some(ref trace) = thread.component_trace {
        if !trace.is_empty() {
            println!("  Components: {}", output::dim(&trace.join(" > ")));
        }
    }
    if let (Some(w), Some(h)) = (thread.viewport_width, thread.viewport_height) {
        println!("  Viewport: {}", output::dim(&format!("{w}x{h}")));
    }
    println!(
        "  Thread: {} ({} message(s), {})",
        thread.id,
        thread.messages.len(),
        if thread.status == ThreadStatus::Open {
            "open"
        } else {
            "resolved"
        },
    );
    for msg in &thread.messages {
        let author = match msg.author {
            Author::Human => "human",
            Author::Agent => "agent",
        };
        println!("    [{}] {}", output::dim(author), msg.body);
        if let Some(ref screenshot) = msg.screenshot {
            if let Some(path) = export_screenshot(screenshot, store, project_root) {
                println!("    Screenshot: {}", output::dim(&path));
            }
        }
    }
}

fn print_scope(scope: &ThreadScope) {
    match scope {
        ThreadScope::Element {
            page_url,
            selector,
            element_text,
            source_file,
            source_line,
            ..
        } => {
            println!("  Element: {}", output::dim(selector));
            if let Some(text) = element_text {
                println!("  Text: {}", output::dim(&format!("\"{text}\"")));
            }
            if let Some(file) = source_file {
                let loc = match source_line {
                    Some(line) => format!("{file}:{line}"),
                    None => file.clone(),
                };
                println!("  Source: {}", output::dim(&loc));
            }
            println!("  Page: {}", output::dim(page_url));
        }
        ThreadScope::Page { page_url } => {
            println!("  Page: {}", output::dim(page_url));
        }
        ThreadScope::Global => {
            println!("  Scope: {}", output::dim("global"));
        }
    }
}

fn print_thread(thread: &Thread) {
    let status = match thread.status {
        ThreadStatus::Open => "open",
        ThreadStatus::Resolved => "resolved",
    };
    let origin = match thread.origin {
        ThreadOrigin::Human => "human",
        ThreadOrigin::Agent => "agent",
    };

    println!(
        "{} {} [{}] (by {}, {} message(s))",
        output::bold("Thread"),
        thread.id,
        status,
        origin,
        thread.messages.len(),
    );
    print_scope(&thread.scope);

    for msg in &thread.messages {
        let author = match msg.author {
            Author::Human => "human",
            Author::Agent => "agent",
        };
        println!("  [{}] {}", output::dim(author), msg.body,);
    }
    println!();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_strip_shell_escapes() {
        assert_eq!(strip_shell_escapes(r"done\!"), "done!");
        assert_eq!(strip_shell_escapes(r"why\?"), "why?");
        // Other backslash sequences are preserved.
        assert_eq!(strip_shell_escapes(r"path\to"), r"path\to");
    }

    fn test_store(tmp: &TempDir) -> FeedbackStore {
        let db = veld_core::db::Db::open_at(&tmp.path().join("veld.db")).unwrap();
        FeedbackStore::new(db, tmp.path(), "test-run")
    }

    #[test]
    fn test_next_thread_resolves_screenshot_abs_path() {
        let tmp = TempDir::new().unwrap();
        let store = test_store(&tmp);
        store.save_screenshot("ss_1", b"PNG").unwrap();

        let msg = new_message(Author::Human, "look here", Some("ss_1.png".into()), None);
        let thread = new_thread(
            ThreadScope::Global,
            ThreadOrigin::Human,
            None,
            None,
            None,
            msg,
        );

        let nt = NextThread::from_thread(&thread, &store, tmp.path());
        let ss = nt.messages[0].screenshot.as_deref().unwrap();
        assert!(ss.ends_with("ss_1.png"));
        assert!(std::path::Path::new(ss).is_absolute());
        assert_eq!(std::fs::read(ss).unwrap(), b"PNG");
    }

    #[test]
    fn test_next_thread_omits_missing_screenshot() {
        let tmp = TempDir::new().unwrap();
        let store = test_store(&tmp);
        // Referenced screenshot was never saved (or was pruned) — don't
        // hand the agent a dangling path it can't read.
        let msg = new_message(Author::Human, "look here", Some("gone.png".into()), None);
        let thread = new_thread(
            ThreadScope::Global,
            ThreadOrigin::Human,
            None,
            None,
            None,
            msg,
        );

        let nt = NextThread::from_thread(&thread, &store, tmp.path());
        assert!(nt.messages[0].screenshot.is_none());
    }

    #[test]
    fn test_next_output_serialization() {
        let out = NextOutput {
            result: "timeout",
            thread: None,
        };
        assert_eq!(
            serde_json::to_string(&out).unwrap(),
            r#"{"result":"timeout"}"#
        );

        let ended = NextOutput {
            result: "ended",
            thread: None,
        };
        assert_eq!(
            serde_json::to_string(&ended).unwrap(),
            r#"{"result":"ended"}"#
        );
    }
}
