//! Durable remote ownership and terminal creation receipts.
//!
//! Receipts outlive holders and worktrees: forgetting one would turn a reconnect
//! after a reboot into permission to run the command again.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{Db, DbError, now_str};

const LOCAL_OWNER: &str = "local";
const OWNER_PREFIX: &str = "remote.session-owner:";
const TERMINAL_PREFIX: &str = "remote.terminal:";
const IDE_CLIENT_PREFIX: &str = "remote.ide-client:";

/// Ownership and retention fixed when a remote terminal is first requested.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTerminal {
    pub client_id: String,
    pub worktree_id: i64,
    /// Holder cleanup deadline during a daemon outage, fixed at creation.
    /// A remote browser disconnect never expires a session while the daemon runs.
    pub retention_secs: u64,
}

impl Db {
    /// Whether a reserved id belongs to this scope; `None` means it is unreserved.
    pub fn terminal_owner_matches(
        &self,
        session_id: &str,
        client_id: Option<&str>,
    ) -> Result<Option<bool>, DbError> {
        Ok(self
            .kv_get(&format!("{OWNER_PREFIX}{session_id}"))?
            .map(|owner| owner == client_id.unwrap_or(LOCAL_OWNER)))
    }

    /// Reserve ownership without authorizing a spawn, including prelaunch uploads.
    pub fn reserve_terminal_owner(
        &self,
        session_id: &str,
        client_id: Option<&str>,
    ) -> Result<bool, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let matches = reserve_owner(&tx, session_id, client_id.unwrap_or(LOCAL_OWNER))?;
        tx.commit()?;
        Ok(matches)
    }

    /// Read a creation receipt, including receipts for holders that no longer exist.
    pub fn remote_terminal(&self, session_id: &str) -> Result<Option<RemoteTerminal>, DbError> {
        self.kv_get(&format!("{TERMINAL_PREFIX}{session_id}"))?
            .map(|raw| serde_json::from_str(&raw).map_err(DbError::from))
            .transpose()
    }

    /// Enumerate durable terminal receipts for a client, including released holders.
    pub fn remote_terminal_ids(&self, client_id: &str) -> Result<Vec<String>, DbError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT key, value FROM kv WHERE key LIKE 'remote.terminal:%'")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut ids = Vec::new();
        for row in rows {
            let (key, raw) = row?;
            let scope: RemoteTerminal = serde_json::from_str(&raw)?;
            if scope.client_id == client_id {
                if let Some(id) = key.strip_prefix(TERMINAL_PREFIX) {
                    ids.push(id.to_owned());
                }
            }
        }
        Ok(ids)
    }

    /// Reserve a remote terminal once. Only the insertion grants permission to spawn.
    ///
    /// Existing receipts are immutable. The caller must check the returned owner
    /// and worktree before granting an attach, including after a concurrent mint.
    /// `None` refuses a reservation belonging to another scope.
    pub fn reserve_remote_terminal(
        &self,
        session_id: &str,
        requested: &RemoteTerminal,
    ) -> Result<Option<(RemoteTerminal, bool)>, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !reserve_owner(&tx, session_id, &requested.client_id)? {
            return Ok(None);
        }
        let key = format!("{TERMINAL_PREFIX}{session_id}");
        let inserted = tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO NOTHING",
            params![key, serde_json::to_string(requested)?, now_str()],
        )? != 0;
        let raw: String =
            tx.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))?;
        let stored = serde_json::from_str(&raw)?;
        tx.commit()?;
        Ok(Some((stored, inserted)))
    }

    /// Bind a daemon-minted IDE identity to an authenticated remote client.
    pub fn record_remote_ide_client(&self, id: &str, client_id: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO NOTHING",
            params![format!("{IDE_CLIENT_PREFIX}{id}"), client_id, now_str()],
        )?;
        Ok(())
    }

    /// Whether a resume credential belongs to this request's authentication scope.
    /// Local identities have no remote row, preserving local resume behavior.
    pub fn ide_client_matches(&self, id: &str, client_id: Option<&str>) -> Result<bool, DbError> {
        let conn = self.lock();
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM kv WHERE key = ?1",
                [format!("{IDE_CLIENT_PREFIX}{id}")],
                |r| r.get(0),
            )
            .optional()?;
        Ok(stored.as_deref() == client_id)
    }
}

