//! Pane session identity — the token a config-declared terminal pane hands to
//! the tool it runs.
//!
//! The point of the whole table is one illusion: a coding-agent CLI keeps its
//! own conversation transcript keyed by a session id it is told, so a pane that
//! re-runs the tool's *resume* command with the same id looks like it survived a
//! reboot. The shell did not survive — nothing can carry a process across one —
//! but the conversation did, which is the part the user cares about.
//!
//! Two rules run through this module:
//!
//! - **A row means "this pane launched something".** Nothing writes a row before
//!   a holder has actually spawned, so the presence of one is what makes a pane
//!   resume-eligible. Reading a token for a pane that never ran would offer the
//!   user a resume that can only fail.
//! - **A fresh launch always mints a fresh token.** `--session-id` is a *create*
//!   with an id you chose, so reusing one is at best refused and at worst
//!   ambiguous. [`Db::record_pane_session`] therefore replaces the row rather
//!   than reading it — which is also how "start fresh" is spelled: the upsert,
//!   not a delete.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, DbError, ts_to_str};

/// How many launched panes one worktree remembers. Generous next to any real
/// layout — see [`Db::prune_pane_sessions`].
const MAX_PANE_SESSIONS_PER_WORKTREE: i64 = 200;

/// A fresh pane identity.
///
/// A UUID because that is the shape the tools this exists for accept —
/// `claude --session-id` refuses anything else — and because it has to be
/// unguessable enough that two panes never collide on one transcript.
#[must_use]
pub fn mint_pane_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// What a pane launched, and under which identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSession {
    /// The PTY session id, which is also the client's pane tab id.
    pub session_id: String,
    pub worktree_id: i64,
    /// The `ide.panes[].id` this pane was created from.
    pub spec_id: String,
    /// The identity handed to the tool. Never serialised to a client.
    pub token: String,
}

