//! Small key/value state (hints, update/GC stamps) and the relay token cache.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};

use super::{Db, DbError, now_str, parse_ts};

/// What a user has done about one promotion.
///
/// There is no `Unread` variant: unread is the *absence* of a row, which keeps
/// the stored map proportional to what the user has acted on rather than to
/// everything Veld has ever shipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromotionState {
    /// "Stop putting this in front of me." Never prompted again — but still
    /// counted as unread, because dismissing is not reading. That distinction is
    /// the whole reason this is an enum and not a set of ids.
    Dismissed,
    /// Actually read. Clears the unread indicator.
    Read,
}

/// Where the per-promotion state map lives. A `kv` row rather than a `settings`
/// key: settings are *preferences*, with a dialog over them and a localStorage
/// mirror in every client, and a stale mirror written back would resurrect
/// dismissed cards. This is bookkeeping — the same shape as the update/GC stamps
/// beside it.
const PROMOTIONS_STATE_KEY: &str = "promotions.state";

/// When this user first opened the IDE. Written once; see
/// [`Db::promotions_first_use`].
const PROMOTIONS_FIRST_USE_KEY: &str = "promotions.firstUse";

/// Parse the stored state map. **An unparseable value reads as empty**, and that
/// direction is chosen deliberately: the alternative — treating garbage as
/// "everything read" — would silently switch the feature off for a user with no
/// symptom to report. Showing a card again is the recoverable failure.
///
/// An individual entry whose value is not a state Veld knows is dropped rather
/// than failing the whole map, so a *newer* client's future state name costs one
/// re-shown card instead of resetting everything.
fn decode_states(raw: &str) -> BTreeMap<String, PromotionState> {
    serde_json::from_str::<BTreeMap<String, serde_json::Value>>(raw)
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| serde_json::from_value(v).ok().map(|s| (k, s)))
                .collect()
        })
        .unwrap_or_default()
}

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
    // Feature promotions — what this user has seen, and when they arrived
    // -----------------------------------------------------------------------

    /// Every promotion this user has acted on, by id.
    ///
    /// An id that is absent is **unread**, which is the majority state and is
    /// therefore the one that costs nothing to store.
    pub fn promotion_states(&self) -> Result<BTreeMap<String, PromotionState>, DbError> {
        Ok(self
            .kv_get(PROMOTIONS_STATE_KEY)?
            .as_deref()
            .map(decode_states)
            .unwrap_or_default())
    }

    /// When this user first opened the IDE, recording it if this is that moment.
    ///
    /// **The date gate hangs off this, so it is written exactly once and never
    /// overwritten.** A dated promotion older than this timestamp is *auto-read*:
    /// it is visible in the history but is never counted as unread and never
    /// prompts, which is how somebody installing Veld today avoids a modal about
    /// a change that shipped last spring.
    ///
    /// Deliberately **not** derived from anything that looks like database age.
    /// The tempting version of this — "a database with no rows is a new user" —
    /// is wrong here in a way that bites daily: `veld start --preset dev` mints
    /// `.veld-dev/<run>/veld.db` several times a day, and either the CLI or the
    /// daemon may be the process that creates one. This is a stamp, written the
    /// first time a client asks, and a stamp stays true afterwards.
    ///
    /// A caller that only wants to *read* it without establishing it would be
    /// asking a question with no answer — "when did they arrive, if they have
    /// not?" — so there is one function and it does both.
    pub fn promotions_first_use(&self) -> Result<chrono::DateTime<chrono::Utc>, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let stored: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key = ?1",
                [PROMOTIONS_FIRST_USE_KEY],
                |r| r.get(0),
            )
            .optional()?;
        // A stored value that will not parse is repaired rather than trusted:
        // leaving it would make every dated promotion's gate undecidable, and
        // the repair costs at worst one back-catalogue suppression.
        let first_use = match stored.as_deref().and_then(parse_ts) {
            Some(t) => t,
            None => {
                let now = chrono::Utc::now();
                tx.execute(
                    "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                    params![PROMOTIONS_FIRST_USE_KEY, super::ts_to_str(now)],
                )?;
                now
            }
        };
        tx.commit()?;
        Ok(first_use)
    }

    /// Move `ids` to `state`, returning the full map.
    ///
    /// **`read` wins over `dismissed`, and neither is ever undone.** The two are
    /// not a toggle: dismissing says "stop putting this in front of me", reading
    /// says "I have taken this in", and a client that dismisses a card it
    /// already showed must not be able to walk back a read. Making the merge
    /// monotone is also what lets two windows act on the same card at the same
    /// moment with no compare-and-swap — the writes converge in whatever order
    /// they land, the same property the id set had.
    ///
    /// Applied inside one immediate transaction, so the read-modify-write cannot
    /// drop a concurrent client's change.
    pub fn mark_promotions(
        &self,
        ids: &[String],
        state: PromotionState,
    ) -> Result<BTreeMap<String, PromotionState>, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let stored: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key = ?1",
                [PROMOTIONS_STATE_KEY],
                |r| r.get(0),
            )
            .optional()?;
        let mut states = stored.as_deref().map(decode_states).unwrap_or_default();
        for id in ids {
            let entry = states.entry(id.clone()).or_insert(state);
            if state == PromotionState::Read {
                *entry = PromotionState::Read;
            }
        }
        let encoded = serde_json::to_string(&states).expect("a map of plain strings serialises");
        tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![PROMOTIONS_STATE_KEY, encoded, now_str()],
        )?;
        tx.commit()?;
        Ok(states)
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

    use crate::db::PromotionState;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_untouched_promotion_has_no_stored_state() {
        let (_dir, db) = test_db();
        assert!(db.promotion_states().unwrap().is_empty());
        // Unread is the absence of a row, so the map stays proportional to what
        // the user acted on rather than to everything Veld ever shipped.
        db.mark_promotions(&ids(&["a"]), PromotionState::Dismissed)
            .unwrap();
        assert_eq!(db.promotion_states().unwrap().len(), 1);
    }

    #[test]
    fn read_wins_over_dismissed_and_neither_is_ever_undone() {
        let (_dir, db) = test_db();
        db.mark_promotions(&ids(&["a"]), PromotionState::Dismissed)
            .unwrap();
        let states = db
            .mark_promotions(&ids(&["a"]), PromotionState::Read)
            .unwrap();
        assert_eq!(states["a"], PromotionState::Read);
        // The other direction must not walk the read back: a second window
        // dismissing a card this one already read cannot un-read it.
        let states = db
            .mark_promotions(&ids(&["a"]), PromotionState::Dismissed)
            .unwrap();
        assert_eq!(states["a"], PromotionState::Read);
    }

    #[test]
    fn marking_is_a_merge_so_two_clients_cannot_undo_each_other() {
        let (_dir, db) = test_db();
        db.mark_promotions(&ids(&["a"]), PromotionState::Read)
            .unwrap();
        let states = db
            .mark_promotions(&ids(&["b"]), PromotionState::Dismissed)
            .unwrap();
        assert_eq!(states["a"], PromotionState::Read);
        assert_eq!(states["b"], PromotionState::Dismissed);
    }

    #[test]
    fn first_use_is_stamped_once_and_never_moves() {
        let (_dir, db) = test_db();
        let first = db.promotions_first_use().unwrap();
        // Every later call reports the same instant — the date gate would be
        // meaningless if "when did they arrive" drifted forward on every load.
        assert_eq!(db.promotions_first_use().unwrap(), first);
    }

    #[test]
    fn an_unparseable_first_use_is_repaired_rather_than_trusted() {
        let (_dir, db) = test_db();
        db.kv_set("promotions.firstUse", "not a timestamp").unwrap();
        let repaired = db.promotions_first_use().unwrap();
        assert_eq!(db.promotions_first_use().unwrap(), repaired);
    }

    #[test]
    fn a_corrupt_state_map_reshows_promotions_rather_than_hiding_them() {
        let (_dir, db) = test_db();
        db.kv_set("promotions.state", "{not json").unwrap();
        // Garbage must not read as "everything read" — that would switch the
        // feature off with no symptom. It reads as empty, and the next mark
        // repairs the value.
        assert!(db.promotion_states().unwrap().is_empty());
    }

    #[test]
    fn an_unknown_state_name_drops_one_entry_and_keeps_the_rest() {
        let (_dir, db) = test_db();
        // What a *newer* client writing a state this build has no name for
        // looks like. It must cost one re-shown card, not the whole map.
        db.kv_set(
            "promotions.state",
            r#"{"a":"read","b":"snoozed-until-tuesday"}"#,
        )
        .unwrap();
        let states = db.promotion_states().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states["a"], PromotionState::Read);
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
