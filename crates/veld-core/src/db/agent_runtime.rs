//! Immutable host-approved agent selections and one-shot launch claims.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Db, DbError, now_str};
use crate::agent_runtime::SessionSelection;

const LAUNCH_PREFIX: &str = "remote.agent-launch:";
const CLAIM_PREFIX: &str = "remote.agent-claim:";

/// Host-derived checkout binding and authenticated owner, stored before PTY creation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLaunchRecord {
    pub client_id: String,
    pub worktree_id: i64,
    pub directory: String,
    pub session: SessionSelection,
}

/// Domain-separated SHA-256 for nonsecret runtime identities and revisions.
pub fn runtime_digest(domain: &str, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    digest.update([0]);
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

impl Db {
    /// Load the full approved tuple. An absent legacy record grants no launch authority.
    pub fn agent_launch(&self, session_id: &str) -> Result<Option<AgentLaunchRecord>, DbError> {
        self.kv_get(&format!("{LAUNCH_PREFIX}{session_id}"))?
            .map(|raw| serde_json::from_str(&raw).map_err(DbError::from))
            .transpose()
    }

    /// Insert once, returning the existing record unchanged after concurrent requests.
    /// `None` means a legacy terminal already occupies the ID without an agent receipt.
    pub fn reserve_agent_launch(
        &self,
        session_id: &str,
        requested: &AgentLaunchRecord,
    ) -> Result<Option<AgentLaunchRecord>, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let key = format!("{LAUNCH_PREFIX}{session_id}");
        let owner_key = format!("remote.session-owner:{session_id}");
        let owner: Option<String> = tx
            .query_row("SELECT value FROM kv WHERE key = ?1", [&owner_key], |row| {
                row.get(0)
            })
            .optional()?;
        if owner
            .as_ref()
            .is_some_and(|owner| owner != &requested.client_id)
        {
            return Ok(None);
        }
        let legacy: bool = tx.query_row(
            "SELECT (EXISTS(SELECT 1 FROM kv WHERE key = ?1)
             OR EXISTS(SELECT 1 FROM pane_sessions WHERE session_id = ?3))
             AND NOT EXISTS(SELECT 1 FROM kv WHERE key = ?2)",
            params![format!("remote.terminal:{session_id}"), key, session_id],
            |row| row.get(0),
        )?;
        if legacy {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO NOTHING",
            params![owner_key, requested.client_id, now_str()],
        )?;
        tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO NOTHING",
            params![key, serde_json::to_string(requested)?, now_str()],
        )?;
        let raw: String =
            tx.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))?;
        let stored = serde_json::from_str(&raw)?;
        tx.commit()?;
        Ok(Some(stored))
    }

    /// Consume launch authority exactly once, only if the approved record still matches.
    /// A crash or failed exec consumes the claim; recovery requires a new session ID.
    pub fn claim_agent_launch(
        &self,
        session_id: &str,
        expected: &AgentLaunchRecord,
    ) -> Result<bool, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key = ?1",
                [format!("{LAUNCH_PREFIX}{session_id}")],
                |r| r.get(0),
            )
            .optional()?;
        let stored: Option<AgentLaunchRecord> =
            raw.map(|raw| serde_json::from_str(&raw)).transpose()?;
        if stored.as_ref() != Some(expected) {
            return Ok(false);
        }
        let claimed = tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, 'claimed', ?2)
             ON CONFLICT(key) DO NOTHING",
            params![format!("{CLAIM_PREFIX}{session_id}"), now_str()],
        )? != 0;
        tx.commit()?;
        Ok(claimed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::LaunchSelection;
    use std::collections::BTreeMap;

    fn record() -> AgentLaunchRecord {
        AgentLaunchRecord {
            client_id: "client-a".into(),
            worktree_id: 1,
            directory: "/checkout".into(),
            session: SessionSelection {
                instance_id: "instance-a".into(),
                checkout_id: "checkout-a".into(),
                provider_session_id: None,
                selection: LaunchSelection {
                    runtime_id: "codex".into(),
                    account_id: "account-a".into(),
                    provider_connection_id: "inherited".into(),
                    model_id: "fixture-model".into(),
                    options: BTreeMap::new(),
                    catalogue_revision: "revision-a".into(),
                },
            },
        }
    }

    #[test]
    fn receipts_survive_reopen_and_cannot_change_owner_or_options() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("runtime.db");
        let original = record();
        let db = Db::open_at(&path).unwrap();
        assert_eq!(
            db.reserve_agent_launch("session", &original).unwrap(),
            Some(original.clone())
        );
        drop(db);
        let db = Db::open_at(&path).unwrap();
        let mut changed = original.clone();
        changed.client_id = "client-b".into();
        changed
            .session
            .selection
            .options
            .insert("reasoningEffort".into(), "other".into());
        assert_eq!(db.reserve_agent_launch("session", &changed).unwrap(), None);
        assert!(!db.claim_agent_launch("session", &changed).unwrap());
        assert!(db.claim_agent_launch("session", &original).unwrap());
        assert!(!db.claim_agent_launch("session", &original).unwrap());
        assert!(!db.claim_agent_launch("legacy", &original).unwrap());
        assert_eq!(db.agent_launch("session").unwrap(), Some(original));
    }

    #[test]
    fn legacy_terminal_cannot_acquire_agent_launch_authority() {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::open_at(&temp.path().join("runtime.db")).unwrap();
        db.kv_set("remote.terminal:legacy", "{}").unwrap();
        assert_eq!(db.reserve_agent_launch("legacy", &record()).unwrap(), None);
        assert_eq!(db.agent_launch("legacy").unwrap(), None);
    }

    #[test]
    fn hashes_separate_identity_domains() {
        assert_eq!(
            runtime_digest("account", b"value"),
            runtime_digest("account", b"value")
        );
        assert_ne!(
            runtime_digest("account", b"value"),
            runtime_digest("checkout", b"value")
        );
    }

    #[test]
    fn staging_cannot_claim_local_or_other_client_terminal_ids() {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::open_at(&temp.path().join("runtime.db")).unwrap();
        assert!(db.reserve_terminal_owner("local-terminal", None).unwrap());
        assert_eq!(
            db.reserve_agent_launch("local-terminal", &record())
                .unwrap(),
            None
        );
        assert!(
            db.terminal_owner_matches("local-terminal", None)
                .unwrap()
                .unwrap()
        );
        assert!(db.agent_launch("local-terminal").unwrap().is_none());
        assert!(
            db.reserve_terminal_owner("other-terminal", Some("other-client"))
                .unwrap()
        );
        assert_eq!(
            db.reserve_agent_launch("other-terminal", &record())
                .unwrap(),
            None
        );
        let approved = record();
        assert!(
            db.reserve_agent_launch("new-agent", &approved)
                .unwrap()
                .is_some()
        );
        assert!(!db.reserve_terminal_owner("new-agent", None).unwrap());
        assert!(
            db.reserve_terminal_owner("new-agent", Some(&approved.client_id))
                .unwrap()
        );
    }
}
