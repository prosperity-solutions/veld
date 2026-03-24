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

/// Batch listen output — all pending items at once.
#[derive(Serialize)]
struct BatchListenOutput {
    events: Vec<ListenOutput>,
    /// Highest seq in the batch, for the agent's next --after value.
    last_seq: u64,
    /// The agent identity used for claiming.
    agent_id: String,
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum FeedbackCommand {
    /// Wait for feedback events (agent-facing).
    /// Returns all pending events by default (batch mode) and auto-claims their threads.
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

        /// Return only the first event (legacy single-event mode).
        #[arg(long)]
        no_batch: bool,

        /// Agent identity (default: agent-<pid>).
        #[arg(long)]
        agent: Option<String>,
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

        /// JSON array of interactive control definitions (sliders, buttons, etc.)
        #[arg(long)]
        controls: Option<String>,
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

        /// JSON array of interactive control definitions (sliders, buttons, etc.)
        #[arg(long)]
        controls: Option<String>,
    },

    /// Release a claimed thread with an optional status comment (agent-facing).
    Release {
        /// Name of the run.
        #[arg(long)]
        name: Option<String>,

        /// Thread ID to release.
        #[arg(long)]
        thread: String,

        /// Agent identity (only releases if it matches the claimer).
        #[arg(long)]
        agent: Option<String>,

        /// Status comment describing what was done (posted as an agent message).
        message: Option<String>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
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
            no_batch,
            agent,
        } => run_listen(name, after, timeout, json, no_batch, agent).await,
        FeedbackCommand::Answer {
            name,
            thread,
            message,
            controls,
        } => run_answer(name, &thread, &message, controls.as_deref()).await,
        FeedbackCommand::Ask {
            name,
            page,
            message,
            controls,
        } => run_ask(name, page.as_deref(), &message, controls.as_deref()).await,
        FeedbackCommand::Release {
            name,
            thread,
            agent,
            message,
            json,
        } => run_release(name, &thread, agent.as_deref(), message.as_deref(), json).await,
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

async fn run_listen(
    name: Option<String>,
    after: Option<u64>,
    timeout: u64,
    json: bool,
    no_batch: bool,
    agent: Option<String>,
) -> i32 {
    let (project_root, run_name) = match resolve(name, json) {
        Some(pair) => pair,
        None => return 1,
    };

    let store = FeedbackStore::new(&project_root, &run_name);
    let agent_id = agent.unwrap_or_else(|| format!("agent-{}", std::process::id()));

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
                let batch = process_batch(&store, events, after_seq, &agent_id, no_batch);

                if !batch.outputs.is_empty() {
                    if json {
                        if no_batch {
                            // Legacy: single object output.
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&batch.outputs[0]).unwrap()
                            );
                        } else {
                            let output = BatchListenOutput {
                                last_seq: batch.last_seq,
                                agent_id: agent_id.clone(),
                                events: batch.outputs,
                            };
                            println!("{}", serde_json::to_string_pretty(&output).unwrap());
                        }
                    } else {
                        for out in &batch.outputs {
                            print_event(&out.event, out.thread_context.as_ref(), &store);
                        }
                    }
                    let _ = store.heartbeat();
                    return 0;
                }
                // No claimable events — keep polling.
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

/// Check if an event is human-originated (the kind agents care about).
fn is_human_event(event: &Event) -> bool {
    matches!(
        event.event_type,
        EventType::ThreadCreated { .. }
            | EventType::HumanMessage { .. }
            | EventType::Resolved { .. }
            | EventType::Reopened { .. }
            | EventType::SessionEnded
    )
}

/// Extract the thread ID from an event type, if it references a thread.
fn event_thread_id(event_type: &EventType) -> Option<&str> {
    match event_type {
        EventType::ThreadCreated { thread } => Some(&thread.id),
        EventType::HumanMessage { thread_id, .. }
        | EventType::Resolved { thread_id }
        | EventType::Reopened { thread_id } => Some(thread_id.as_str()),
        _ => None,
    }
}

/// Result of processing a batch of events.
struct BatchResult {
    outputs: Vec<ListenOutput>,
    last_seq: u64,
}

