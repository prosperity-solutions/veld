use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    /// has no more feedback. Never cleared directly; `is_ended` treats it as
    /// superseded once any human message post-dates it (the reviewer
    /// re-engaged). Distinct from `status`/`last_heartbeat`, which track agent
    /// liveness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// File-based feedback store.
///
/// Layout:
/// ```text
///   .veld/feedback/{run_name}/threads/{uuid}.json
///   .veld/feedback/{run_name}/events/000001.json
///   .veld/feedback/{run_name}/screenshots/
///   .veld/feedback/{run_name}/session.json
///   .veld/feedback/{run_name}/seq
/// ```
pub struct FeedbackStore {
    base: PathBuf,
    threads_dir: PathBuf,
    events_dir: PathBuf,
    screenshots_dir: PathBuf,
    session_path: PathBuf,
    seq_path: PathBuf,
    run_name: String,
}

impl FeedbackStore {
    pub fn new(project_root: &Path, run_name: &str) -> Self {
        let base = project_root.join(".veld").join("feedback").join(run_name);
        Self {
            threads_dir: base.join("threads"),
            events_dir: base.join("events"),
            screenshots_dir: base.join("screenshots"),
            session_path: base.join("session.json"),
            seq_path: base.join("seq"),
            base,
            run_name: run_name.to_owned(),
        }
    }

    /// The run name this store is scoped to.
    pub fn run_name(&self) -> &str {
        &self.run_name
    }

    /// Check whether any feedback data exists for this run.
    pub fn has_data(&self) -> bool {
        self.base.exists()
    }

    fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.threads_dir)?;
        std::fs::create_dir_all(&self.events_dir)?;
        Ok(())
    }

    // -- Threads --------------------------------------------------------------

    /// Save (create or overwrite) a thread.
    pub fn save_thread(&self, thread: &Thread) -> anyhow::Result<()> {
        self.ensure_dirs()?;
        let path = self.threads_dir.join(format!("{}.json", thread.id));
        std::fs::write(&path, serde_json::to_string_pretty(thread)?)?;
        Ok(())
    }

    /// Get a single thread by ID.
    ///
    /// Supports prefix matching: if `id` is shorter than a full UUID and no
    /// exact match exists, scans the threads directory for a unique prefix
    /// match (like git's short commit hashes).
    pub fn get_thread(&self, id: &str) -> anyhow::Result<Option<Thread>> {
        // Try exact match first (fast path).
        let path = self.threads_dir.join(format!("{id}.json"));
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            return Ok(Some(serde_json::from_str(&data)?));
        }

        // Prefix match: scan directory for files starting with the prefix.
        self.resolve_prefix(id)
    }

    /// Resolve a short thread ID prefix to a full thread.
    /// Returns Ok(None) if no match, Ok(Some) if exactly one match,
    /// or Err if the prefix is ambiguous (matches multiple threads).
    fn resolve_prefix(&self, prefix: &str) -> anyhow::Result<Option<Thread>> {
        if !self.threads_dir.exists() || prefix.is_empty() {
            return Ok(None);
        }
        let mut matches: Vec<std::path::PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&self.threads_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(prefix) && name_str.ends_with(".json") {
                matches.push(entry.path());
            }
        }
        match matches.len() {
            0 => Ok(None),
            1 => {
                let data = std::fs::read_to_string(&matches[0])?;
                Ok(Some(serde_json::from_str(&data)?))
            }
            n => anyhow::bail!("ambiguous thread prefix '{prefix}' matches {n} threads"),
        }
    }

    /// Resolve a short thread ID prefix to the full UUID.
    /// Returns the input unchanged if it's already a full UUID or exact match.
    pub fn resolve_thread_id(&self, id: &str) -> anyhow::Result<String> {
        // Exact match?
        let path = self.threads_dir.join(format!("{id}.json"));
        if path.exists() {
            return Ok(id.to_owned());
        }
        // Prefix scan.
        if !self.threads_dir.exists() || id.is_empty() {
            anyhow::bail!("thread {id} not found");
        }
        let mut matches: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&self.threads_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(id) && name_str.ends_with(".json") {
                matches.push(name_str.trim_end_matches(".json").to_owned());
            }
        }
        match matches.len() {
            0 => anyhow::bail!("thread {id} not found"),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => anyhow::bail!("ambiguous thread prefix '{id}' matches {n} threads"),
        }
    }

    /// List all threads, optionally filtered by status.
    pub fn list_threads(&self, filter: Option<ThreadStatus>) -> anyhow::Result<Vec<Thread>> {
        if !self.threads_dir.exists() {
            return Ok(Vec::new());
        }
        let mut threads = Vec::new();
        for entry in std::fs::read_dir(&self.threads_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let data = std::fs::read_to_string(&path)?;
                if let Ok(thread) = serde_json::from_str::<Thread>(&data) {
                    if filter.is_none_or(|f| thread.status == f) {
                        threads.push(thread);
                    }
                }
            }
        }
        threads.sort_by_key(|a| a.created_at);
        Ok(threads)
    }

    /// Read-modify-write a thread file under an exclusive file lock.
    /// Reads and writes through the locked fd to ensure atomicity.
    /// Supports prefix matching for short thread IDs.
    fn modify_thread(
        &self,
        thread_id: &str,
        mutate: impl FnOnce(&mut Thread),
    ) -> anyhow::Result<Thread> {
        self.ensure_dirs()?;
        let resolved_id = self.resolve_thread_id(thread_id)?;
        let path = self.threads_dir.join(format!("{resolved_id}.json"));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|_| anyhow::anyhow!("thread {resolved_id} not found"))?;

        let mut locked = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
            .map_err(|(_file, errno)| errno)?;

        // Read through the locked fd.
        let mut data = String::new();
        locked.read_to_string(&mut data)?;
        let mut thread: Thread = serde_json::from_str(&data)?;
        mutate(&mut thread);
        thread.updated_at = Utc::now();

        // Write through the locked fd.
        let new_data = serde_json::to_string_pretty(&thread)?;
        locked.seek(std::io::SeekFrom::Start(0))?;
        locked.set_len(0)?;
        locked.write_all(new_data.as_bytes())?;

        // Lock released when Flock is dropped.
        Ok(thread)
    }

    /// Add a message to an existing thread. Returns the updated thread.
    pub fn add_message(&self, thread_id: &str, message: &Message) -> anyhow::Result<Thread> {
        let msg = message.clone();
        self.modify_thread(thread_id, move |thread| {
            thread.messages.push(msg);
        })
    }

    /// Set thread status (resolve / reopen). Returns the updated thread.
    pub fn set_thread_status(
        &self,
        thread_id: &str,
        status: ThreadStatus,
    ) -> anyhow::Result<Thread> {
        self.modify_thread(thread_id, move |thread| {
            thread.status = status;
        })
    }

    /// Update `last_human_seen_seq` for a thread.
    pub fn mark_thread_seen(&self, thread_id: &str, seq: u64) -> anyhow::Result<()> {
        self.modify_thread(thread_id, move |thread| {
            thread.last_human_seen_seq = Some(seq);
        })?;
        Ok(())
    }

    /// Return the head of the agent's linear queue: the oldest *waiting* thread.
    ///
    /// A thread is "waiting" when it is Open and its most recent message came
    /// from a human (see [`thread_is_waiting`]). Agent replies flip a thread to
    /// blocked (hidden); a new human message flips it back.
    ///
    /// This is a **pure read** — it never mutates state, so calling it
    /// repeatedly returns the same thread until a `reply`/`resolve` moves the
    /// head. Ordering is FIFO by last-activity: the thread whose last message
    /// is oldest comes first, so a fresh human comment naturally moves its
    /// thread to the back of the queue.
    pub fn next_waiting_thread(&self) -> anyhow::Result<Option<Thread>> {
        let mut waiting: Vec<Thread> = self
            .list_threads(Some(ThreadStatus::Open))?
            .into_iter()
            .filter(thread_is_waiting)
            .collect();
        waiting.sort_by_key(|t| {
            t.messages
                .last()
                .map(|m| m.created_at)
                .unwrap_or(t.created_at)
        });
        Ok(waiting.into_iter().next())
    }

    // -- Event log ------------------------------------------------------------

    /// Atomically increment the sequence counter and return the new value.
    /// Uses an advisory file lock (flock) for cross-process safety.
    fn next_seq(&self) -> anyhow::Result<u64> {
        self.ensure_dirs()?;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.seq_path)?;

        // Exclusive advisory lock — blocks if another process holds it.
        let mut locked = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
            .map_err(|(_file, errno)| errno)?;

        // Read current value.
        let mut contents = String::new();
        locked.read_to_string(&mut contents)?;
        let current: u64 = contents.trim().parse().unwrap_or(0);
        let next = current + 1;

        // Seek to start, truncate, write new value.
        locked.seek(std::io::SeekFrom::Start(0))?;
        locked.set_len(0)?;
        locked.write_all(next.to_string().as_bytes())?;

        // Lock released when Flock is dropped.
        Ok(next)
    }

    /// Append an event to the log. Returns the created event with its seq.
    pub fn append_event(&self, event_type: EventType) -> anyhow::Result<Event> {
        let seq = self.next_seq()?;
        let event = Event {
            seq,
            event_type,
            timestamp: Utc::now(),
        };
        let path = self.events_dir.join(format!("{seq:06}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&event)?)?;
        Ok(event)
    }

    /// Get a single event by sequence number.
    pub fn get_event(&self, seq: u64) -> anyhow::Result<Option<Event>> {
        let path = self.events_dir.join(format!("{seq:06}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&data)?))
    }

    /// Get all events with `seq > after`, sorted ascending.
    /// Probes sequential filenames instead of scanning the directory.
    /// Skips corrupted event files instead of failing.
    pub fn get_events_after(&self, after: u64) -> anyhow::Result<Vec<Event>> {
        if !self.events_dir.exists() {
            return Ok(Vec::new());
        }
        let max_seq = self.current_seq()?;
        let mut events = Vec::new();
        let mut seq = after + 1;
        while seq <= max_seq {
            let path = self.events_dir.join(format!("{seq:06}.json"));
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|data| serde_json::from_str::<Event>(&data).ok())
            {
                Some(event) => events.push(event),
                None => {
                    // File missing or corrupted — skip (gaps from failed writes).
                }
            }
            seq += 1;
        }
        Ok(events)
    }

    /// Get the current (latest) sequence number. Returns 0 if no events.
    pub fn current_seq(&self) -> anyhow::Result<u64> {
        if !self.seq_path.exists() {
            return Ok(0);
        }
        let contents = std::fs::read_to_string(&self.seq_path)?;
        Ok(contents.trim().parse().unwrap_or(0))
    }

    // -- Session / heartbeat --------------------------------------------------

    /// Read-modify-write `session.json` under an exclusive file lock.
    ///
    /// The agent process heartbeats once per second while the daemon handles the
    /// human's "Done" click in a *different* process. Without locking, a
    /// heartbeat's blind write can clobber the `ended_at` flag `end_session` just
    /// set — losing the stop signal so the agent loops forever. This serializes
    /// all session mutations (mirrors `next_seq` / `modify_thread`).
    fn modify_session(&self, mutate: impl FnOnce(&mut Session)) -> anyhow::Result<()> {
        self.ensure_dirs()?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.session_path)?;

        let mut locked = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
            .map_err(|(_file, errno)| errno)?;

        let mut data = String::new();
        locked.read_to_string(&mut data)?;
        let mut session = serde_json::from_str::<Session>(&data).unwrap_or(Session {
            status: SessionStatus::Idle,
            last_heartbeat: Utc::now(),
            ended_at: None,
        });
        mutate(&mut session);

        let new_data = serde_json::to_string_pretty(&session)?;
        locked.seek(std::io::SeekFrom::Start(0))?;
        locked.set_len(0)?;
        locked.write_all(new_data.as_bytes())?;
        Ok(())
    }

    /// Write a heartbeat — marks session as listening with current timestamp.
    /// Preserves the `ended_at` flag: agent liveness and the human's "Done"
    /// signal are independent.
    pub fn heartbeat(&self) -> anyhow::Result<()> {
        self.modify_session(|s| {
            s.status = SessionStatus::Listening;
            s.last_heartbeat = Utc::now();
        })
    }

    /// Read the current session state.
    pub fn get_session(&self) -> anyhow::Result<Option<Session>> {
        if !self.session_path.exists() {
            return Ok(None);
        }
        // A concurrent writer may briefly truncate the file mid-update (see
        // `modify_session`); treat an empty/partial read as "no session yet"
        // rather than erroring, matching how `current_seq` / `list_threads`
        // tolerate torn reads. Readers self-heal on the next poll.
        let data = std::fs::read_to_string(&self.session_path)?;
        Ok(serde_json::from_str(&data).ok())
    }

    /// Check if an agent is actively listening (heartbeat within threshold).
    pub fn is_listening(&self, threshold_secs: u64) -> anyhow::Result<bool> {
        match self.get_session()? {
            Some(session) if session.status == SessionStatus::Listening => {
                let elapsed = Utc::now()
                    .signed_duration_since(session.last_heartbeat)
                    .num_seconds();
                Ok(elapsed >= 0 && (elapsed as u64) < threshold_secs)
            }
            _ => Ok(false),
        }
    }

    /// Explicitly end the session — the human clicked "Done".
    ///
    /// Sets `ended_at`, which `is_ended` reads: once the queue drains, an ended
    /// session tells the agent to stop. It is not cleared — instead `is_ended`
    /// treats a human message newer than `ended_at` as the reviewer re-engaging,
    /// so a post-Done comment revives the loop with no write-ordering race.
    pub fn end_session(&self) -> anyhow::Result<()> {
        let now = Utc::now();
        self.modify_session(move |s| {
            s.status = SessionStatus::Idle;
            s.last_heartbeat = now;
            s.ended_at = Some(now);
        })
    }

    /// Whether the human has ended the session (clicked "Done") and has not
    /// since sent new feedback.
    ///
    /// Timestamp-based rather than a mutable flag: if any open thread carries a
    /// human message newer than `ended_at`, the reviewer re-engaged after
    /// clicking Done, so the session is not ended. This is derived purely from
    /// timestamps, so there is no race between "Done" and a near-simultaneous
    /// new comment arriving on a separate HTTP request — whichever the human
    /// actually did last (by message time) wins.
    pub fn is_ended(&self) -> anyhow::Result<bool> {
        let ended_at = match self.get_session()?.and_then(|s| s.ended_at) {
            Some(t) => t,
            None => return Ok(false),
        };
        for thread in self.list_threads(Some(ThreadStatus::Open))? {
            if thread
                .messages
                .iter()
                .any(|m| matches!(m.author, Author::Human) && m.created_at > ended_at)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // -- Screenshots (unchanged) ----------------------------------------------

    /// Save a screenshot PNG and return its filename.
    ///
    /// The `id` must not contain path separators or `..` sequences.
    pub fn save_screenshot(&self, id: &str, data: &[u8]) -> anyhow::Result<String> {
        anyhow::ensure!(
            !id.contains('/') && !id.contains('\\') && !id.contains(".."),
            "invalid screenshot id"
        );
        std::fs::create_dir_all(&self.screenshots_dir)?;
        let filename = format!("{id}.png");
        let path = self.screenshots_dir.join(&filename);
        std::fs::write(&path, data)?;
        Ok(filename)
    }

    /// Get the absolute path to a screenshot file.
    ///
    /// The `filename` must not contain path separators or `..` sequences.
    pub fn screenshot_path(&self, filename: &str) -> PathBuf {
        let safe = filename.rsplit('/').next().unwrap_or(filename);
        let safe = safe.rsplit('\\').next().unwrap_or(safe);
        let safe = safe.replace("..", "");
        self.screenshots_dir.join(safe)
    }
}

// ---------------------------------------------------------------------------
// Helper: create a new message.
// ---------------------------------------------------------------------------

/// A thread is "waiting" (actionable by the agent) when it is Open and its most
/// recent message came from a human. An agent reply makes the latest author the
/// agent → blocked; a human reply flips it back → waiting. This derived state is
/// the entire machine-side queue model — no stored `blocked` field.
pub fn thread_is_waiting(thread: &Thread) -> bool {
    thread.status == ThreadStatus::Open
        && matches!(thread.messages.last().map(|m| m.author), Some(Author::Human))
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
    use tempfile::TempDir;

    fn make_store(tmp: &TempDir) -> FeedbackStore {
        FeedbackStore::new(tmp.path(), "test-run")
    }

    fn make_thread(body: &str) -> Thread {
        let msg = new_message(Author::Human, body, None, None);
        new_thread(
            ThreadScope::Element {
                page_url: "/dashboard".into(),
                selector: "h1.title".into(),
                position: None,
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

        // Create and save a thread.
        let t = make_thread("Font is too big");
        store.save_thread(&t).unwrap();

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
    }

    #[test]
    fn test_screenshot_unchanged() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let filename = store.save_screenshot("ss_test_001", b"PNG_DATA").unwrap();
        assert_eq!(filename, "ss_test_001.png");

        let path = store.screenshot_path(&filename);
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"PNG_DATA");
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
        };
        let json = serde_json::to_string(&scope).unwrap();
        assert!(json.contains(r#""type":"element"#));
        let _: ThreadScope = serde_json::from_str(&json).unwrap();

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
        let store_path = tmp.path().to_owned();

        let mut handles = Vec::new();
        for _ in 0..4 {
            let p = store_path.clone();
            handles.push(std::thread::spawn(move || {
                let s = FeedbackStore::new(&p, "test-run");
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
        let mut t = new_thread(ThreadScope::Global, ThreadOrigin::Human, None, None, None, msg);
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
    fn test_ended_flag_survives_concurrent_heartbeats() {
        // Regression: the agent process heartbeats every second while the daemon
        // sets `ended_at` on "Done". Without a lock on session.json a heartbeat's
        // write clobbers the flag and the agent never stops. With the lock, no
        // interleaving of concurrent heartbeats can lose an end_session.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_owned();

        let mut handles = Vec::new();
        for _ in 0..4 {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                let s = FeedbackStore::new(&p, "test-run");
                for _ in 0..50 {
                    s.heartbeat().unwrap();
                }
            }));
        }

        // End the session while the heartbeats are in flight.
        FeedbackStore::new(&path, "test-run").end_session().unwrap();

        for h in handles {
            h.join().unwrap();
        }

        // A heartbeat must never have clobbered the Done flag.
        assert!(FeedbackStore::new(&path, "test-run").is_ended().unwrap());
    }

    #[test]
    fn test_skip_dont_crash_on_bad_thread_file() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        // A valid, waiting thread.
        let good = waiting_thread(&store, "Real feedback", 10);

        // A garbage / old-format file sitting alongside it.
        store.ensure_dirs().unwrap();
        std::fs::write(
            store.threads_dir.join("deadbeef.json"),
            b"{ not valid json, from an old version",
        )
        .unwrap();

        // list / next must skip the bad file, not panic.
        let listed = store.list_threads(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(store.next_waiting_thread().unwrap().unwrap().id, good.id);
    }
}
