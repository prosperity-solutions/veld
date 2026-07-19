//! Feedback thread storage, scoped by project + run.
//!
//! Replaces the flock-guarded `.veld/feedback/{run}/` file tree. All the
//! read-modify-write races the file store solved with advisory locks
//! (heartbeat vs "Done", concurrent seq increments) are handled here with
//! `BEGIN IMMEDIATE` transactions instead.

use std::path::Path;

use chrono::Utc;
use rusqlite::{OptionalExtension, params};

use crate::feedback::{Event, EventType, Session, SessionStatus, Thread, ThreadStatus};

use super::state::root_key;
use super::{Db, now_str, parse_ts, ts_to_str};

/// SQLite-backed feedback store for one run.
#[derive(Clone)]
pub struct FeedbackStore {
    db: Db,
    root: String,
    run_name: String,
}

impl FeedbackStore {
    pub fn new(db: Db, project_root: &Path, run_name: &str) -> Self {
        Self {
            db,
            root: root_key(project_root),
            run_name: run_name.to_owned(),
        }
    }

    /// The run name this store is scoped to.
    pub fn run_name(&self) -> &str {
        &self.run_name
    }

    /// Check whether any feedback data exists for this run.
    pub fn has_data(&self) -> bool {
        let conn = self.db.lock();
        let n: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM feedback_threads WHERE project_root=?1 AND run_name=?2)
                      + (SELECT COUNT(*) FROM feedback_events  WHERE project_root=?1 AND run_name=?2)
                      + (SELECT COUNT(*) FROM feedback_sessions WHERE project_root=?1 AND run_name=?2)",
                params![self.root, self.run_name],
                |r| r.get(0),
            )
            .unwrap_or(0);
        n > 0
    }

    /// Delete all feedback data for this run (threads, events, session,
    /// screenshots). Used when a run name is reused.
    pub fn clear(&self) -> anyhow::Result<()> {
        let mut conn = self.db.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for table in [
            "feedback_threads",
            "feedback_events",
            "feedback_sessions",
            "feedback_screenshots",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE project_root=?1 AND run_name=?2"),
                params![self.root, self.run_name],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // -- Threads --------------------------------------------------------------

    /// Save (create or overwrite) a thread.
    pub fn save_thread(&self, thread: &Thread) -> anyhow::Result<()> {
        let conn = self.db.lock();
        conn.execute(
            "INSERT INTO feedback_threads (project_root, run_name, id, payload, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_root, run_name, id) DO UPDATE SET
               payload = excluded.payload, updated_at = excluded.updated_at",
            params![
                self.root,
                self.run_name,
                thread.id,
                serde_json::to_string(thread)?,
                ts_to_str(thread.created_at),
                ts_to_str(thread.updated_at),
            ],
        )?;
        Ok(())
    }

    /// Get a single thread by ID. Supports unique-prefix matching (like git's
    /// short commit hashes).
    pub fn get_thread(&self, id: &str) -> anyhow::Result<Option<Thread>> {
        match self.resolve_thread_id_opt(id)? {
            None => Ok(None),
            Some(full_id) => {
                let conn = self.db.lock();
                let payload: Option<String> = conn
                    .query_row(
                        "SELECT payload FROM feedback_threads
                         WHERE project_root=?1 AND run_name=?2 AND id=?3",
                        params![self.root, self.run_name, full_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok(payload.and_then(|p| serde_json::from_str(&p).ok()))
            }
        }
    }

    /// Resolve a short thread ID prefix to the full ID, or `Ok(None)` when
    /// nothing matches. Errors when the prefix is ambiguous.
    fn resolve_thread_id_opt(&self, id: &str) -> anyhow::Result<Option<String>> {
        if id.is_empty() {
            return Ok(None);
        }
        let conn = self.db.lock();
        // Exact match fast path.
        let exact: Option<String> = conn
            .query_row(
                "SELECT id FROM feedback_threads WHERE project_root=?1 AND run_name=?2 AND id=?3",
                params![self.root, self.run_name, id],
                |r| r.get(0),
            )
            .optional()?;
        if exact.is_some() {
            return Ok(exact);
        }
        // Prefix scan. Escape LIKE wildcards in the (uuid-shaped) prefix.
        let escaped = id
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let mut stmt = conn.prepare_cached(
            "SELECT id FROM feedback_threads
             WHERE project_root=?1 AND run_name=?2 AND id LIKE ?3 ESCAPE '\\' LIMIT 3",
        )?;
        let matches: Vec<String> = stmt
            .query_map(
                params![self.root, self.run_name, format!("{escaped}%")],
                |r| r.get(0),
            )?
            .collect::<Result<_, _>>()?;
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches.into_iter().next().unwrap())),
            n => anyhow::bail!("ambiguous thread prefix '{id}' matches {n} threads"),
        }
    }

    /// Resolve a short thread ID prefix to the full ID, erroring when absent.
    pub fn resolve_thread_id(&self, id: &str) -> anyhow::Result<String> {
        self.resolve_thread_id_opt(id)?
            .ok_or_else(|| anyhow::anyhow!("thread {id} not found"))
    }

    /// List all threads, optionally filtered by status, oldest first.
    pub fn list_threads(&self, filter: Option<ThreadStatus>) -> anyhow::Result<Vec<Thread>> {
        let conn = self.db.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT payload FROM feedback_threads
             WHERE project_root=?1 AND run_name=?2 ORDER BY created_at, id",
        )?;
        let payloads: Vec<String> = stmt
            .query_map(params![self.root, self.run_name], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(payloads
            .iter()
            // Skip rows that no longer parse (old shapes) instead of failing.
            .filter_map(|p| serde_json::from_str::<Thread>(p).ok())
            .filter(|t| filter.is_none_or(|f| t.status == f))
            .collect())
    }

    /// Read-modify-write a thread inside an IMMEDIATE transaction (replaces
    /// the old per-file flock). Supports prefix matching for short IDs.
    fn modify_thread(
        &self,
        thread_id: &str,
        mutate: impl FnOnce(&mut Thread),
    ) -> anyhow::Result<Thread> {
        let resolved_id = self.resolve_thread_id(thread_id)?;
        let mut conn = self.db.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let payload: String = tx
            .query_row(
                "SELECT payload FROM feedback_threads
                 WHERE project_root=?1 AND run_name=?2 AND id=?3",
                params![self.root, self.run_name, resolved_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("thread {resolved_id} not found"))?;

        let mut thread: Thread = serde_json::from_str(&payload)?;
        mutate(&mut thread);
        thread.updated_at = Utc::now();

        tx.execute(
            "UPDATE feedback_threads SET payload=?4, updated_at=?5
             WHERE project_root=?1 AND run_name=?2 AND id=?3",
            params![
                self.root,
                self.run_name,
                resolved_id,
                serde_json::to_string(&thread)?,
                ts_to_str(thread.updated_at),
            ],
        )?;
        tx.commit()?;
        Ok(thread)
    }

    /// Add a message to an existing thread. Returns the updated thread.
    pub fn add_message(
        &self,
        thread_id: &str,
        message: &crate::feedback::Message,
    ) -> anyhow::Result<Thread> {
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
            // Never lower the seen count: a stale/racing write (multi-tab, or a
            // late `markAllRead` after a per-thread PUT) must not resurrect unread.
            let seen = seq.max(thread.last_human_seen_seq.unwrap_or(0));
            thread.last_human_seen_seq = Some(seen);
        })?;
        Ok(())
    }

    /// Return the head of the agent's linear queue: the oldest *waiting* thread.
    ///
    /// A thread is "waiting" when it is Open and its most recent message came
    /// from a human (see [`crate::feedback::thread_is_waiting`]). This is a
    /// pure read — calling it repeatedly returns the same thread until a
    /// `reply`/`resolve` moves the head. FIFO by last-activity.
    pub fn next_waiting_thread(&self) -> anyhow::Result<Option<Thread>> {
        let mut waiting: Vec<Thread> = self
            .list_threads(Some(ThreadStatus::Open))?
            .into_iter()
            .filter(crate::feedback::thread_is_waiting)
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

    /// Append an event to the log. The sequence number is allocated inside
    /// the same transaction as the insert, so it is race-free across
    /// processes without a counter file.
    pub fn append_event(&self, event_type: EventType) -> anyhow::Result<Event> {
        let mut conn = self.db.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM feedback_events
             WHERE project_root=?1 AND run_name=?2",
            params![self.root, self.run_name],
            |r| r.get(0),
        )?;
        let event = Event {
            seq: seq as u64,
            event_type,
            timestamp: Utc::now(),
        };
        tx.execute(
            "INSERT INTO feedback_events (project_root, run_name, seq, payload, ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                self.root,
                self.run_name,
                seq,
                serde_json::to_string(&event)?,
                ts_to_str(event.timestamp),
            ],
        )?;
        tx.commit()?;
        Ok(event)
    }

    /// Get a single event by sequence number.
    pub fn get_event(&self, seq: u64) -> anyhow::Result<Option<Event>> {
        let conn = self.db.lock();
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload FROM feedback_events
                 WHERE project_root=?1 AND run_name=?2 AND seq=?3",
                params![self.root, self.run_name, seq as i64],
                |r| r.get(0),
            )
            .optional()?;
        Ok(payload.and_then(|p| serde_json::from_str(&p).ok()))
    }

    /// Get all events with `seq > after`, sorted ascending. Events that no
    /// longer parse (old shapes) are skipped instead of failing.
    pub fn get_events_after(&self, after: u64) -> anyhow::Result<Vec<Event>> {
        let conn = self.db.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT payload FROM feedback_events
             WHERE project_root=?1 AND run_name=?2 AND seq > ?3 ORDER BY seq",
        )?;
        let payloads: Vec<String> = stmt
            .query_map(params![self.root, self.run_name, after as i64], |r| {
                r.get(0)
            })?
            .collect::<Result<_, _>>()?;
        Ok(payloads
            .iter()
            .filter_map(|p| serde_json::from_str::<Event>(p).ok())
            .collect())
    }

    /// Get the current (latest) sequence number. Returns 0 if no events.
    pub fn current_seq(&self) -> anyhow::Result<u64> {
        let conn = self.db.lock();
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM feedback_events
             WHERE project_root=?1 AND run_name=?2",
            params![self.root, self.run_name],
            |r| r.get(0),
        )?;
        Ok(seq as u64)
    }

    // -- Session / heartbeat --------------------------------------------------

    /// Read-modify-write the session row inside an IMMEDIATE transaction.
    ///
    /// The agent process heartbeats once per second while the daemon handles
    /// the human's "Done" click in a *different* process. The transaction
    /// guarantees a heartbeat can never clobber the `ended_at` flag.
    fn modify_session(&self, mutate: impl FnOnce(&mut Session)) -> anyhow::Result<()> {
        let mut conn = self.db.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT status, last_heartbeat, ended_at FROM feedback_sessions
                 WHERE project_root=?1 AND run_name=?2",
                params![self.root, self.run_name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let mut session = match existing {
            Some((status, heartbeat, ended_at)) => Session {
                status: if status == "listening" {
                    SessionStatus::Listening
                } else {
                    SessionStatus::Idle
                },
                last_heartbeat: parse_ts(&heartbeat).unwrap_or_else(Utc::now),
                ended_at: ended_at.as_deref().and_then(parse_ts),
            },
            None => Session {
                status: SessionStatus::Idle,
                last_heartbeat: Utc::now(),
                ended_at: None,
            },
        };
        mutate(&mut session);

        tx.execute(
            "INSERT INTO feedback_sessions (project_root, run_name, status, last_heartbeat, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_root, run_name) DO UPDATE SET
               status = excluded.status,
               last_heartbeat = excluded.last_heartbeat,
               ended_at = excluded.ended_at",
            params![
                self.root,
                self.run_name,
                match session.status {
                    SessionStatus::Listening => "listening",
                    SessionStatus::Idle => "idle",
                },
                ts_to_str(session.last_heartbeat),
                session.ended_at.map(ts_to_str),
            ],
        )?;
        tx.commit()?;
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
        let conn = self.db.lock();
        let row: Option<(String, String, Option<String>)> = conn
            .query_row(
                "SELECT status, last_heartbeat, ended_at FROM feedback_sessions
                 WHERE project_root=?1 AND run_name=?2",
                params![self.root, self.run_name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(row.map(|(status, heartbeat, ended_at)| Session {
            status: if status == "listening" {
                SessionStatus::Listening
            } else {
                SessionStatus::Idle
            },
            last_heartbeat: parse_ts(&heartbeat).unwrap_or_else(Utc::now),
            ended_at: ended_at.as_deref().and_then(parse_ts),
        }))
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
    /// Sets `ended_at`, which `is_ended` reads. When the agent's queue drains,
    /// an ended session tells it to stop; the agent then calls
    /// [`mark_stopped`](Self::mark_stopped) as it exits. A human message newer
    /// than `ended_at` also supersedes it, so a post-Done comment revives the
    /// loop.
    pub fn end_session(&self) -> anyhow::Result<()> {
        let now = Utc::now();
        self.modify_session(move |s| {
            s.status = SessionStatus::Idle;
            s.last_heartbeat = now;
            s.ended_at = Some(now);
        })
    }

    /// Mark the agent as no longer listening — called as it reports the
    /// `ended` stop and exits. Sets status to Idle and consumes `ended_at`
    /// (so a freshly-launched loop starts clean).
    pub fn mark_stopped(&self) -> anyhow::Result<()> {
        self.modify_session(|s| {
            s.status = SessionStatus::Idle;
            s.ended_at = None;
        })
    }

    /// Whether the human has ended the session (clicked "Done") and has not
    /// since sent new feedback. Timestamp-derived, so there is no race between
    /// "Done" and a near-simultaneous new comment — whichever the human
    /// actually did last (by message time) wins.
    pub fn is_ended(&self) -> anyhow::Result<bool> {
        let ended_at = match self.get_session()?.and_then(|s| s.ended_at) {
            Some(t) => t,
            None => return Ok(false),
        };
        for thread in self.list_threads(Some(ThreadStatus::Open))? {
            if thread.messages.iter().any(|m| {
                matches!(m.author, crate::feedback::Author::Human) && m.created_at > ended_at
            }) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // -- Screenshots ------------------------------------------------------------

    /// Save a screenshot PNG and return its filename.
    pub fn save_screenshot(&self, id: &str, data: &[u8]) -> anyhow::Result<String> {
        anyhow::ensure!(
            !id.contains('/') && !id.contains('\\') && !id.contains(".."),
            "invalid screenshot id"
        );
        let filename = format!("{id}.png");
        let conn = self.db.lock();
        conn.execute(
            "INSERT INTO feedback_screenshots (project_root, run_name, filename, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_root, run_name, filename) DO UPDATE SET data = excluded.data",
            params![self.root, self.run_name, filename, data, now_str()],
        )?;
        Ok(filename)
    }

    /// Load a screenshot's bytes by filename.
    pub fn get_screenshot(&self, filename: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let conn = self.db.lock();
        let data: Option<Vec<u8>> = conn
            .query_row(
                "SELECT data FROM feedback_screenshots
                 WHERE project_root=?1 AND run_name=?2 AND filename=?3",
                params![self.root, self.run_name, filename],
                |r| r.get(0),
            )
            .optional()?;
        Ok(data)
    }
}

impl Db {
    /// Prune feedback data for runs that no longer exist and whose last
    /// activity is older than `cutoff` (GC housekeeping so BLOBs don't
    /// accumulate forever).
    pub fn prune_orphaned_feedback(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, super::DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let cutoff = ts_to_str(cutoff);
        // Scopes with no matching run and no thread updated since the cutoff.
        let scopes: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT f.project_root, f.run_name
                 FROM feedback_threads f
                 LEFT JOIN runs r ON r.project_root = f.project_root AND r.name = f.run_name
                 WHERE r.id IS NULL
                 GROUP BY f.project_root, f.run_name
                 HAVING MAX(f.updated_at) < ?1",
            )?;
            stmt.query_map([&cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        for (root, run) in &scopes {
            for table in [
                "feedback_threads",
                "feedback_events",
                "feedback_sessions",
                "feedback_screenshots",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE project_root=?1 AND run_name=?2"),
                    params![root, run],
                )?;
            }
        }
        tx.commit()?;
        Ok(scopes.len())
    }
}