impl Db {
    /// Record a fresh launch, discarding any previous identity for this pane.
    ///
    /// Called **after** the holder has spawned, never before: a row written
    /// optimistically would survive a failed spawn and leave the pane offering a
    /// resume for a conversation the tool never created. That ordering is also
    /// why the token is minted by the caller ([`mint_pane_token`]) rather than
    /// here — it has to exist early enough to be interpolated into the command,
    /// and be written only once that command is running.
    pub fn record_pane_session(
        &self,
        session_id: &str,
        worktree_id: i64,
        spec_id: &str,
        token: &str,
    ) -> Result<PaneSession, DbError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO pane_sessions (session_id, worktree_id, spec_id, token, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET
                 worktree_id = excluded.worktree_id,
                 spec_id     = excluded.spec_id,
                 token       = excluded.token,
                 created_at  = excluded.created_at",
            params![
                session_id,
                worktree_id,
                spec_id,
                token,
                ts_to_str(chrono::Utc::now())
            ],
        )?;
        // After the insert, not before: pruning first leaves the row just added
        // sitting on top of a full set, so the steady state would be one over.
        Self::prune_pane_sessions(&conn, worktree_id)?;
        Ok(PaneSession {
            session_id: session_id.to_owned(),
            worktree_id,
            spec_id: spec_id.to_owned(),
            token: token.to_owned(),
        })
    }

    /// The identity a pane launched under, if it ever launched.
    pub fn pane_session(&self, session_id: &str) -> Result<Option<PaneSession>, DbError> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT session_id, worktree_id, spec_id, token
                 FROM pane_sessions WHERE session_id = ?1",
                params![session_id],
                |r| {
                    Ok(PaneSession {
                        session_id: r.get(0)?,
                        worktree_id: r.get(1)?,
                        spec_id: r.get(2)?,
                        token: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Every pane in a worktree that has launched, so the UI can tell a pane it
    /// can resume from one it can only start.
    ///
    /// Returns `(session_id, spec_id)` pairs and deliberately **not** the token:
    /// the client has no use for it and every hop it does not travel is a hop
    /// that cannot drop it.
    pub fn resumable_panes(&self, worktree_id: i64) -> Result<Vec<(String, String)>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT session_id, spec_id FROM pane_sessions
             WHERE worktree_id = ?1 ORDER BY session_id",
        )?;
        let rows = stmt
            .query_map(params![worktree_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Keep a worktree's pane rows bounded.
    ///
    /// The FK cascade collects rows when a *worktree* goes, which is the case
    /// that matters for correctness — rowids are reused. It does not bound
    /// growth inside a living worktree, and nothing else did either: a tab id is
    /// a fresh UUID per pane and `close_on_exit` defaults to true, so every
    /// clean exit strands a row no future tab can present for the upsert to
    /// reclaim. One row per launch, forever, on a checkout worked in daily.
    ///
    /// So the newest [`MAX_PANE_SESSIONS_PER_WORKTREE`] survive. Evicting one
    /// costs a pane older than that many launches its resume — far beyond any
    /// layout anyone holds — and the failure is "start fresh", not silence.
    fn prune_pane_sessions(conn: &rusqlite::Connection, worktree_id: i64) -> Result<(), DbError> {
        conn.execute(
            "DELETE FROM pane_sessions
             WHERE worktree_id = ?1 AND session_id NOT IN (
                 SELECT session_id FROM pane_sessions
                 WHERE worktree_id = ?1
                 ORDER BY created_at DESC, session_id DESC
                 LIMIT ?2
             )",
            params![worktree_id, MAX_PANE_SESSIONS_PER_WORKTREE],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worktree row to hang pane sessions off, since the FK is what collects
    /// them.
    fn worktree(db: &Db) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO repos (root, name, created_at) VALUES ('/tmp/repo', 'repo', '2026-01-01T00:00:00.000000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (repo_root, path, branch, alias, is_main, created_at)
             VALUES ('/tmp/repo', '/tmp/repo/wt', 'main', 'wt', 1, '2026-01-01T00:00:00.000000Z')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
        (db, dir)
    }

    #[test]
    fn a_pane_with_no_row_has_nothing_to_resume() {
        let (db, _dir) = db();
        assert_eq!(db.pane_session("nope").unwrap(), None);
    }

    #[test]
    fn starting_a_pane_mints_a_token_and_relaunching_replaces_it() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        let first = db
            .record_pane_session("tab-1", wt, "claude", &mint_pane_token())
            .unwrap();
        assert!(!first.token.is_empty());
        assert_eq!(db.pane_session("tab-1").unwrap().unwrap(), first);

        // A fresh launch must never reuse the previous token: `--session-id` is a
        // create, so the second run would collide with the first conversation.
        let second = db
            .record_pane_session("tab-1", wt, "claude", &mint_pane_token())
            .unwrap();
        assert_ne!(second.token, first.token);
        assert_eq!(
            db.pane_session("tab-1").unwrap().unwrap().token,
            second.token
        );
    }

    /// **The property the whole feature rests on.**
    ///
    /// The table is keyed by `session_id` — one row per pane *tab instance* —
    /// not by `(worktree_id, spec_id)`, which is the normalisation someone will
    /// eventually propose because it looks tidier. That would collapse two
    /// Claude panes in one worktree onto a single token, so the second pane
    /// would reopen the first one's conversation: exactly the failure the token
    /// exists to prevent, and the one plain `--continue` already gets wrong.
    /// Nothing but this test stands between that change and shipping.
    #[test]
    fn two_panes_of_one_spec_in_one_worktree_keep_independent_tokens() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        let first = db
            .record_pane_session("tab-a", wt, "claude", &mint_pane_token())
            .unwrap();
        let second = db
            .record_pane_session("tab-b", wt, "claude", &mint_pane_token())
            .unwrap();
        assert_ne!(first.token, second.token);
        // …and neither displaced the other.
        assert_eq!(
            db.pane_session("tab-a").unwrap().unwrap().token,
            first.token
        );
        assert_eq!(
            db.pane_session("tab-b").unwrap().unwrap().token,
            second.token
        );
        assert_eq!(db.resumable_panes(wt).unwrap().len(), 2);
    }

    /// The cascade collects rows when a *worktree* goes, but a living worktree
    /// accretes one stranded row per launch: tab ids are fresh UUIDs and
    /// `close_on_exit` defaults to true, so no future tab can ever present the
    /// id again for the upsert to reclaim.
    #[test]
    fn a_worktrees_pane_rows_stay_bounded() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        for i in 0..(MAX_PANE_SESSIONS_PER_WORKTREE + 25) {
            db.record_pane_session(&format!("tab-{i:04}"), wt, "claude", &mint_pane_token())
                .unwrap();
        }
        assert_eq!(
            db.resumable_panes(wt).unwrap().len() as i64,
            MAX_PANE_SESSIONS_PER_WORKTREE
        );
        // The newest survive: the oldest ids are the ones that went.
        assert!(db.pane_session("tab-0000").unwrap().is_none());
        assert!(
            db.pane_session(&format!("tab-{:04}", MAX_PANE_SESSIONS_PER_WORKTREE + 24))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn resumable_panes_lists_the_worktrees_launched_panes_without_their_tokens() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        db.record_pane_session("tab-2", wt, "codex", &mint_pane_token())
            .unwrap();
        db.record_pane_session("tab-1", wt, "claude", &mint_pane_token())
            .unwrap();
        assert_eq!(
            db.resumable_panes(wt).unwrap(),
            vec![
                ("tab-1".to_owned(), "claude".to_owned()),
                ("tab-2".to_owned(), "codex".to_owned()),
            ]
        );
        assert!(db.resumable_panes(wt + 1).unwrap().is_empty());
    }

    /// Worktree ids are rowids and SQLite reuses them, so a pane row that
    /// outlived its worktree would eventually be read by an unrelated checkout —
    /// and hand a stranger's conversation to whoever opened that pane. The FK is
    /// what makes that impossible; a comment would not.
    #[test]
    fn deleting_a_worktree_takes_its_pane_sessions_with_it() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        db.record_pane_session("tab-1", wt, "claude", &mint_pane_token())
            .unwrap();
        db.lock()
            .execute("DELETE FROM worktrees WHERE id = ?1", params![wt])
            .unwrap();
        assert_eq!(
            db.pane_session("tab-1").unwrap(),
            None,
            "the cascade must collect pane rows, or a reused rowid inherits them"
        );
    }
}