/// Core batch processing logic: filter human events, auto-claim, dedup, skip claimed.
/// Extracted for testability.
fn process_batch(
    store: &FeedbackStore,
    events: Vec<Event>,
    after_seq: u64,
    agent_id: &str,
    no_batch: bool,
) -> BatchResult {
    let human_events: Vec<Event> = events.into_iter().filter(|e| is_human_event(e)).collect();

    let mut outputs: Vec<ListenOutput> = Vec::new();
    let mut last_seq = after_seq;
    let mut claimed_threads: std::collections::HashSet<String> = std::collections::HashSet::new();

    for event in human_events {
        let thread_id = event_thread_id(&event.event_type);
        let should_claim = thread_id.is_some()
            && !matches!(
                event.event_type,
                EventType::Resolved { .. } | EventType::SessionEnded
            );

        if should_claim {
            let tid = thread_id.unwrap();
            if store.claim_thread(tid, agent_id).is_err() {
                continue;
            }
            if claimed_threads.insert(tid.to_owned()) {
                let _ = store.append_event(EventType::ThreadClaimed {
                    thread_id: tid.to_owned(),
                    agent_id: agent_id.to_owned(),
                });
            }
        }

        last_seq = event.seq;

        let thread = match &event.event_type {
            EventType::ThreadCreated { .. } => None,
            EventType::HumanMessage { thread_id, .. }
            | EventType::Resolved { thread_id }
            | EventType::Reopened { thread_id } => store.get_thread(thread_id).ok().flatten(),
            _ => None,
        };

        outputs.push(ListenOutput {
            event,
            thread_context: thread,
        });

        if no_batch {
            break;
        }
    }

    BatchResult { outputs, last_seq }
}

// ---------------------------------------------------------------------------
// answer
// ---------------------------------------------------------------------------

async fn run_answer(
    name: Option<String>,
    thread_id: &str,
    body: &str,
    controls: Option<&str>,
) -> i32 {
    let (project_root, run_name) = match resolve(name, false) {
        Some(pair) => pair,
        None => return 1,
    };

    let store = FeedbackStore::new(&project_root, &run_name);

    let controls_value = controls.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    let msg = new_message(Author::Agent, body, None, controls_value);

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

    output::print_info(&format!(
        "Replied to thread {}",
        &thread_id[..8.min(thread_id.len())]
    ));
    0
}

// ---------------------------------------------------------------------------
// release
// ---------------------------------------------------------------------------

async fn run_release(
    name: Option<String>,
    thread_id: &str,
    agent_id: Option<&str>,
    message: Option<&str>,
    json: bool,
) -> i32 {
    let (project_root, run_name) = match resolve(name, json) {
        Some(pair) => pair,
        None => return 1,
    };

    let store = FeedbackStore::new(&project_root, &run_name);

    // Post the status comment before releasing (atomic: comment + release).
    if let Some(body) = message {
        let msg = new_message(Author::Agent, body, None, None);
        if let Err(e) = store.add_message(thread_id, &msg) {
            output::print_error(&format!("Failed to add message: {e}"), json);
            return 1;
        }
        if let Err(e) = store.append_event(EventType::AgentMessage {
            thread_id: thread_id.to_owned(),
            message: msg,
        }) {
            output::print_error(&format!("Failed to emit event: {e}"), json);
            return 1;
        }
    }

    match store.release_thread(thread_id, agent_id) {
        Ok(_) => {
            let releaser = agent_id.unwrap_or("force");
            if let Err(e) = store.append_event(EventType::ThreadReleased {
                thread_id: thread_id.to_owned(),
                agent_id: releaser.to_owned(),
            }) {
                output::print_error(&format!("Failed to emit event: {e}"), json);
                return 1;
            }

            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "released": true,
                        "thread_id": thread_id,
                        "agent_id": releaser,
                    })
                );
            } else {
                output::print_info(&format!(
                    "Released thread {}",
                    &thread_id[..8.min(thread_id.len())]
                ));
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to release thread: {e}"), json);
            1
        }
    }
}

// ---------------------------------------------------------------------------
// ask
// ---------------------------------------------------------------------------

