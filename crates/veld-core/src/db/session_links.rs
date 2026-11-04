//! Links back to a terminal: what `veld agent link` hands an agent to end a pull
//! request with, and what the daemon's `/session/<id>` page resolves.
//!
//! See `migrate_v23_session_links` for why the id is minted rather than derived,
//! why a project agent pane's link follows its conversation rather than its tab,
//! and why a row cascades with its worktree.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, DbError, now_str};

/// One terminal's link: the opaque `id` in the URL, and where it leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLink {
    pub id: String,
    /// The PTY session id the link was made in, which is also the IDE's tab id.
    pub session_id: String,
    pub worktree_id: i64,
    /// The conversation the tab was running, for one of the project's own panes:
    /// its token and pane spec. `None` for a plain terminal.
    pub conversation: Option<Conversation>,
}

/// A project pane's conversation, as `pane_sessions` recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// The pane's token — the agent's own session id. Never sent to a client
    /// except as the session a resume adopts, which is the picker's shape.
    pub token: String,
    pub spec_id: String,
}

/// Whether `id` has the shape [`Db::session_link_for`] mints: 32 lowercase hex.
///
/// Checked before a lookup so a URL segment that could never be a link answers
/// "not found" without reaching SQLite, and so nothing else ends up in a log.
#[must_use]
pub fn valid_link_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

impl Db {
    /// The link for terminal `session_id` in `worktree_id`, minted on first ask.
    ///
    /// The same terminal gets the same link while it runs the same conversation,
    /// so every pull request opened from one tab points at one place. The
    /// conversation is read from `pane_sessions` now, and re-read on every ask:
    /// a tab restarted from scratch since the last link is running a new one, and
    /// the next pull request is about that.
    pub fn session_link_for(
        &self,
        session_id: &str,
        worktree_id: i64,
    ) -> Result<SessionLink, DbError> {
        let conn = self.lock();
        let conversation: Option<(String, String)> = conn
            .query_row(
                "SELECT token, spec_id FROM pane_sessions
                 WHERE session_id = ?1 AND worktree_id = ?2",
                params![session_id, worktree_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        // An empty token is no conversation: the unique index reads a missing
        // token as `''`, and keeping one would collide with the plain-terminal row.
        let (token, spec_id) = conversation.filter(|(t, _)| !t.is_empty()).unzip();
        // Insert-or-keep, then read back: the unique index on (tab, conversation)
        // decides which, so two agents asking at once still share one link. A new
        // conversation in the same tab is a new row, and the old link keeps leading
        // to the conversation the earlier pull request came from.
        conn.execute(
            "INSERT OR IGNORE INTO session_links
                 (id, session_id, worktree_id, token, spec_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                uuid::Uuid::new_v4().simple().to_string(),
                session_id,
                worktree_id,
                token,
                spec_id,
                now_str()
            ],
        )?;
        let id: String = conn.query_row(
            "SELECT id FROM session_links WHERE session_id = ?1 AND token IS ?2",
            params![session_id, token],
            |r| r.get(0),
        )?;
        Ok(SessionLink {
            id,
            session_id: session_id.to_owned(),
            worktree_id,
            conversation: token
                .zip(spec_id)
                .map(|(token, spec_id)| Conversation { token, spec_id }),
        })
    }

    /// Where link `id` leads, if this machine minted it and its worktree still exists.
    pub fn session_link(&self, id: &str) -> Result<Option<SessionLink>, DbError> {
        if !valid_link_id(id) {
            return Ok(None);
        }
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, session_id, worktree_id, token, spec_id
                 FROM session_links WHERE id = ?1",
                params![id],
                |r| {
                    let token: Option<String> = r.get(3)?;
                    let spec_id: Option<String> = r.get(4)?;
                    Ok(SessionLink {
                        id: r.get(0)?,
                        session_id: r.get(1)?,
                        worktree_id: r.get(2)?,
                        conversation: token
                            .zip(spec_id)
                            .map(|(token, spec_id)| Conversation { token, spec_id }),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// The terminals in `worktree_id` whose recorded conversation is `token` —
    /// where a conversation is now, after a close and a resume moved it to a new tab.
    pub fn sessions_with_token(
        &self,
        worktree_id: i64,
        token: &str,
    ) -> Result<Vec<String>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT session_id FROM pane_sessions
             WHERE worktree_id = ?1 AND token = ?2 ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map(params![worktree_id, token], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_worktree() -> (tempfile::TempDir, Db, i64) {
        let (dir, db) = crate::db::test_db();
        let id = {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO repos (root, name, created_at)
                 VALUES ('/r', 'repo', '2026-01-01T00:00:00.000000Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO worktrees (repo_root, path, branch, alias, is_main, created_at)
                 VALUES ('/r', '/r/_wt/a', 'feat', 'a', 0, '2026-01-01T00:00:00.000000Z')",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        (dir, db, id)
    }

    #[test]
    fn one_terminal_keeps_one_link() {
        let (_dir, db, wt) = db_with_worktree();
        let first = db.session_link_for("tab-a", wt).unwrap();
        assert!(valid_link_id(&first.id), "{}", first.id);
        assert_eq!(first.conversation, None, "a plain terminal has none");
        let again = db.session_link_for("tab-a", wt).unwrap();
        assert_eq!(first, again);
        let other = db.session_link_for("tab-b", wt).unwrap();
        assert_ne!(first.id, other.id);
        assert_eq!(db.session_link(&first.id).unwrap(), Some(first));
    }

    #[test]
    fn an_agent_pane_link_follows_its_conversation() {
        let (_dir, db, wt) = db_with_worktree();
        db.record_pane_session("tab-a", wt, "claude", "tok-1")
            .unwrap();
        let first = db.session_link_for("tab-a", wt).unwrap();
        assert_eq!(
            first.conversation,
            Some(Conversation {
                token: "tok-1".into(),
                spec_id: "claude".into()
            })
        );
        assert_eq!(db.session_link_for("tab-a", wt).unwrap().id, first.id);

        // Restarted from scratch: same tab, new conversation, new link — and the
        // old link still names the conversation it was made for.
        db.record_pane_session("tab-a", wt, "claude", "tok-2")
            .unwrap();
        let second = db.session_link_for("tab-a", wt).unwrap();
        assert_ne!(second.id, first.id);
        let old = db.session_link(&first.id).unwrap().unwrap();
        assert_eq!(old.session_id, "tab-a");
        assert_eq!(old.conversation.unwrap().token, "tok-1");

        // Picked up again in another tab: found by its token.
        db.record_pane_session("tab-b", wt, "claude", "tok-1")
            .unwrap();
        assert_eq!(db.sessions_with_token(wt, "tok-1").unwrap(), vec!["tab-b"]);
    }

    #[test]
    fn a_link_names_nothing_it_did_not_mint() {
        let (_dir, db, _wt) = db_with_worktree();
        assert_eq!(db.session_link(&"0".repeat(32)).unwrap(), None);
        assert_eq!(db.session_link("../etc").unwrap(), None);
        assert_eq!(db.session_link(&"A".repeat(32)).unwrap(), None);
    }

    #[test]
    fn a_deleted_worktree_takes_its_links_with_it() {
        let (_dir, db, wt) = db_with_worktree();
        let link = db.session_link_for("tab-a", wt).unwrap();
        db.lock()
            .execute("DELETE FROM worktrees WHERE id = ?1", params![wt])
            .unwrap();
        assert_eq!(db.session_link(&link.id).unwrap(), None);
    }
}
