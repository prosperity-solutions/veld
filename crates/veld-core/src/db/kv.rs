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

/// Worktree root → its file-serving grant. See [`Db::file_grant_for_root`].
const FILE_GRANT_PREFIX: &str = "files.grant:";
/// Grant → the worktree root it stands for. The reverse of the above, stored so a
/// request resolves in one row read.
const FILE_ROOT_PREFIX: &str = "files.root:";

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
    // File-serving grants — the unguessable stand-in for a worktree root
    // -----------------------------------------------------------------------

    /// This worktree root's grant, minting one the first time it is asked for.
    ///
    /// A grant is the first path segment of every file URL a pane loads, and it is
    /// what the daemon resolves back into a directory — so the client never names a
    /// path, the way a PTY ticket already carries its worktree dir. Random rather
    /// than derived: a random value needs no key management and no hand-rolled MAC,
    /// and the property wanted here is only unguessability.
    ///
    /// **Keyed on the root path, never on the worktree row id.** Row ids are reused
    /// when a worktree is deleted and another created, so a grant keyed on one would
    /// silently start serving a *different* project's files to a pane URL persisted
    /// in `veld.panes.v1` before the swap.
    ///
    /// Stored in both directions so the reverse lookup at request time is one row
    /// read rather than a scan of every worktree.
    pub fn file_grant_for_root(&self, root: &str) -> Result<String, DbError> {
        let forward = format!("{FILE_GRANT_PREFIX}{root}");
        if let Some(existing) = self.kv_get(&forward)? {
            if !existing.is_empty() {
                return Ok(existing);
            }
        }
        let grant = uuid::Uuid::new_v4().simple().to_string();
        self.kv_set(&forward, &grant)?;
        self.kv_set(&format!("{FILE_ROOT_PREFIX}{grant}"), root)?;
        Ok(grant)
    }

    /// The worktree root a grant stands for, if this daemon ever minted it.
    ///
    /// Answering this does **not** mean the directory may be served: the caller
    /// still has to confirm the root is a live worktree and confine the path to it.
    /// This is the lookup, not the authorisation.
    pub fn file_grant_root(&self, grant: &str) -> Result<Option<String>, DbError> {
        self.kv_get(&format!("{FILE_ROOT_PREFIX}{grant}"))
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
    /// visible in the history, never counted as unread, never prompted — which is
    /// how somebody installing Veld today avoids a modal about a change that
    /// shipped last spring.
    ///
    /// **The stamp is the earliest evidence this user predates now, not the
    /// clock.** Reaching for `now()` looks obviously right and is wrong for the
    /// cohort that matters most: every *existing* user meets this code for the
    /// first time on the day they upgrade, so a stamp of "now" declares an
    /// eight-month user brand new, and the promotion shipped in that very release
    /// is dated before their "arrival" and auto-read for everyone who opens a day
    /// late. The channel would launch reaching almost nobody. So the oldest
    /// registered repo wins when there is one: a user with repos has demonstrably
    /// been here since that day, whatever this row says.
    ///
    /// Deliberately **not** derived from anything that looks like database *age*.
    /// The tempting version — "a database with no rows is a new user" — is wrong
    /// here in a way that bites daily: `veld start --preset dev` mints
    /// `.veld-dev/<run>/veld.db` several times a day, and either the CLI or the
    /// daemon may be the process that creates one. Note the asymmetry that makes
    /// the repo evidence safe where freshness is not: a repo row is proof the
    /// user *was* here, while an empty table proves nothing at all, so this only
    /// ever moves the stamp **backwards** and a fresh dev database simply falls
    /// through to now.
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
                // The oldest repo this user registered, or now. See the note
                // above on why "now" alone silently kills the launch cohort.
                let oldest: Option<String> = tx
                    .query_row("SELECT MIN(created_at) FROM repos", [], |r| r.get(0))
                    .optional()?
                    .flatten();
                let stamp = oldest
                    .as_deref()
                    .and_then(parse_ts)
                    .unwrap_or_else(chrono::Utc::now);
                let encoded = super::ts_to_str(stamp);
                tx.execute(
                    "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                    params![PROMOTIONS_FIRST_USE_KEY, encoded],
                )?;
                // Return what was *stored*, never what was measured. `ts_to_str`
                // truncates to microseconds, so on a clock with finer resolution
                // (Linux; macOS is microsecond-granular and hides this) the
                // stamping call would report an instant no later call ever
                // reproduces — and "stamped once and never moves" would be false
                // for exactly one caller, the one that established it.
                parse_ts(&encoded).unwrap_or(stamp)
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
        // Merge over the **raw** map, not the decoded one. `decode_states` drops
        // entries this build has no name for, and its doc calls that loss
        // temporary — "costs one re-shown card". Decoding here and writing the
        // result back would make it permanent on the very next mark, which is
        // precisely the downgrade case the forward-compatibility is for: a newer
        // daemon writes a state this one cannot read, this one marks anything,
        // and the newer daemon's knowledge is gone.
        let mut raw: BTreeMap<String, serde_json::Value> = stored
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or_default();
        let read = serde_json::to_value(PromotionState::Read).expect("an enum serialises");
        let target = serde_json::to_value(state).expect("an enum serialises");
        for id in ids {
            match raw.get(id) {
                // A read is never walked back — not by a concurrent dismiss, and
                // not by this client's own later one.
                Some(existing) if *existing == read => {}
                _ => {
                    raw.insert(id.clone(), target.clone());
                }
            }
        }
        let encoded = serde_json::to_string(&raw).expect("a map of JSON values serialises");
        tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![PROMOTIONS_STATE_KEY, encoded, now_str()],
        )?;
        let states = decode_states(&encoded);
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
    fn the_stamp_it_returns_parses_back_out_of_what_it_stored() {
        let (_dir, db) = test_db();
        let stamped = db.promotions_first_use().unwrap();
        let stored = db.kv_get("promotions.firstUse").unwrap().unwrap();
        // Assert the **parse** direction. Comparing `ts_to_str(stamped)` to the
        // stored string instead is a tautology that passes against the bug: it
        // applies the same microsecond truncation to both sides, so the returned
        // value could be a finer instant and nothing would notice.
        //
        // Scope of the claim, stated because the name cannot carry it: this only
        // *fails* where the clock is finer than microseconds. macOS's
        // `CLOCK_REALTIME` is microsecond-granular, so the original bug was
        // invisible here and red on CI's Linux — no assertion can close that gap
        // on a clock that cannot express the difference.
        assert_eq!(super::super::parse_ts(&stored), Some(stamped));
    }

    #[test]
    fn an_existing_user_is_dated_by_their_oldest_repo_not_by_the_upgrade() {
        let (_dir, db) = test_db();
        db.upsert_repo(std::path::Path::new("/tmp/some-repo"), "some-repo")
            .unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE repos SET created_at = '2025-03-04T10:00:00.000000Z'",
                [],
            )
            .unwrap();
        }
        // Every existing user meets this code for the first time on the day they
        // upgrade. Stamping "now" would call them brand new and auto-read the
        // promotion shipped in that very release — the channel would launch
        // reaching almost nobody.
        let first_use = db.promotions_first_use().unwrap();
        assert_eq!(first_use.to_rfc3339(), "2025-03-04T10:00:00+00:00");
    }

    #[test]
    fn a_user_with_no_repos_is_dated_now_so_a_fresh_dev_db_stays_quiet() {
        let (_dir, db) = test_db();
        let before = chrono::Utc::now() - chrono::Duration::seconds(5);
        // An empty repos table proves nothing, so the evidence only ever moves
        // the stamp backwards — a throwaway `.veld-dev/<run>/veld.db` falls
        // through to now and gets no back-catalogue.
        assert!(db.promotions_first_use().unwrap() > before);
    }

    #[test]
    fn a_mark_preserves_a_state_name_this_build_cannot_read() {
        let (_dir, db) = test_db();
        db.kv_set(
            "promotions.state",
            r#"{"a":"read","b":"snoozed-until-tuesday"}"#,
        )
        .unwrap();
        db.mark_promotions(&ids(&["c"]), PromotionState::Dismissed)
            .unwrap();
        // The downgrade case the forward-compatibility exists for: decoding the
        // map and writing the decoded result back would erase `b` on the first
        // mark, turning a re-shown card into permanent data loss.
        let raw = db.kv_get("promotions.state").unwrap().unwrap();
        assert!(raw.contains("snoozed-until-tuesday"), "{raw}");
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
