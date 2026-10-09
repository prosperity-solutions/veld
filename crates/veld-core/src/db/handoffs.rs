//! Worktrees an agent handed off to a human, and the agent pane each one is
//! waiting to start.
//!
//! `veld worktree new --prompt` is how a coding agent branches a task off into a
//! checkout of its own: the daemon creates the worktree, files it into the
//! rail's "Waiting for you" section ([`HANDOFF_LANE`]), and records a [`Handoff`]. The
//! first window to show that worktree starts the agent pane under the session id
//! recorded here with the prompt as its first message, and from then on the
//! human drives it. Nothing reports back to the agent that handed it off.
//!
//! See `migrate_v18_worktree_handoffs` for why the row outlives a window reading
//! it and why it cascades with its worktree.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, DbError, now_str};

/// The `worktrees.lane` value of the rail's "Waiting for you" section.
///
/// Not a lane: there is no row for it in `lanes`, so it cannot be renamed,
/// deleted, reordered or dropped into — `patch_worktree` accepts only lanes that
/// exist, which is what keeps users from filing anything here by hand. What makes
/// it a lane *value* rather than a flag is how a row leaves: every way of moving a
/// worktree writes `lane`, and every lane a user can write differs from this one,
/// so dragging a row out, "Move to lane", and a batch move all take it out of the
/// section without having to know the section exists.
///
/// NUL-prefixed like [`super::UNGROUPED_LANE`], so `valid_lane_name` (which
/// rejects control characters) can never mint a user lane that collides with it.
/// The UI spells the same constant `HANDOFF_LANE` in `model.ts`; the two are one
/// stored value and must stay in step.
pub const HANDOFF_LANE: &str = "\u{0}handoff";

/// An agent pane waiting to be started in a handed-off worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    pub worktree_id: i64,
    /// The terminal session the pane will run under. Chosen by the daemon, so the
    /// window that starts it and the daemon that retires it agree on which
    /// session finishes the hand-off without either trusting the other's memory.
    pub session_id: String,
    /// The declared `ide.panes[].id` to run, or `""` for the agent the user
    /// usually picks in this project — a preference only a client holds.
    pub pane: String,
    /// The first message the agent opens with.
    pub prompt: String,
    pub created_at: String,
}

impl Db {
    /// Record a worktree's pending agent pane, replacing any earlier one.
    ///
    /// **The worktree must exist**: with `foreign_keys=ON` an insert for an unknown
    /// id fails rather than leaving a hand-off for the next checkout to inherit.
    pub fn put_handoff(
        &self,
        worktree_id: i64,
        session_id: &str,
        pane: &str,
        prompt: &str,
    ) -> Result<(), DbError> {
        self.lock().execute(
            "INSERT INTO worktree_handoffs (worktree_id, session_id, pane, prompt, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(worktree_id) DO UPDATE SET
                 session_id = excluded.session_id,
                 pane = excluded.pane,
                 prompt = excluded.prompt,
                 created_at = excluded.created_at",
            params![worktree_id, session_id, pane, prompt, now_str()],
        )?;
        Ok(())
    }

