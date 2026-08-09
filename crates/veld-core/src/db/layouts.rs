//! One worktree's panes — the layout every client of that worktree renders.
//!
//! The daemon is the store and not the author: what a layout *means* lives in
//! the UI (`panes/model.ts`), and this module keeps a JSON document, a version,
//! and nothing else. See `migrate_v14_pane_layouts` for why the opacity is
//! deliberate.
//!
//! Two rules run through it:
//!
//! - **A write states the version it read.** Not to merge concurrent edits —
//!   they are prevented upstream, by one client showing a worktree at a time —
//!   but so a stale writer is *told* rather than silently winning. The window
//!   that yields a worktree can still have a debounced save in flight when the
//!   window that claimed it starts editing, and last-write-wins there costs the
//!   new owner real panes.
//! - **Version 0 means "no row".** A client that has never seen a layout writes
//!   with `expected: 0`, so "create" and "update" are the same call and a first
//!   write cannot silently overwrite a layout another client created in between.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, DbError, ts_to_str};

/// Upper bound on a stored layout, in bytes of JSON.
///
/// A layout is a handful of tabs with short string fields; the realistic
/// ceiling is a few kilobytes. This is not a correctness limit but a refusal to
/// let a client turn a same-origin `PUT` into unbounded per-worktree storage —
/// the payload is attacker-shaped only via the page itself, but the page is
/// also what a bug lives in, and a runaway layout would be discovered as a
/// database that stopped fitting in memory.
pub const MAX_LAYOUT_BYTES: usize = 256 * 1024;

/// A worktree's stored panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneLayout {
    /// Bumped on every accepted write. `0` is the version of a worktree with no
    /// row, which no stored row ever carries.
    pub version: i64,
    /// The UI's layout document, verbatim.
    pub layout: String,
}

/// What a versioned write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutWrite {
    /// Accepted; the row now carries this version.
    Stored(PaneLayout),
    /// Refused: the caller's `expected` did not match. Carries what is actually
    /// stored, so the caller can reconcile in the same round trip rather than
    /// re-reading and racing again.
    Conflict(PaneLayout),
}

/// Rejected before it reached SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutRejected {
    /// Not syntactically JSON. The one thing the daemon checks: a row that does
    /// not parse is one every future reader has to defend against, and the
    /// client that wrote it is the only party that could have noticed.
    NotJson,
    /// Past [`MAX_LAYOUT_BYTES`].
    TooLarge,
}

