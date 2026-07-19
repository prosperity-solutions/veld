//! Feedback thread data types and queue semantics.
//!
//! Storage lives in the central database — see [`crate::db`]. The
//! [`FeedbackStore`] (re-exported here) replaces the old flock-guarded
//! `.veld/feedback/{run}/` file tree; SQLite transactions provide the
//! cross-process safety the advisory locks used to.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::db::feedback::FeedbackStore;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Where a thread is anchored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ThreadScope {
    /// Pinned to a specific element on a page.
    Element {
        page_url: String,
        selector: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<ElementPosition>,
        /// Visible text of the element, middle-truncated by the client —
        /// helps an agent disambiguate when the selector alone is ambiguous.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_text: Option<String>,
        /// Source file of the element's JSX/template tag, when the
        /// framework's dev build exposes it (React `_debugSource`, Vue
        /// `__file`). Best-effort — absent in production builds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_line: Option<u32>,
    },
    /// Attached to a page but not a specific element.
    Page { page_url: String },
    /// Not attached to any page — global feedback.
    Global,
}

/// Bounding-box position for an element on page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ElementPosition {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

// Manual Eq: f64 doesn't implement Eq but we need it for ThreadScope.
// Positions are stored as-is and never compared for equality in practice.
impl Eq for ElementPosition {}

/// Who created a thread.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadOrigin {
    Human,
    Agent,
}

/// Whether a thread is open or resolved.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Open,
    Resolved,
}

/// Author of a message within a thread.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Human,
    Agent,
}

/// A single message within a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub author: Author,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// Interactive controls (sliders, pickers, etc.) attached to this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// A feedback thread — a conversation pinned to an element, page, or global.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub scope: ThreadScope,
    pub origin: ThreadOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_trace: Option<Vec<String>>,
    pub status: ThreadStatus,
    pub messages: Vec<Message>,
    /// The seq of the last message the human has viewed in this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_human_seen_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_height: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// A single event in the append-only log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    #[serde(flatten)]
    pub event_type: EventType,
    pub timestamp: DateTime<Utc>,
}

/// The type of event that occurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum EventType {
    // -- Human → Agent --
    ThreadCreated { thread: Thread },
    HumanMessage { thread_id: String, message: Message },
    Resolved { thread_id: String },
    Reopened { thread_id: String },
    SessionEnded,

    // -- Agent → Browser --
    AgentMessage { thread_id: String, message: Message },
    AgentThreadCreated { thread: Thread },
    AgentListening,
    AgentStopped,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Listening,
    Idle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub status: SessionStatus,
    pub last_heartbeat: DateTime<Utc>,
    /// Set when the human clicks "Done" — a durable signal that the reviewer
    /// has no more feedback. Consumed (cleared) by the agent when it reports the
    /// `ended` stop, so a relaunched loop starts clean; also treated as
    /// superseded by any newer human message (the reviewer re-engaged). Distinct
    /// from `status`/`last_heartbeat`, which track agent liveness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A thread is "waiting" (actionable by the agent) when it is Open and its most
/// recent message came from a human. An agent reply makes the latest author the
/// agent → blocked; a human reply flips it back → waiting. This derived state is
/// the entire machine-side queue model — no stored `blocked` field.
pub fn thread_is_waiting(thread: &Thread) -> bool {
    thread.status == ThreadStatus::Open
        && matches!(
            thread.messages.last().map(|m| m.author),
            Some(Author::Human)
        )
}

pub fn new_message(
    author: Author,
    body: &str,
    screenshot: Option<String>,
    controls: Option<serde_json::Value>,
) -> Message {
    Message {
        id: Uuid::new_v4().to_string(),
        author,
        body: body.to_owned(),
        screenshot,
        controls,
        created_at: Utc::now(),
    }
}

