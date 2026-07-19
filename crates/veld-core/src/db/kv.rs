//! Small key/value state (hints, update/GC stamps) and the relay token cache.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};

use super::{Db, DbError, now_str, parse_ts};

impl Db {
    // -----------------------------------------------------------------------
    // Generic key/value
    // -----------------------------------------------------------------------

    pub fn kv_get(&self, key: &str) -> Result<Option<String>, DbError> {
        let conn = self.lock();
        let v = conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(v)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now_str()],
        )?;
        Ok(())
    }

    pub fn kv_delete(&self, key: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
        Ok(())
    }

    /// When the key was last written, if ever.
    pub fn kv_updated_at(
        &self,
        key: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, DbError> {
        let conn = self.lock();
        let v: Option<String> = conn
            .query_row("SELECT updated_at FROM kv WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(v.as_deref().and_then(parse_ts))
    }

    /// Atomically claim an interval-gated stamp: returns `true` (and bumps the
    /// stamp) only when the key is absent or older than `interval`. Used for
    /// "at most once per N minutes" work like auto-GC, race-free across
    /// concurrent CLI invocations.
    pub fn kv_try_claim_interval(
        &self,
        key: &str,
        interval: std::time::Duration,
    ) -> Result<bool, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let last: Option<String> = tx
            .query_row("SELECT updated_at FROM kv WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        let now = chrono::Utc::now();
        // A future-dated stamp (clock moved backward) counts as claimable —
        // otherwise auto-GC/update checks would stall until wall-clock
        // catches up.
        let fresh_enough = last.as_deref().and_then(parse_ts).is_some_and(|t| {
            (now - t)
                .to_std()
                .map(|age| age < interval)
                .unwrap_or(false)
        });
        if fresh_enough {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, '', ?2)
             ON CONFLICT(key) DO UPDATE SET updated_at = excluded.updated_at",
            params![key, super::ts_to_str(now)],
        )?;
        tx.commit()?;
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Relay token cache (relay URL → auth token; secrets)
    // -----------------------------------------------------------------------

    /// Load all cached relay tokens. Errors read as empty — the cache must
    /// never break joining.
    pub fn relay_tokens(&self) -> BTreeMap<String, String> {
        let conn = self.lock();
        let Ok(mut stmt) = conn.prepare_cached("SELECT relay_url, token FROM relay_tokens") else {
            return BTreeMap::new();
        };
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Persist `token` for `url`, merging into the existing cache.
    pub fn save_relay_token(&self, url: &str, token: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO relay_tokens (relay_url, token, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(relay_url) DO UPDATE SET token = excluded.token, updated_at = excluded.updated_at",
            params![url, token, now_str()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::test_db;

    #[test]
    fn kv_roundtrip() {
        let (_dir, db) = test_db();
        assert!(db.kv_get("a").unwrap().is_none());
        db.kv_set("a", "1").unwrap();
        assert_eq!(db.kv_get("a").unwrap().as_deref(), Some("1"));
        db.kv_set("a", "2").unwrap();
        assert_eq!(db.kv_get("a").unwrap().as_deref(), Some("2"));
        assert!(db.kv_updated_at("a").unwrap().is_some());
        db.kv_delete("a").unwrap();
        assert!(db.kv_get("a").unwrap().is_none());
    }

    #[test]
    fn claim_interval_gates() {
        let (_dir, db) = test_db();
        let hour = std::time::Duration::from_secs(3600);
        assert!(db.kv_try_claim_interval("gc", hour).unwrap());
        // Immediately after: still within the interval.
        assert!(!db.kv_try_claim_interval("gc", hour).unwrap());
        // Zero interval: always claimable.
        assert!(
            db.kv_try_claim_interval("gc", std::time::Duration::ZERO)
                .unwrap()
        );
    }

    #[test]
    fn relay_tokens_merge() {
        let (_dir, db) = test_db();
        assert!(db.relay_tokens().is_empty());
        db.save_relay_token("https://a.example/", "tok-a").unwrap();
        db.save_relay_token("https://b.example/", "tok-b").unwrap();
        db.save_relay_token("https://a.example/", "tok-a2").unwrap();
        let map = db.relay_tokens();
        assert_eq!(map["https://a.example/"], "tok-a2");
        assert_eq!(map["https://b.example/"], "tok-b");
    }
}