    /// A worktree's pending hand-off, prompt included.
    pub fn handoff(&self, worktree_id: i64) -> Result<Option<Handoff>, DbError> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT worktree_id, session_id, pane, prompt, created_at
                 FROM worktree_handoffs WHERE worktree_id = ?1",
                params![worktree_id],
                |r| {
                    Ok(Handoff {
                        worktree_id: r.get(0)?,
                        session_id: r.get(1)?,
                        pane: r.get(2)?,
                        prompt: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// The session id and pane of a worktree's pending hand-off, without the
    /// prompt — what every rail poll carries, where a prompt of up to 64 KiB per
    /// worktree would be paid for on every refresh by a client that only needs
    /// to know there is one.
    pub fn handoff_session(&self, worktree_id: i64) -> Result<Option<(String, String)>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT session_id, pane FROM worktree_handoffs WHERE worktree_id = ?1",
        )?;
        Ok(stmt
            .query_row(params![worktree_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?)
    }

    /// Drop a worktree's pending hand-off, whatever session it names — for a
    /// window that cannot start it (the checkout declares no agent pane), so the
    /// refusal is reported once rather than on every show.
    pub fn drop_handoff(&self, worktree_id: i64) -> Result<bool, DbError> {
        let n = self.lock().execute(
            "DELETE FROM worktree_handoffs WHERE worktree_id = ?1",
            params![worktree_id],
        )?;
        Ok(n > 0)
    }

    /// Retire a hand-off because its session has been spawned. Returns whether
    /// there was one for exactly that worktree and session.
    ///
    /// Matched on both, so a session that merely *runs in* a handed-off worktree —
    /// a shell the user opened beside it — cannot retire the agent pane's launch.
    pub fn finish_handoff(&self, worktree_id: i64, session_id: &str) -> Result<bool, DbError> {
        let n = self.lock().execute(
            "DELETE FROM worktree_handoffs WHERE worktree_id = ?1 AND session_id = ?2",
            params![worktree_id, session_id],
        )?;
        Ok(n > 0)
    }

    /// File a worktree into the rail's "Waiting for you" section.
    ///
    /// Written directly rather than through `patch_worktree`, which accepts only
    /// lanes that exist in `lanes` — the property that keeps a user from filing a
    /// row here, and so the one this, the daemon's own hand-off path, must
    /// bypass. Clears `sort_position` the way any lane change does.
    pub fn file_into_handoffs(&self, worktree_id: i64) -> Result<bool, DbError> {
        let n = self.lock().execute(
            "UPDATE worktrees SET lane = ?2, sort_position = NULL WHERE id = ?1",
            params![worktree_id, HANDOFF_LANE],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{WorktreePatch, test_db};

    fn seed_worktree(db: &Db, root: &str, path: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT OR IGNORE INTO repos (root, name, created_at)
             VALUES (?1, 'repo', '2026-01-01T00:00:00.000000Z')",
            params![root],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (repo_root, path, branch, alias, is_main, created_at)
             VALUES (?1, ?2, 'feat', ?2, 0, '2026-01-01T00:00:00.000000Z')",
            params![root, path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn a_handoff_is_retired_only_by_the_session_it_names() {
        let (_dir, db) = test_db();
        let wt = seed_worktree(&db, "/r", "/r/_wt/a");
        db.put_handoff(wt, "sess-1", "claude", "fix the thing")
            .unwrap();
        assert_eq!(
            db.handoff_session(wt).unwrap(),
            Some(("sess-1".to_owned(), "claude".to_owned()))
        );
        assert!(!db.finish_handoff(wt, "a-shell-beside-it").unwrap());
        assert!(db.handoff(wt).unwrap().is_some());
        assert!(db.finish_handoff(wt, "sess-1").unwrap());
        assert_eq!(db.handoff(wt).unwrap(), None);
        assert!(!db.finish_handoff(wt, "sess-1").unwrap());
    }

    /// Declared twice, here and as `HANDOFF_LANE` in the UI's `model.ts`, with
    /// nothing generating one from the other; a drift is silent (the rows fall into
    /// the ungrouped section). `model.test.ts` holds the twin.
    #[test]
    fn the_handoff_lane_is_the_bytes_the_ui_reads() {
        assert_eq!(HANDOFF_LANE.as_bytes(), b"\0handoff");
    }

    /// Neither lane operation may reach the section: rename would move its rows
    /// into a real lane, delete would empty it into the ungrouped section.
    #[test]
    fn the_handoff_section_cannot_be_renamed_or_deleted() {
        let (_dir, db) = test_db();
        let wt = seed_worktree(&db, "/r", "/r/_wt/a");
        db.file_into_handoffs(wt).unwrap();
        let root = std::path::Path::new("/r");
        assert!(!db.delete_lane(root, HANDOFF_LANE).unwrap());
        assert!(!db.rename_lane(root, HANDOFF_LANE, "mine").unwrap());
        assert_eq!(db.get_worktree(wt).unwrap().unwrap().lane, HANDOFF_LANE);
    }

    #[test]
    fn a_second_handoff_replaces_the_first() {
        let (_dir, db) = test_db();
        let wt = seed_worktree(&db, "/r", "/r/_wt/a");
        db.put_handoff(wt, "sess-1", "", "one").unwrap();
        db.put_handoff(wt, "sess-2", "codex", "two").unwrap();
        let h = db.handoff(wt).unwrap().unwrap();
        assert_eq!(
            (h.session_id.as_str(), h.pane.as_str(), h.prompt.as_str()),
            ("sess-2", "codex", "two")
        );
    }

    /// The rowid-reuse hazard the cascade exists for: a hand-off must not survive
    /// its checkout and start an agent in whichever worktree takes the id next.
    #[test]
    fn a_handoff_goes_with_its_worktree() {
        let (_dir, db) = test_db();
        let wt = seed_worktree(&db, "/r", "/r/_wt/a");
        db.put_handoff(wt, "sess-1", "", "prompt").unwrap();
        db.lock()
            .execute("DELETE FROM worktrees WHERE id = ?1", params![wt])
            .unwrap();
        let next = seed_worktree(&db, "/r", "/r/_wt/b");
        assert_eq!(next, wt, "the test relies on SQLite reusing the rowid");
        assert_eq!(db.handoff(next).unwrap(), None);
    }

    #[test]
    fn a_handoff_cannot_name_a_worktree_that_does_not_exist() {
        let (_dir, db) = test_db();
        assert!(db.put_handoff(4242, "sess-1", "", "prompt").is_err());
    }

    /// The section is left by writing any lane a user can write — including `''`,
    /// which is what makes "drag it into the ungrouped section" a real move rather
    /// than a no-op the client would skip.
    #[test]
    fn the_handoff_section_is_left_by_any_lane_write_and_never_entered_by_one() {
        let (_dir, db) = test_db();
        let wt = seed_worktree(&db, "/r", "/r/_wt/a");
        assert!(db.file_into_handoffs(wt).unwrap());
        assert_eq!(db.get_worktree(wt).unwrap().unwrap().lane, HANDOFF_LANE);

        let refused = db.patch_worktree(
            wt,
            WorktreePatch {
                lane: Some(HANDOFF_LANE),
                ..Default::default()
            },
        );
        assert!(
            refused.is_err(),
            "a user write must not file into the section"
        );

        db.patch_worktree(
            wt,
            WorktreePatch {
                lane: Some(""),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(db.get_worktree(wt).unwrap().unwrap().lane, "");
    }
}