async fn run_ask(
    name: Option<String>,
    page: Option<&str>,
    body: &str,
    controls: Option<&str>,
) -> i32 {
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

    let controls_value = controls.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    let msg = new_message(Author::Agent, body, None, controls_value);
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
        EventType::ThreadClaimed { .. } => "thread_claimed",
        EventType::ThreadReleased { .. } => "thread_released",
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
        EventType::ThreadClaimed {
            thread_id,
            agent_id,
        } => {
            println!(
                "{} Thread {} claimed by {}",
                output::bold("Event"),
                &thread_id[..8.min(thread_id.len())],
                agent_id,
            );
        }
        EventType::ThreadReleased {
            thread_id,
            agent_id,
        } => {
            println!(
                "{} Thread {} released by {}",
                output::bold("Event"),
                &thread_id[..8.min(thread_id.len())],
                agent_id,
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
    use veld_core::feedback::{new_message, new_thread};

    fn make_store(tmp: &TempDir) -> FeedbackStore {
        FeedbackStore::new(tmp.path(), "test-run")
    }

    fn make_thread_and_event(store: &FeedbackStore, body: &str) -> (Thread, Event) {
        let msg = new_message(Author::Human, body, None, None);
        let thread = new_thread(
            ThreadScope::Global,
            ThreadOrigin::Human,
            None,
            None,
            None,
            msg.clone(),
        );
        store.save_thread(&thread).unwrap();
        let event = store
            .append_event(EventType::ThreadCreated {
                thread: thread.clone(),
            })
            .unwrap();
        (thread, event)
    }

    #[test]
    fn test_batch_returns_all_events() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let (_, _e1) = make_thread_and_event(&store, "Fix button");
        let (_, _e2) = make_thread_and_event(&store, "Fix header");
        let (_, _e3) = make_thread_and_event(&store, "Fix footer");

        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-1", false);

        assert_eq!(result.outputs.len(), 3);
        assert_eq!(result.last_seq, 3);
    }

    #[test]
    fn test_batch_auto_claims_threads() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let (t1, _) = make_thread_and_event(&store, "Fix button");
        let (t2, _) = make_thread_and_event(&store, "Fix header");

        let events = store.get_events_after(0).unwrap();
        let _result = process_batch(&store, events, 0, "agent-1", false);

        // Both threads should be claimed by agent-1.
        let thread1 = store.get_thread(&t1.id).unwrap().unwrap();
        let thread2 = store.get_thread(&t2.id).unwrap().unwrap();
        assert_eq!(thread1.claimed_by.as_deref(), Some("agent-1"));
        assert_eq!(thread2.claimed_by.as_deref(), Some("agent-1"));
    }

    #[test]
    fn test_batch_skips_claimed_threads() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let (t1, _) = make_thread_and_event(&store, "Fix button");
        let (_, _) = make_thread_and_event(&store, "Fix header");

        // Agent-1 claims thread 1.
        store.claim_thread(&t1.id, "agent-1").unwrap();

        // Agent-2 tries to get events — should skip thread 1.
        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-2", false);

        assert_eq!(result.outputs.len(), 1);
        // Should have gotten thread 2 only.
        assert!(matches!(
            &result.outputs[0].event.event_type,
            EventType::ThreadCreated { thread } if thread.messages[0].body == "Fix header"
        ));
    }

    #[test]
    fn test_last_seq_does_not_advance_past_skipped() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let (t1, _) = make_thread_and_event(&store, "Fix button"); // seq 1
        let (_, _) = make_thread_and_event(&store, "Fix header"); // seq 2

        // Agent-1 claims thread at seq 2.
        let t2 = store.list_threads(None).unwrap();
        let t2_thread = t2.iter().find(|t| t.id != t1.id).unwrap();
        store.claim_thread(&t2_thread.id, "agent-1").unwrap();

        // Agent-2 processes batch — gets seq 1 only, skips seq 2.
        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-2", false);

        assert_eq!(result.outputs.len(), 1);
        // last_seq should be 1, NOT 2 — so agent-2 will retry seq 2 later.
        assert_eq!(result.last_seq, 1);
    }

    #[test]
    fn test_no_batch_skips_claimed_finds_next() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let (t1, _) = make_thread_and_event(&store, "Fix button"); // seq 1
        let (_, _) = make_thread_and_event(&store, "Fix header"); // seq 2

        // Agent-1 claims thread 1.
        store.claim_thread(&t1.id, "agent-1").unwrap();

        // Agent-2 in no_batch mode — should skip thread 1, get thread 2.
        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-2", true);

        assert_eq!(result.outputs.len(), 1);
        assert!(matches!(
            &result.outputs[0].event.event_type,
            EventType::ThreadCreated { thread } if thread.messages[0].body == "Fix header"
        ));
    }

    #[test]
    fn test_no_batch_returns_only_one() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        make_thread_and_event(&store, "Fix button");
        make_thread_and_event(&store, "Fix header");
        make_thread_and_event(&store, "Fix footer");

        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-1", true);

        assert_eq!(result.outputs.len(), 1);
    }

    #[test]
    fn test_resolved_events_not_claimed() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let (t1, _) = make_thread_and_event(&store, "Fix button");
        store
            .set_thread_status(&t1.id, ThreadStatus::Resolved)
            .unwrap();
        store
            .append_event(EventType::Resolved {
                thread_id: t1.id.clone(),
            })
            .unwrap();

        // Process from after the ThreadCreated event (seq 1).
        let events = store.get_events_after(1).unwrap();
        let result = process_batch(&store, events, 1, "agent-1", false);

        // Resolved event should be returned but thread should NOT be claimed.
        assert_eq!(result.outputs.len(), 1);
        let thread = store.get_thread(&t1.id).unwrap().unwrap();
        assert!(thread.claimed_by.is_none());
    }

    #[test]
    fn test_session_ended_not_claimed() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store.append_event(EventType::SessionEnded).unwrap();

        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-1", false);

        assert_eq!(result.outputs.len(), 1);
        assert!(matches!(
            result.outputs[0].event.event_type,
            EventType::SessionEnded
        ));
    }

    #[test]
    fn test_dedup_claimed_events_for_same_thread() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        // Create one thread, then add two follow-up messages (3 events for same thread).
        let (t1, _) = make_thread_and_event(&store, "Fix button");
        let msg2 = new_message(Author::Human, "Also fix color", None, None);
        store.add_message(&t1.id, &msg2).unwrap();
        store
            .append_event(EventType::HumanMessage {
                thread_id: t1.id.clone(),
                message: msg2,
            })
            .unwrap();
        let msg3 = new_message(Author::Human, "And the border", None, None);
        store.add_message(&t1.id, &msg3).unwrap();
        store
            .append_event(EventType::HumanMessage {
                thread_id: t1.id.clone(),
                message: msg3,
            })
            .unwrap();

        let seq_before = store.current_seq().unwrap();
        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-1", false);

        // All 3 events should be returned.
        assert_eq!(result.outputs.len(), 3);

        // Only 1 ThreadClaimed event should have been emitted (not 3).
        let seq_after = store.current_seq().unwrap();
        let claim_events: Vec<_> = store
            .get_events_after(seq_before)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.event_type, EventType::ThreadClaimed { .. }))
            .collect();
        assert_eq!(claim_events.len(), 1);
        // Verify no extra events were emitted.
        assert_eq!(seq_after, seq_before + 1);
    }

    #[test]
    fn test_filters_non_human_events() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        // Mix of human and agent events.
        make_thread_and_event(&store, "Fix button"); // seq 1 — human
        store.append_event(EventType::AgentListening).unwrap(); // seq 2 — agent
        make_thread_and_event(&store, "Fix header"); // seq 3 — human

        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-1", false);

        // Should only get the 2 human events, not AgentListening.
        assert_eq!(result.outputs.len(), 2);
    }

    #[test]
    fn test_all_claimed_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let (t1, _) = make_thread_and_event(&store, "Fix button");
        store.claim_thread(&t1.id, "agent-1").unwrap();

        let events = store.get_events_after(0).unwrap();
        let result = process_batch(&store, events, 0, "agent-2", false);

        assert!(result.outputs.is_empty());
        // last_seq should stay at after_seq (0) since nothing was returned.
        assert_eq!(result.last_seq, 0);
    }
}