/// Create a new thread with an initial message.
pub fn new_thread(
    scope: ThreadScope,
    origin: ThreadOrigin,
    component_trace: Option<Vec<String>>,
    viewport_width: Option<u32>,
    viewport_height: Option<u32>,
    initial_message: Message,
) -> Thread {
    let now = Utc::now();
    Thread {
        id: Uuid::new_v4().to_string(),
        scope,
        origin,
        component_trace,
        status: ThreadStatus::Open,
        messages: vec![initial_message],
        last_human_seen_seq: None,
        viewport_width,
        viewport_height,
        created_at: now,
        updated_at: now,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use tempfile::TempDir;

    fn make_db(tmp: &TempDir) -> Db {
        Db::open_at(&tmp.path().join("veld.db")).unwrap()
    }

    fn make_store(tmp: &TempDir) -> FeedbackStore {
        FeedbackStore::new(make_db(tmp), tmp.path(), "test-run")
    }

    fn make_thread(body: &str) -> Thread {
        let msg = new_message(Author::Human, body, None, None);
        new_thread(
            ThreadScope::Element {
                page_url: "/dashboard".into(),
                selector: "h1.title".into(),
                position: None,
                element_text: None,
                source_file: None,
                source_line: None,
            },
            ThreadOrigin::Human,
            Some(vec!["App".into(), "Header".into()]),
            Some(1440),
            Some(900),
            msg,
        )
    }

    #[test]
    fn test_thread_crud() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        // No threads initially.
        assert!(store.list_threads(None).unwrap().is_empty());
        assert!(!store.has_data());

        // Create and save a thread.
        let t = make_thread("Font is too big");
        store.save_thread(&t).unwrap();
        assert!(store.has_data());

        // Retrieve by ID.
        let fetched = store.get_thread(&t.id).unwrap().unwrap();
        assert_eq!(fetched.id, t.id);
        assert_eq!(fetched.messages.len(), 1);
        assert_eq!(fetched.messages[0].body, "Font is too big");
        assert_eq!(fetched.status, ThreadStatus::Open);

        // List all.
        let all = store.list_threads(None).unwrap();
        assert_eq!(all.len(), 1);

        // Filter by status.
        assert_eq!(
            store.list_threads(Some(ThreadStatus::Open)).unwrap().len(),
            1
        );
        assert_eq!(
            store
                .list_threads(Some(ThreadStatus::Resolved))
                .unwrap()
                .len(),
            0
        );

        // Non-existent thread.
        assert!(store.get_thread("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_thread_prefix_matching() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let t = make_thread("Prefix me");
        store.save_thread(&t).unwrap();

        let prefix = &t.id[..8];
        assert_eq!(store.get_thread(prefix).unwrap().unwrap().id, t.id);
        assert_eq!(store.resolve_thread_id(prefix).unwrap(), t.id);
        assert!(store.resolve_thread_id("zzzz").is_err());
    }

    #[test]
    fn test_add_message() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let t = make_thread("Fix padding");
        store.save_thread(&t).unwrap();

        // Agent replies.
        let reply = new_message(Author::Agent, "Fixed — reduced to 1.5rem", None, None);
        let updated = store.add_message(&t.id, &reply).unwrap();
        assert_eq!(updated.messages.len(), 2);
        assert_eq!(updated.messages[1].author, Author::Agent);
        assert_eq!(updated.messages[1].body, "Fixed — reduced to 1.5rem");

        // Human follows up.
        let followup = new_message(Author::Human, "Looks good, thanks", None, None);
        let updated = store.add_message(&t.id, &followup).unwrap();
        assert_eq!(updated.messages.len(), 3);

        // Verify persistence.
        let reloaded = store.get_thread(&t.id).unwrap().unwrap();
        assert_eq!(reloaded.messages.len(), 3);
    }

    #[test]
    fn test_thread_status() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let t = make_thread("Color is off");
        store.save_thread(&t).unwrap();
        assert_eq!(
            store.get_thread(&t.id).unwrap().unwrap().status,
            ThreadStatus::Open
        );

        // Resolve.
        store
            .set_thread_status(&t.id, ThreadStatus::Resolved)
            .unwrap();
        assert_eq!(
            store.get_thread(&t.id).unwrap().unwrap().status,
            ThreadStatus::Resolved
        );

        // Reopen.
        store.set_thread_status(&t.id, ThreadStatus::Open).unwrap();
        assert_eq!(
            store.get_thread(&t.id).unwrap().unwrap().status,
            ThreadStatus::Open
        );

        // Filter.
        let resolved = store.list_threads(Some(ThreadStatus::Resolved)).unwrap();
        assert!(resolved.is_empty());
    }

    #[test]
    fn test_event_append_and_read() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let t = make_thread("Test thread");
        let e1 = store
            .append_event(EventType::ThreadCreated { thread: t.clone() })
            .unwrap();
        assert_eq!(e1.seq, 1);

        let msg = new_message(Author::Human, "Follow-up", None, None);
        let e2 = store
            .append_event(EventType::HumanMessage {
                thread_id: t.id.clone(),
                message: msg,
            })
            .unwrap();
        assert_eq!(e2.seq, 2);

        let e3 = store
            .append_event(EventType::Resolved {
                thread_id: t.id.clone(),
            })
            .unwrap();
        assert_eq!(e3.seq, 3);

        // Read all.
        let all = store.get_events_after(0).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].seq, 1);
        assert_eq!(all[2].seq, 3);

        // Read after seq 1.
        let after1 = store.get_events_after(1).unwrap();
        assert_eq!(after1.len(), 2);
        assert_eq!(after1[0].seq, 2);

        // Read after seq 3 (none).
        let after3 = store.get_events_after(3).unwrap();
        assert!(after3.is_empty());

        // Get single event.
        let single = store.get_event(2).unwrap().unwrap();
        assert_eq!(single.seq, 2);
        assert!(store.get_event(99).unwrap().is_none());
    }

    #[test]
    fn test_seq_counter() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        assert_eq!(store.current_seq().unwrap(), 0);

        for i in 1..=10 {
            let t = make_thread(&format!("Thread {i}"));
            let event = store
                .append_event(EventType::ThreadCreated { thread: t })
                .unwrap();
            assert_eq!(event.seq, i);
        }

        assert_eq!(store.current_seq().unwrap(), 10);
    }

    #[test]
    fn test_session_heartbeat() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        // No session initially.
        assert!(store.get_session().unwrap().is_none());
        assert!(!store.is_listening(60).unwrap());

        // Heartbeat.
        store.heartbeat().unwrap();
        let session = store.get_session().unwrap().unwrap();
        assert_eq!(session.status, SessionStatus::Listening);
        assert!(store.is_listening(60).unwrap());

        // End session.
        store.end_session().unwrap();
        assert!(!store.is_listening(60).unwrap());
        let session = store.get_session().unwrap().unwrap();
        assert_eq!(session.status, SessionStatus::Idle);
    }

    #[test]
    fn test_mark_thread_seen() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let t = make_thread("Feedback");
        store.save_thread(&t).unwrap();

        assert!(
            store
                .get_thread(&t.id)
                .unwrap()
                .unwrap()
                .last_human_seen_seq
                .is_none()
        );

        store.mark_thread_seen(&t.id, 5).unwrap();
        assert_eq!(
            store
                .get_thread(&t.id)
                .unwrap()
                .unwrap()
                .last_human_seen_seq,
            Some(5)
        );

        // A racing lower value never lowers the seen count.
        store.mark_thread_seen(&t.id, 3).unwrap();
        assert_eq!(
            store
                .get_thread(&t.id)
                .unwrap()
                .unwrap()
                .last_human_seen_seq,
            Some(5)
        );
    }

    #[test]
    fn test_screenshot_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let filename = store.save_screenshot("ss_test_001", b"PNG_DATA").unwrap();
        assert_eq!(filename, "ss_test_001.png");
        assert_eq!(
            store.get_screenshot(&filename).unwrap().unwrap(),
            b"PNG_DATA"
        );
        assert!(store.get_screenshot("missing.png").unwrap().is_none());
        assert!(store.save_screenshot("../evil", b"x").is_err());
    }

    #[test]
    fn test_serde_event_types() {
        // ThreadCreated
        let t = make_thread("Test");
        let event = Event {
            seq: 1,
            event_type: EventType::ThreadCreated { thread: t },
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"thread_created"#));
        let _: Event = serde_json::from_str(&json).unwrap();

        // SessionEnded
        let event = Event {
            seq: 2,
            event_type: EventType::SessionEnded,
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"session_ended"#));
        let _: Event = serde_json::from_str(&json).unwrap();

        // AgentListening
        let event = Event {
            seq: 3,
            event_type: EventType::AgentListening,
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"agent_listening"#));
        let _: Event = serde_json::from_str(&json).unwrap();

        // Resolved
        let event = Event {
            seq: 4,
            event_type: EventType::Resolved {
                thread_id: "t_123".into(),
            },
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"resolved"#));
        assert!(json.contains(r#""thread_id":"t_123"#));
        let roundtrip: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.seq, 4);
    }

    #[test]
    fn test_thread_scopes() {
        // Element scope.
        let scope = ThreadScope::Element {
            page_url: "/test".into(),
            selector: "div.main".into(),
            position: Some(ElementPosition {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
            }),
            element_text: Some("Submit".into()),
            source_file: Some("src/components/Button.tsx".into()),
            source_line: Some(42),
        };
        let json = serde_json::to_string(&scope).unwrap();
        assert!(json.contains(r#""type":"element"#));
        let roundtrip: ThreadScope = serde_json::from_str(&json).unwrap();
        match roundtrip {
            ThreadScope::Element {
                element_text,
                source_file,
                source_line,
                ..
            } => {
                assert_eq!(element_text.as_deref(), Some("Submit"));
                assert_eq!(source_file.as_deref(), Some("src/components/Button.tsx"));
                assert_eq!(source_line, Some(42));
            }
            _ => panic!("expected ThreadScope::Element"),
        }

        // Page scope.
        let scope = ThreadScope::Page {
            page_url: "/dashboard".into(),
        };
        let json = serde_json::to_string(&scope).unwrap();
        assert!(json.contains(r#""type":"page"#));
        let _: ThreadScope = serde_json::from_str(&json).unwrap();

        // Global scope.
        let scope = ThreadScope::Global;
        let json = serde_json::to_string(&scope).unwrap();
        assert!(json.contains(r#""type":"global"#));
        let _: ThreadScope = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_concurrent_seq() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let store_path = tmp.path().to_owned();

        let mut handles = Vec::new();
        for _ in 0..4 {
            let p = store_path.clone();
            let db = db.clone();
            handles.push(std::thread::spawn(move || {
                let s = FeedbackStore::new(db, &p, "test-run");
                let mut seqs = Vec::new();
                for _ in 0..25 {
                    let t = make_thread("concurrent");
                    let event = s
                        .append_event(EventType::ThreadCreated { thread: t })
                        .unwrap();
                    seqs.push(event.seq);
                }
                seqs
            }));
        }

        let mut all_seqs: Vec<u64> = Vec::new();
        for h in handles {
            all_seqs.extend(h.join().unwrap());
        }
        all_seqs.sort();

        // 4 threads × 25 events = 100 events, all unique seqs.
        assert_eq!(all_seqs.len(), 100);
        all_seqs.dedup();
        assert_eq!(all_seqs.len(), 100);

        // Should be 1..=100 with no gaps.
        assert_eq!(all_seqs[0], 1);
        assert_eq!(*all_seqs.last().unwrap(), 100);
    }

    /// Build a Human-authored waiting thread with an explicit age so queue
    /// ordering tests are deterministic.
    fn waiting_thread(store: &FeedbackStore, body: &str, age_secs: i64) -> Thread {
        let ts = Utc::now() - chrono::Duration::seconds(age_secs);
        let msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            author: Author::Human,
            body: body.to_owned(),
            screenshot: None,
            controls: None,
            created_at: ts,
        };
        let mut t = new_thread(
            ThreadScope::Global,
            ThreadOrigin::Human,
            None,
            None,
            None,
            msg,
        );
        t.created_at = ts;
        t.updated_at = ts;
        store.save_thread(&t).unwrap();
        t
    }

    #[test]
    fn test_thread_is_waiting() {
        let mut t = make_thread("Human said something");
        // Open + last author human → waiting.
        assert!(thread_is_waiting(&t));

        // Agent replies → blocked.
        t.messages
            .push(new_message(Author::Agent, "Done", None, None));
        assert!(!thread_is_waiting(&t));

        // Human follows up → waiting again.
        t.messages
            .push(new_message(Author::Human, "Not quite", None, None));
        assert!(thread_is_waiting(&t));

        // Resolved → never waiting, even with a trailing human message.
        t.status = ThreadStatus::Resolved;
        assert!(!thread_is_waiting(&t));
    }

    #[test]
    fn test_next_waiting_thread_blocks_and_unblocks() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let t = waiting_thread(&store, "Fix the button", 10);

        // Human comment is at the head.
        let head = store.next_waiting_thread().unwrap().unwrap();
        assert_eq!(head.id, t.id);

        // Pure read: calling again returns the same head (no side effects).
        let head2 = store.next_waiting_thread().unwrap().unwrap();
        assert_eq!(head2.id, t.id);

        // Agent replies → thread becomes blocked → queue empties.
        store
            .add_message(&t.id, &new_message(Author::Agent, "Fixed", None, None))
            .unwrap();
        assert!(store.next_waiting_thread().unwrap().is_none());

        // Human replies → unblocked → back at the head.
        store
            .add_message(&t.id, &new_message(Author::Human, "Still off", None, None))
            .unwrap();
        assert_eq!(store.next_waiting_thread().unwrap().unwrap().id, t.id);
    }

    #[test]
    fn test_next_waiting_fifo_and_moves_to_back() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let older = waiting_thread(&store, "Older", 100);
        let newer = waiting_thread(&store, "Newer", 50);

        // Oldest last-activity first.
        assert_eq!(store.next_waiting_thread().unwrap().unwrap().id, older.id);

        // A fresh human comment on the older thread moves it to the back.
        store
            .add_message(&older.id, &new_message(Author::Human, "More", None, None))
            .unwrap();
        assert_eq!(store.next_waiting_thread().unwrap().unwrap().id, newer.id);
    }

    #[test]
    fn test_next_waiting_ignores_resolved() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let t = waiting_thread(&store, "Approved", 10);
        store
            .set_thread_status(&t.id, ThreadStatus::Resolved)
            .unwrap();
        assert!(store.next_waiting_thread().unwrap().is_none());
    }

    #[test]
    fn test_ended_flag_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        assert!(!store.is_ended().unwrap());

        // Human clicks Done.
        store.end_session().unwrap();
        assert!(store.is_ended().unwrap());

        // Agent heartbeat must not clobber the Done flag.
        store.heartbeat().unwrap();
        assert!(store.is_ended().unwrap());

        // New human feedback after Done (message post-dates ended_at) → the
        // reviewer re-engaged, so the session is no longer ended.
        let t = waiting_thread(&store, "one more thing", -1);
        assert!(!store.is_ended().unwrap());

        // Agent replies (thread becomes blocked) but the human message still
        // post-dates Done, so the conversation is ongoing — still not ended.
        store
            .add_message(&t.id, &new_message(Author::Agent, "on it", None, None))
            .unwrap();
        assert!(!store.is_ended().unwrap());
    }

    #[test]
    fn test_mark_stopped_consumes_flag_and_stops_listening() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store.heartbeat().unwrap();
        store.end_session().unwrap();
        assert!(store.is_ended().unwrap());

        // Agent reports the stop and exits: the session is no longer ended (so a
        // relaunched loop won't immediately re-stop) and no longer listening (so
        // /session won't re-announce the now-exited agent).
        store.mark_stopped().unwrap();
        assert!(!store.is_ended().unwrap());
        assert!(!store.is_listening(60).unwrap());
    }

    #[test]
    fn test_ended_flag_survives_concurrent_heartbeats() {
        // Regression: the agent process heartbeats every second while the daemon
        // sets `ended_at` on "Done". The IMMEDIATE transaction in
        // `modify_session` guarantees no interleaving of concurrent heartbeats
        // can lose an end_session.
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let path = tmp.path().to_owned();

        let mut handles = Vec::new();
        for _ in 0..4 {
            let p = path.clone();
            let db = db.clone();
            handles.push(std::thread::spawn(move || {
                let s = FeedbackStore::new(db, &p, "test-run");
                for _ in 0..50 {
                    s.heartbeat().unwrap();
                }
            }));
        }

        // End the session while the heartbeats are in flight.
        FeedbackStore::new(db.clone(), &path, "test-run")
            .end_session()
            .unwrap();

        for h in handles {
            h.join().unwrap();
        }

        // A heartbeat must never have clobbered the Done flag.
        assert!(
            FeedbackStore::new(db, &path, "test-run")
                .is_ended()
                .unwrap()
        );
    }

    #[test]
    fn test_clear_removes_everything() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let t = waiting_thread(&store, "Real feedback", 10);
        store
            .append_event(EventType::ThreadCreated { thread: t })
            .unwrap();
        store.heartbeat().unwrap();
        store.save_screenshot("ss1", b"png").unwrap();
        assert!(store.has_data());

        store.clear().unwrap();
        assert!(!store.has_data());
        assert!(store.list_threads(None).unwrap().is_empty());
        assert_eq!(store.current_seq().unwrap(), 0);
        assert!(store.get_screenshot("ss1.png").unwrap().is_none());
    }

    #[test]
    fn test_skip_dont_crash_on_bad_thread_payload() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let store = FeedbackStore::new(db.clone(), tmp.path(), "test-run");

        // A valid, waiting thread.
        let good = waiting_thread(&store, "Real feedback", 10);

        // A garbage / old-format payload sitting alongside it.
        db.lock()
            .execute(
                "INSERT INTO feedback_threads (project_root, run_name, id, payload, created_at, updated_at)
                 VALUES (?1, 'test-run', 'deadbeef', '{ not valid json', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
                [tmp.path().to_string_lossy()],
            )
            .unwrap();

        // list / next must skip the bad payload, not panic.
        let listed = store.list_threads(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(store.next_waiting_thread().unwrap().unwrap().id, good.id);
    }
}
