use clap::Subcommand;
use serde::Serialize;
use veld_core::feedback::{
    Author, Event, EventType, FeedbackStore, Thread, ThreadOrigin, ThreadScope, ThreadStatus,
    new_message, new_thread,
};
use veld_core::state::ProjectState;

use crate::output;

/// Enriched listen output — includes the event and the full thread context.
#[derive(Serialize)]
struct ListenOutput {
    #[serde(flatten)]
    event: Event,
    /// The full thread with all messages, for context.
    /// Named `thread_context` to avoid collision with the `thread` field
    /// in ThreadCreated/AgentThreadCreated event types.
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_context: Option<Thread>,
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum FeedbackCommand {
    /// Wait for the next feedback event (agent-facing).
    Listen {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Only return events after this sequence number.
        #[arg(long)]
        after: Option<u64>,

        /// Timeout in seconds.
        #[arg(long, default_value = "120")]
        timeout: u64,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Reply to a feedback thread (agent-facing).
    Answer {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Thread ID to reply to.
        #[arg(long)]
        thread: String,

        /// Message body.
        message: String,
    },

    /// Open a new thread with a question (agent-facing).
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
        FeedbackCommand::Listen {
            name,
            after,
            timeout,
            json,
        } => run_listen(name, after, timeout, json).await,
        FeedbackCommand::Answer {
            name,
            thread,
            message,
        } => run_answer(name, &thread, &message).await,
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

fn resolve(name: Option<String>, json: bool) -> Option<(std::path::PathBuf, String)> {
    let (config_path, _config) = super::load_config(json)?;
    let project_root = veld_core::config::project_root(&config_path);

    let run_name = match name {
        Some(n) => n,
        None => {
            let project_state = match ProjectState::load(&project_root) {
                Ok(ps) => ps,
                Err(e) => {
                    output::print_error(&format!("Failed to load project state: {e}"), json);
                    return None;
                }
            };
            super::resolve_run_name(None, &project_state, true, json)?
        }
    };

    Some((project_root, run_name))
}

// ---------------------------------------------------------------------------
// listen
// ---------------------------------------------------------------------------

async fn run_listen(name: Option<String>, after: Option<u64>, timeout: u64, json: bool) -> i32 {
    let (project_root, run_name) = match resolve(name, json) {
        Some(pair) => pair,
        None => return 1,
    };

    let store = FeedbackStore::new(&project_root, &run_name);

    // Heartbeat — marks the session as listening.
    if let Err(e) = store.heartbeat() {
        output::print_error(&format!("Failed to update session: {e}"), json);
        return 1;
    }

    // On first call (no --after), emit AgentListening event.
    if after.is_none() {
        if let Err(e) = store.append_event(EventType::AgentListening) {
            output::print_error(&format!("Failed to emit event: {e}"), json);
            return 1;
        }
    }

    let after_seq = after.unwrap_or(0);
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout);

    loop {
        // Check for events.
        match store.get_events_after(after_seq) {
            Ok(events) => {
                // Find the first event that the agent cares about (human-originated).
                if let Some(event) = events.into_iter().find(|e| {
                    matches!(
                        e.event_type,
                        EventType::ThreadCreated { .. }
                            | EventType::HumanMessage { .. }
                            | EventType::Resolved { .. }
                            | EventType::Reopened { .. }
                            | EventType::SessionEnded
                    )
                }) {
                    // Resolve the full thread for context.
                    // Skip for ThreadCreated — the full thread is already in the event.
                    let thread = match &event.event_type {
                        EventType::ThreadCreated { .. } => None,
                        EventType::HumanMessage { thread_id, .. }
                        | EventType::Resolved { thread_id }
                        | EventType::Reopened { thread_id } => {
                            store.get_thread(thread_id).ok().flatten()
                        }
                        _ => None,
                    };

                    if json {
                        let output = ListenOutput { event, thread_context: thread };
                        println!("{}", serde_json::to_string_pretty(&output).unwrap());
                    } else {
                        print_event(&event, thread.as_ref(), &store);
                    }
                    // Update heartbeat before exiting.
                    let _ = store.heartbeat();
                    return 0;
                }
            }
            Err(e) => {
                output::print_error(&format!("Failed to read events: {e}"), json);
                return 1;
            }
        }

        // Check timeout.
        if tokio::time::Instant::now() >= deadline {
            if json {
                println!("null");
            } else {
                output::print_info("Timeout — no new events.");
            }
            // Emit AgentStopped and end session on timeout.
            let _ = store.append_event(EventType::AgentStopped);
            let _ = store.end_session();
            return 0;
        }

        // Heartbeat on each poll iteration.
        let _ = store.heartbeat();

        // Poll every 1 second.
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}

// ---------------------------------------------------------------------------
// answer
// ---------------------------------------------------------------------------

async fn run_answer(name: Option<String>, thread_id: &str, body: &str) -> i32 {
    let (project_root, run_name) = match resolve(name, false) {
        Some(pair) => pair,
        None => return 1,
    };

    let store = FeedbackStore::new(&project_root, &run_name);

    let msg = new_message(Author::Agent, body, None);

    if let Err(e) = store.add_message(thread_id, &msg) {
        output::print_error(&format!("Failed to add message: {e}"), false);
        return 1;
    }

    if let Err(e) = store.append_event(EventType::AgentMessage {
        thread_id: thread_id.to_owned(),
        message: msg,
    }) {
        output::print_error(&format!("Failed to emit event: {e}"), false);
        return 1;
    }

    output::print_info(&format!("Replied to thread {}", &thread_id[..8.min(thread_id.len())]));
    0
}

// ---------------------------------------------------------------------------
// ask
// ---------------------------------------------------------------------------

async fn run_ask(name: Option<String>, page: Option<&str>, body: &str) -> i32 {
    let (project_root, run_name) = match resolve(name, false) {
        Some(pair) => pair,
        None => return 1,
    };

    let store = FeedbackStore::new(&project_root, &run_name);

    let scope = match page {
        Some(url) => ThreadScope::Page {
            page_url: url.to_owned(),
        },
        None => ThreadScope::Global,
    };

    let msg = new_message(Author::Agent, body, None);
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

    output::print_info(&format!(
        "Created thread {} — question posted.",
        &thread.id[..8.min(thread.id.len())]
    ));
    0
}

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

fn run_threads(name: Option<String>, json: bool, open: bool, resolved: bool) -> i32 {
    let (project_root, run_name) = match resolve(name, json) {
        Some(pair) => pair,
        None => return 1,
    };

    let store = FeedbackStore::new(&project_root, &run_name);

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
    let (project_root, run_name) = match resolve(name, json) {
        Some(pair) => pair,
        None => return 1,
    };

    let store = FeedbackStore::new(&project_root, &run_name);

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

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

fn print_event(
    event: &veld_core::feedback::Event,
    thread: Option<&veld_core::feedback::Thread>,
    store: &FeedbackStore,
) {
    match &event.event_type {
        EventType::ThreadCreated { thread: t } => {
            println!(
                "{} Thread created ({})",
                output::bold("Event"),
                &t.id[..8.min(t.id.len())],
            );
            print_thread_context(t, store);
        }
        EventType::HumanMessage { thread_id, message } => {
            println!(
                "{} New message on thread {}",
                output::bold("Event"),
                &thread_id[..8.min(thread_id.len())],
            );
            println!("  New: {}", message.body);
            if let Some(ref screenshot) = message.screenshot {
                print_screenshot(screenshot, store);
            }
            // Print full thread context so the agent has all messages.
            if let Some(t) = thread {
                println!();
                print_thread_context(t, store);
            }
        }
        EventType::Resolved { thread_id } => {
            println!(
                "{} Thread {} resolved",
                output::bold("Event"),
                &thread_id[..8.min(thread_id.len())],
            );
        }
        EventType::Reopened { thread_id } => {
            println!(
                "{} Thread {} reopened",
                output::bold("Event"),
                &thread_id[..8.min(thread_id.len())],
            );
            if let Some(t) = thread {
                println!();
                print_thread_context(t, store);
            }
        }
        EventType::SessionEnded => {
            println!(
                "{} Session ended — reviewer clicked \"All Good\".",
                output::bold("Event"),
            );
        }
        _ => {
            println!("{} {:?}", output::bold("Event"), event.event_type);
        }
    }
    println!("  seq: {}", event.seq);
}

fn print_thread_context(thread: &veld_core::feedback::Thread, store: &FeedbackStore) {
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
        &thread.id[..8.min(thread.id.len())],
        thread.messages.len(),
        if thread.status == ThreadStatus::Open { "open" } else { "resolved" },
    );
    for msg in &thread.messages {
        let author = match msg.author {
            Author::Human => "human",
            Author::Agent => "agent",
        };
        println!("    [{}] {}", output::dim(author), msg.body);
        if let Some(ref screenshot) = msg.screenshot {
            print_screenshot(screenshot, store);
        }
    }
}

fn print_screenshot(screenshot: &str, store: &FeedbackStore) {
    let id = screenshot
        .trim_end_matches(".png")
        .rsplit('/')
        .next()
        .unwrap_or(screenshot);
    let path = store
        .screenshot_path(&format!("{id}.png"))
        .display()
        .to_string();
    println!("    Screenshot: {}", output::dim(&path));
}

fn print_scope(scope: &ThreadScope) {
    match scope {
        ThreadScope::Element {
            page_url, selector, ..
        } => {
            println!("  Element: {}", output::dim(selector));
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

fn print_thread(thread: &veld_core::feedback::Thread) {
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
        &thread.id[..8.min(thread.id.len())],
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
        println!(
            "  [{}] {}",
            output::dim(author),
            msg.body,
        );
    }
    println!();
}