fn reserve_owner(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
    client_id: &str,
) -> Result<bool, DbError> {
    let key = format!("{OWNER_PREFIX}{session_id}");
    tx.execute(
        "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO NOTHING",
        params![key, client_id, now_str()],
    )?;
    let stored: String = tx.query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| {
        row.get(0)
    })?;
    Ok(stored == client_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_receipts_survive_reopen_and_cannot_be_reassigned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.db");
        let original = RemoteTerminal {
            client_id: "client-a".into(),
            worktree_id: 42,
            retention_secs: 1800,
        };
        let db = Db::open_at(&path).unwrap();
        assert_eq!(
            db.reserve_remote_terminal("terminal", &original).unwrap(),
            Some((original.clone(), true))
        );
        drop(db);
        let db = Db::open_at(&path).unwrap();
        let changed = RemoteTerminal {
            client_id: "client-b".into(),
            ..original.clone()
        };
        assert_eq!(
            db.reserve_remote_terminal("terminal", &changed).unwrap(),
            None
        );
        assert_eq!(
            db.reserve_remote_terminal("terminal", &original).unwrap(),
            Some((original.clone(), false))
        );
        assert_eq!(db.remote_terminal("terminal").unwrap(), Some(original));
    }

    #[test]
    fn upload_reservation_does_not_spend_terminal_creation() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
        assert!(
            db.reserve_terminal_owner("terminal", Some("client-a"))
                .unwrap()
        );
        assert!(
            !db.reserve_terminal_owner("terminal", Some("client-b"))
                .unwrap()
        );
        assert_eq!(db.remote_terminal("terminal").unwrap(), None);
        let scope = RemoteTerminal {
            client_id: "client-a".into(),
            worktree_id: 1,
            retention_secs: 1800,
        };
        assert_eq!(
            db.reserve_remote_terminal("terminal", &scope).unwrap(),
            Some((scope, true))
        );
        assert!(db.reserve_terminal_owner("local", None).unwrap());
        assert!(
            !db.reserve_terminal_owner("local", Some("client-a"))
                .unwrap()
        );
        assert!(db.reserve_terminal_owner("local", None).unwrap());
    }

    #[test]
    fn concurrent_terminal_creation_has_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.db");
        let first = Db::open_at(&path).unwrap();
        let second = Db::open_at(&path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let scope = RemoteTerminal {
            client_id: "client-a".into(),
            worktree_id: 1,
            retention_secs: 1800,
        };
        let threads: Vec<_> = [first, second]
            .into_iter()
            .map(|db| {
                let barrier = barrier.clone();
                let scope = scope.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    db.reserve_remote_terminal("terminal", &scope)
                        .unwrap()
                        .unwrap()
                        .1
                })
            })
            .collect();
        let winners = threads
            .into_iter()
            .map(|thread| usize::from(thread.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
    }

    #[test]
    fn corrupt_terminal_policy_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("veld.db")).unwrap();
        db.kv_set("remote.terminal:terminal", "invalid-json")
            .unwrap();
        assert!(db.remote_terminal("terminal").is_err());
    }

    #[test]
    fn ide_resume_requires_the_persisted_remote_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.db");
        let db = Db::open_at(&path).unwrap();
        db.record_remote_ide_client("ide-a", "client-a").unwrap();
        drop(db);
        let db = Db::open_at(&path).unwrap();
        assert!(db.ide_client_matches("ide-a", Some("client-a")).unwrap());
        assert!(!db.ide_client_matches("ide-a", Some("client-b")).unwrap());
        assert!(!db.ide_client_matches("ide-a", None).unwrap());
        assert!(!db.ide_client_matches("local", Some("client-a")).unwrap());
        assert!(db.ide_client_matches("local", None).unwrap());
    }
}