impl Db {
    /// A worktree's panes, or `None` when it has none stored.
    ///
    /// `None` is distinct from an empty layout: it means "this worktree has
    /// never been arranged", which is what lets a client seed a default without
    /// wondering whether it is discarding somebody's deliberate empty screen.
    pub fn pane_layout(&self, worktree_id: i64) -> Result<Option<PaneLayout>, DbError> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT version, layout FROM pane_layouts WHERE worktree_id = ?1",
                params![worktree_id],
                |r| {
                    Ok(PaneLayout {
                        version: r.get(0)?,
                        layout: r.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Store a worktree's panes if `expected` still matches.
    ///
    /// `expected` is the version the caller read — `0` for "there was no row".
    /// A mismatch returns [`LayoutWrite::Conflict`] with the current state and
    /// writes nothing.
    ///
    /// The read and the write share one transaction. Without it two writers
    /// that both read version 4 could both find their `expected` satisfied and
    /// the second would overwrite the first while being told it had not — which
    /// is the exact failure the version exists to report, reintroduced by the
    /// gap between checking and writing.
    ///
    /// **The worktree must exist.** The foreign key is what stops a layout
    /// outliving the checkout it names (rowids are reused), and with
    /// `foreign_keys=ON` an insert for an unknown worktree fails here rather
    /// than creating an orphan the next `veld worktree create` would inherit.
    pub fn put_pane_layout(
        &self,
        worktree_id: i64,
        expected: i64,
        layout: &str,
    ) -> Result<Result<LayoutWrite, LayoutRejected>, DbError> {
        if layout.len() > MAX_LAYOUT_BYTES {
            return Ok(Err(LayoutRejected::TooLarge));
        }
        if serde_json::from_str::<serde_json::Value>(layout).is_err() {
            return Ok(Err(LayoutRejected::NotJson));
        }

        let mut conn = self.lock();
        // IMMEDIATE: this is a read-then-write, and a deferred transaction
        // upgrades to a write lock only at the `INSERT`, where SQLite can hand
        // back `SQLITE_BUSY` after the read has already been used to decide.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = tx
            .query_row(
                "SELECT version, layout FROM pane_layouts WHERE worktree_id = ?1",
                params![worktree_id],
                |r| {
                    Ok(PaneLayout {
                        version: r.get(0)?,
                        layout: r.get(1)?,
                    })
                },
            )
            .optional()?;

        let have = current.as_ref().map_or(0, |c| c.version);
        if have != expected {
            // No row and `expected != 0` reports version 0 with an empty
            // document rather than inventing one: "there is nothing here" is
            // the honest answer, and the client's own parser treats it as such.
            return Ok(Ok(LayoutWrite::Conflict(current.unwrap_or(PaneLayout {
                version: 0,
                layout: String::new(),
            }))));
        }

        let next = have + 1;
        tx.execute(
            "INSERT INTO pane_layouts (worktree_id, version, layout, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(worktree_id) DO UPDATE SET
                 version    = excluded.version,
                 layout     = excluded.layout,
                 updated_at = excluded.updated_at",
            params![worktree_id, next, layout, ts_to_str(chrono::Utc::now())],
        )?;
        tx.commit()?;
        Ok(Ok(LayoutWrite::Stored(PaneLayout {
            version: next,
            layout: layout.to_owned(),
        })))
    }

    /// Forget a worktree's panes, if `expected` still matches.
    ///
    /// For the client-side "this worktree has no panes left" case only. Worktree
    /// *deletion* is handled by the foreign key, which is the case that has to
    /// be airtight.
    ///
    /// **Versioned like a store**, because it is the destructive one. An
    /// unversioned delete lets a client running on a stale read erase the panes
    /// of whoever holds the worktree now — the exact write the version exists to
    /// refuse, arriving through the one path that skipped the check.
    /// `LayoutWrite::Stored` on success carries version 0: there is no row.
    pub fn delete_pane_layout(
        &self,
        worktree_id: i64,
        expected: i64,
    ) -> Result<LayoutWrite, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = tx
            .query_row(
                "SELECT version, layout FROM pane_layouts WHERE worktree_id = ?1",
                params![worktree_id],
                |r| {
                    Ok(PaneLayout {
                        version: r.get(0)?,
                        layout: r.get(1)?,
                    })
                },
            )
            .optional()?;

        let have = current.as_ref().map_or(0, |c| c.version);
        if have != expected {
            return Ok(LayoutWrite::Conflict(current.unwrap_or(PaneLayout {
                version: 0,
                layout: String::new(),
            })));
        }
        tx.execute(
            "DELETE FROM pane_layouts WHERE worktree_id = ?1",
            params![worktree_id],
        )?;
        tx.commit()?;
        Ok(LayoutWrite::Stored(PaneLayout {
            version: 0,
            layout: String::new(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(db: &Db) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT OR IGNORE INTO repos (root, name, created_at)
             VALUES ('/tmp/repo', 'repo', '2026-01-01T00:00:00.000000Z')",
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

    fn stored(w: Result<LayoutWrite, LayoutRejected>) -> PaneLayout {
        match w.unwrap() {
            LayoutWrite::Stored(l) => l,
            LayoutWrite::Conflict(l) => panic!("expected a store, got a conflict at {}", l.version),
        }
    }

    #[test]
    fn a_worktree_with_no_row_has_no_layout() {
        let (db, _dir) = db();
        assert_eq!(db.pane_layout(worktree(&db)).unwrap(), None);
    }

    /// The first write is the one that has to spell "there was nothing here",
    /// which is what makes create and update the same call.
    #[test]
    fn the_first_write_expects_version_zero_and_lands_at_one() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        let first = stored(db.put_pane_layout(wt, 0, r#"{"docks":[]}"#).unwrap());
        assert_eq!(first.version, 1);
        assert_eq!(db.pane_layout(wt).unwrap().unwrap(), first);

        let second = stored(db.put_pane_layout(wt, 1, r#"{"docks":[1]}"#).unwrap());
        assert_eq!(second.version, 2);
        assert_eq!(second.layout, r#"{"docks":[1]}"#);
    }

    /// **The hand-off guard.** A window that yielded a worktree can still have a
    /// debounced save in flight; without this it would overwrite the panes of
    /// the window that took it over, and be told the write succeeded.
    #[test]
    fn a_write_against_a_stale_version_is_refused_and_reports_the_current_one() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        stored(db.put_pane_layout(wt, 0, r#"{"a":1}"#).unwrap());
        stored(db.put_pane_layout(wt, 1, r#"{"a":2}"#).unwrap());

        // A client still holding version 1 tries to save.
        let out = db.put_pane_layout(wt, 1, r#"{"stale":true}"#).unwrap();
        match out.unwrap() {
            LayoutWrite::Conflict(cur) => {
                assert_eq!(cur.version, 2);
                assert_eq!(cur.layout, r#"{"a":2}"#, "the conflict carries the winner");
            }
            LayoutWrite::Stored(_) => panic!("a stale write must not be stored"),
        }
        assert_eq!(db.pane_layout(wt).unwrap().unwrap().layout, r#"{"a":2}"#);
    }

    /// A client that believes it is creating the row, when somebody else
    /// already did, must lose — otherwise two clients seeding defaults at boot
    /// silently pick a winner by arrival order.
    #[test]
    fn expecting_zero_against_an_existing_row_conflicts() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        stored(db.put_pane_layout(wt, 0, r#"{"real":true}"#).unwrap());
        match db
            .put_pane_layout(wt, 0, r#"{"clobber":true}"#)
            .unwrap()
            .unwrap()
        {
            LayoutWrite::Conflict(cur) => assert_eq!(cur.layout, r#"{"real":true}"#),
            LayoutWrite::Stored(_) => panic!("a second create must not overwrite the first"),
        }
    }

    /// A non-zero expectation against a missing row is the deletion race: the
    /// worktree's layout was dropped while this client held a version.
    #[test]
    fn expecting_a_version_against_a_missing_row_conflicts_at_zero() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        match db.put_pane_layout(wt, 3, "{}").unwrap().unwrap() {
            LayoutWrite::Conflict(cur) => {
                assert_eq!(cur.version, 0);
                assert!(cur.layout.is_empty());
            }
            LayoutWrite::Stored(_) => panic!("must not resurrect a deleted layout"),
        }
    }

    #[test]
    fn a_layout_that_is_not_json_is_refused() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        assert_eq!(
            db.put_pane_layout(wt, 0, "not json").unwrap(),
            Err(LayoutRejected::NotJson)
        );
        assert_eq!(db.pane_layout(wt).unwrap(), None);
    }

    #[test]
    fn an_oversized_layout_is_refused() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        let big = format!(r#"{{"pad":"{}"}}"#, "x".repeat(MAX_LAYOUT_BYTES));
        assert_eq!(
            db.put_pane_layout(wt, 0, &big).unwrap(),
            Err(LayoutRejected::TooLarge)
        );
    }

    /// Worktree rowids are reused, so a layout row that outlived its worktree
    /// would eventually be handed to an unrelated checkout — together with the
    /// terminal session ids it names. The foreign key is what prevents it.
    #[test]
    fn deleting_a_worktree_takes_its_layout_with_it() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        stored(db.put_pane_layout(wt, 0, "{}").unwrap());
        db.lock()
            .execute("DELETE FROM worktrees WHERE id = ?1", params![wt])
            .unwrap();
        assert_eq!(db.pane_layout(wt).unwrap(), None);
    }

    /// And the same key is why an unknown worktree cannot be given a layout at
    /// all: the orphan would be adopted by whichever checkout is created next.
    #[test]
    fn a_layout_for_an_unknown_worktree_is_rejected_by_the_foreign_key() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        assert!(db.put_pane_layout(wt + 5000, 0, "{}").is_err());
    }

    #[test]
    fn deleting_a_layout_returns_the_worktree_to_having_none() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        stored(db.put_pane_layout(wt, 0, "{}").unwrap());
        assert!(matches!(
            db.delete_pane_layout(wt, 1).unwrap(),
            LayoutWrite::Stored(_)
        ));
        assert_eq!(db.pane_layout(wt).unwrap(), None);
        // …and the version restarts, which is what `expected: 0` means.
        assert_eq!(stored(db.put_pane_layout(wt, 0, "{}").unwrap()).version, 1);
    }

    /// The destructive write is the one that most needs the check: a client
    /// running on a stale read would otherwise erase the panes of whoever holds
    /// the worktree now.
    #[test]
    fn a_stale_delete_is_refused_and_leaves_the_layout_alone() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        stored(db.put_pane_layout(wt, 0, r#"{"a":1}"#).unwrap());
        stored(db.put_pane_layout(wt, 1, r#"{"a":2}"#).unwrap());
        match db.delete_pane_layout(wt, 1).unwrap() {
            LayoutWrite::Conflict(cur) => assert_eq!(cur.version, 2),
            LayoutWrite::Stored(_) => panic!("a stale delete must not erase a layout"),
        }
        assert_eq!(db.pane_layout(wt).unwrap().unwrap().layout, r#"{"a":2}"#);
    }

    /// Deleting a worktree that has no layout is not an error — the client that
    /// closed the last pane and the one that never opened one agree on the
    /// outcome.
    #[test]
    fn deleting_an_absent_layout_at_version_zero_succeeds() {
        let (db, _dir) = db();
        let wt = worktree(&db);
        assert!(matches!(
            db.delete_pane_layout(wt, 0).unwrap(),
            LayoutWrite::Stored(_)
        ));
    }
}
