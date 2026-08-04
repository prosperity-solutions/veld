//! The desktop app's repo/worktree registry, stored in the central database.
//!
//! A "repo" is a git repository the user imported into Veld Desktop (keyed by
//! the main checkout root); "worktrees" are its `git worktree` checkouts, each
//! with a user-editable alias. Rows live in the `repos`/`worktrees` tables
//! (see the v5 migration). Run state is NOT duplicated here — callers join a
//! worktree to veld state by path (`worktrees.path` = `projects.root`).

use std::path::Path;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::state::root_key;
use super::{Db, DbError, now_str};

/// An imported git repository (main checkout).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRecord {
    pub root: String,
    pub name: String,
    pub created_at: String,
}

/// One checkout of a repo — either the main checkout (`is_main`) or a
/// `git worktree` checkout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub id: i64,
    pub repo_root: String,
    pub path: String,
    pub branch: String,
    pub alias: String,
    /// Visual identifier for dense UI (collapsed rail): one emoji from
    /// [`WORKTREE_EMOJI`]. Assigned at insert, preserved across syncs and
    /// renames.
    ///
    /// **Not an identity.** Uniqueness is a property of *assignment* only —
    /// [`pick_emoji`] probes for a free glyph among the repo's siblings, but
    /// [`Db::patch_worktree`] lets the user pick one that is already taken.
    /// Don't add a `UNIQUE` index, and don't assume one holder per glyph.
    pub emoji: String,
    /// The colour half of the marker: a literal `#rrggbb`, or `""` for "not
    /// assigned yet" (backfilled on the next sync, exactly as `emoji` is).
    ///
    /// A worktree's marker is a **composite** — this colour and the `emoji` glyph —
    /// and the `worktree.markerStyle` setting picks which face renders. Both faces
    /// are stored permanently and neither is cleared by a style change, which is
    /// what makes switching colour → emoji and back lossless. Do not "simplify"
    /// this into a tagged union: that turns a rendering choice back into a data
    /// migration, and the user's hand-picked glyph becomes something a preference
    /// can destroy.
    ///
    /// **The colour, not an index into a palette.** An index meant retuning the
    /// palette repainted every existing worktree, could not express a custom
    /// colour, and coupled Rust to a stylesheet. See the v9 migration.
    pub marker_color: String,
    pub is_main: bool,
    pub created_at: String,
    /// The rail lane this worktree is grouped into — a [`LaneRecord::name`] of
    /// the same repo, or `""` for ungrouped.
    ///
    /// **The name, not a surrogate id, and stored on this row rather than in a
    /// side table.** Markers are auto-assigned for uniqueness; lanes group
    /// deliberately, so a lane is a many-to-one label and its natural home is the
    /// row it labels. Keeping it here is also what makes it immune to the rowid
    /// reuse that broke three stores in #201: there is no key pointing at this
    /// worktree that could outlive it. See the v10 migration.
    ///
    /// A name this repo has no lane for renders as ungrouped rather than as an
    /// error — the read path is deliberately tolerant, and [`Db::delete_lane`]
    /// clears assignments in the same transaction so it should not arise.
    pub lane: String,
    /// Manual position within the lane, or `None` for "the user has not placed
    /// this one" — which sorts to an alias-ordered tail rather than to position 0.
    ///
    /// A newly discovered worktree is always `None`, so the reconcile pass never
    /// authors user intent and a new checkout cannot appear wedged into the middle
    /// of a hand-made order.
    pub sort_position: Option<i64>,
    /// When the user asked for this worktree to be removed, or `""` for a live
    /// worktree. Removal runs in the background, so this is the durable record of
    /// intent that survives a daemon restart *and* the flag
    /// [`Db::sync_worktrees`] checks so it does not resurrect the row.
    pub trashed_at: String,
    /// Why the last removal attempt failed, or `""`. Set together with clearing
    /// [`Self::trashed_at`]: a failed removal takes the worktree back out of trash
    /// with the reason attached, rather than leaving it in a state that looks like
    /// pending work forever.
    pub trash_error: String,
}

/// A user-defined rail lane — a named group of worktrees within one repo.
///
/// Identified by `(repo_root, name)`; there is deliberately no surrogate id (see
/// the v10 migration). `position` orders the lanes in the rail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneRecord {
    pub repo_root: String,
    pub name: String,
    pub position: i64,
    pub created_at: String,
}

/// Longest accepted lane name. Lanes are rail labels read at a glance in a
/// 236px-wide column, so the cap is about legibility, not storage.
pub const MAX_LANE_NAME_LEN: usize = 32;

/// Most lanes one repo may define. A rail that needs more than this is not
/// being organised by it.
pub const MAX_LANES_PER_REPO: usize = 32;

/// Longest accepted reorder payload, for worktrees or lanes.
///
/// Generous against any real repo (a rail with a thousand checkouts is not a rail)
/// and small enough that the per-element `UPDATE` loop cannot hold the write lock
/// long enough to stall the daemon's other writers.
pub const MAX_ORDER_LEN: usize = 1024;

/// A worktree as discovered on disk (`git worktree list --porcelain`), used
/// to sync the table with reality.
#[derive(Debug, Clone)]
pub struct DiscoveredWorktree {
    pub path: String,
    pub branch: String,
    pub is_main: bool,
}

// Column order is load-bearing: wt_from_row reads by index, and the INSERT /
// UPDATE statements in sync_worktrees hand-list the same columns. Adding a
// field means touching all of them (plus a NEW migration — never edit v5) AND
// the TS `Worktree` interface in crates/veld-daemon/ui/src/api.ts — serde
// flattens the new field into the API, but TS ignores unknown fields silently.
const WT_COLS: &str = "id, repo_root, path, branch, alias, emoji, is_main, created_at, \
     marker_color, lane, sort_position, trashed_at, trash_error";

fn wt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeRecord> {
    Ok(WorktreeRecord {
        id: row.get(0)?,
        repo_root: row.get(1)?,
        path: row.get(2)?,
        branch: row.get(3)?,
        alias: row.get(4)?,
        emoji: row.get(5)?,
        is_main: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
        marker_color: row.get(8)?,
        lane: row.get(9)?,
        sort_position: row.get(10)?,
        trashed_at: row.get(11)?,
        trash_error: row.get(12)?,
    })
}

/// The rail's render order, shared by every query that returns worktrees.
///
/// Ungrouped worktrees (`lane = ''`) come first, so a repo with no lanes defined
/// sorts exactly as it did before v10; then lanes in their own `position` order,
/// resolved by a correlated subquery because `lane` stores the name. Within a
/// group the main checkout leads, then hand-placed worktrees in their positions,
/// then everything unplaced alias-sorted — `sort_position IS NULL` sorts the
/// unplaced *after* the placed rather than treating NULL as position zero.
const WT_ORDER: &str = "ORDER BY lane != '',
              COALESCE((SELECT position FROM lanes l
                        WHERE l.repo_root = worktrees.repo_root AND l.name = worktrees.lane), 0),
              lane COLLATE NOCASE,
              is_main DESC, sort_position IS NULL, sort_position,
              alias COLLATE NOCASE";

impl Db {
    /// Register (or re-register) a repo. Idempotent: an existing row keeps its
    /// `created_at` and only updates the name.
    pub fn upsert_repo(&self, root: &Path, name: &str) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO repos (root, name, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(root) DO UPDATE SET name = excluded.name",
            params![root_key(root), name, now_str()],
        )?;
        Ok(())
    }

    /// All imported repos, name-sorted.
    pub fn list_repos(&self) -> Result<Vec<RepoRecord>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT root, name, created_at FROM repos ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RepoRecord {
                root: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Look up one repo by root path.
    pub fn get_repo(&self, root: &Path) -> Result<Option<RepoRecord>, DbError> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT root, name, created_at FROM repos WHERE root = ?1",
                params![root_key(root)],
                |row| {
                    Ok(RepoRecord {
                        root: row.get(0)?,
                        name: row.get(1)?,
                        created_at: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Unregister a repo. Worktree rows cascade-delete; the filesystem is
    /// never touched. Returns whether a row existed.
    pub fn remove_repo(&self, root: &Path) -> Result<bool, DbError> {
        let conn = self.lock();
        let n = conn.execute("DELETE FROM repos WHERE root = ?1", params![root_key(root)])?;
        Ok(n > 0)
    }

    /// Reconcile a repo's worktree rows with the set discovered on disk, in
    /// one transaction: insert new paths (alias = `default_alias(branch)`,
    /// de-duplicated with a numeric suffix), update `branch`/`is_main` on
    /// existing rows (a worktree can switch branches), and delete rows whose
    /// path vanished. User-chosen aliases on surviving rows are preserved.
    pub fn sync_worktrees(
        &self,
        repo_root: &Path,
        discovered: &[DiscoveredWorktree],
    ) -> Result<Vec<WorktreeRecord>, DbError> {
        // Guard the degenerate case explicitly: an empty `discovered` would
        // make the prune below `path NOT IN ()` — which SQLite evaluates as
        // true-for-all, silently wiping every row for the repo. Current
        // callers always pass ≥1 entry (git lists the main checkout), but a
        // parse-to-empty regression must not become a wipe.
        if discovered.is_empty() {
            return self.list_worktrees(repo_root);
        }
        let root = root_key(repo_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            // Delete rows for paths that no longer exist on disk.
            let keep: Vec<&str> = discovered.iter().map(|d| d.path.as_str()).collect();
            let placeholders = std::iter::repeat_n("?", keep.len())
                .collect::<Vec<_>>()
                .join(",");
            let mut params_vec: Vec<&dyn rusqlite::ToSql> = vec![&root];
            for p in &keep {
                params_vec.push(p);
            }
            tx.execute(
                &format!(
                    "DELETE FROM worktrees WHERE repo_root = ?1
                     AND path NOT IN ({placeholders})"
                ),
                params_vec.as_slice(),
            )?;

            for d in discovered {
                let existing: Option<(i64, String)> = tx
                    .query_row(
                        "SELECT id, trashed_at FROM worktrees WHERE path = ?1",
                        params![d.path],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                // Trashed rows are updated like any other, and that is deliberate.
                //
                // A worktree in the trash is still a real `git worktree` — the
                // checkout stays on disk for the whole retention period, which
                // defaults to "until the user empties it", i.e. possibly forever. An
                // earlier version skipped these rows to avoid writing to something
                // "about to be deleted"; with an unbounded retention that reasoning
                // does not hold, and the cost was that a branch switch made inside a
                // trashed checkout stayed invisible for as long as it sat there.
                //
                // Nothing here clears `trashed_at`, so the row cannot be resurrected
                // by a poll. The anti-resurrection property comes from the DELETE
                // above sparing it: its path is still in `discovered`, because the
                // checkout is still there. And the mirror case needs no code either —
                // once the deletion runs, the path leaves `discovered` and that same
                // DELETE reaps the row, which is what makes a re-run of an
                // already-completed deletion idempotent.
                if let Some((id, _)) = existing {
                    // **Only columns derived from git belong in this UPDATE.**
                    // It runs on every discovery poll (every few seconds), so a
                    // column listed here is overwritten from `discovered` forever —
                    // and a *user-choice* column would therefore be silently reset
                    // seconds after the user set it, having appeared to work when
                    // tested by hand. `lane`, `sort_position`, `alias`, the marker
                    // faces and the trash columns are all deliberately absent for
                    // that reason; they are written by `patch_worktree`,
                    // `reorder_worktrees` and the trash helpers instead. The
                    // file-header note about "touching all of them" when adding a
                    // column means WT_COLS, `wt_from_row` and the INSERT — not this
                    // statement.
                    //
                    // Write only on change: steady-state syncs must not take the
                    // write path and append WAL frames for identical rows.
                    tx.execute(
                        "UPDATE worktrees SET branch = ?1, is_main = ?2, repo_root = ?3
                         WHERE id = ?4
                           AND (branch != ?1 OR is_main != ?2 OR repo_root != ?3)",
                        params![d.branch, d.is_main as i64, root, id],
                    )?;
                    // Backfill both marker faces: `emoji` for rows created
                    // before v6, `marker_color` for rows created before v9. They
                    // are read together and backfilled independently, because a
                    // row upgraded through v6 already has a glyph and needs only
                    // the colour.
                    let (cur_emoji, cur_color): (String, String) = tx.query_row(
                        "SELECT emoji, marker_color FROM worktrees WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    if cur_emoji.is_empty() {
                        let emoji = pick_emoji(&tx, &root, &d.branch)?;
                        tx.execute(
                            "UPDATE worktrees SET emoji = ?1 WHERE id = ?2",
                            params![emoji, id],
                        )?;
                    }
                    // `is_empty()`, not `!is_worktree_color(...)`, so the two
                    // faces behave alike. Testing validity would overwrite any
                    // value *this* binary does not recognise — and the palette doc
                    // promises widening the validator (`#rrggbbaa`, a named colour)
                    // is "a UI addition, not a schema change", so an older daemon's
                    // next sync would silently repaint every hand-picked marker.
                    if cur_color.is_empty() {
                        let color = pick_color(&tx, &root, &d.branch)?;
                        tx.execute(
                            "UPDATE worktrees SET marker_color = ?1 WHERE id = ?2",
                            params![color, id],
                        )?;
                    }
                } else {
                    let alias = unique_alias(&tx, &root, &default_alias(&d.branch))?;
                    let emoji = pick_emoji(&tx, &root, &alias)?;
                    let color = pick_color(&tx, &root, &alias)?;
                    tx.execute(
                        "INSERT INTO worktrees
                            (repo_root, path, branch, alias, emoji, is_main, created_at,
                             marker_color)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            root,
                            d.path,
                            d.branch,
                            alias,
                            emoji,
                            d.is_main as i64,
                            now_str(),
                            color
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        drop(conn);
        self.list_worktrees(repo_root)
    }

    /// All worktrees of a repo in rail order ([`WT_ORDER`]): ungrouped first,
    /// then lanes in their own order, main checkout leading its group, then
    /// hand-placed worktrees, then the unplaced alias-sorted.
    ///
    /// Trashed worktrees are included — the rail renders them as a pending-removal
    /// group, which is what makes a background removal visible at all.
    pub fn list_worktrees(&self, repo_root: &Path) -> Result<Vec<WorktreeRecord>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {WT_COLS} FROM worktrees WHERE repo_root = ?1 {WT_ORDER}"
        ))?;
        let rows = stmt.query_map(params![root_key(repo_root)], wt_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Look up one worktree by its checkout path.
    ///
    /// `path` is `UNIQUE`, so this is the stable lookup — unlike an id, which is a
    /// rowid SQLite reuses.
    pub fn get_worktree_by_path(&self, path: &str) -> Result<Option<WorktreeRecord>, DbError> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare_cached(&format!("SELECT {WT_COLS} FROM worktrees WHERE path = ?1"))?;
        Ok(stmt.query_row(params![path], wt_from_row).optional()?)
    }

    /// Look up one worktree by id.
    pub fn get_worktree(&self, id: i64) -> Result<Option<WorktreeRecord>, DbError> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare_cached(&format!("SELECT {WT_COLS} FROM worktrees WHERE id = ?1"))?;
        Ok(stmt.query_row(params![id], wt_from_row).optional()?)
    }

    /// Rename a worktree's alias. Returns whether the row existed;
    /// [`DbError::AliasTaken`] when a sibling of the same repo already holds
    /// the alias.
    pub fn rename_worktree(&self, id: i64, alias: &str) -> Result<bool, DbError> {
        self.patch_worktree(id, Some(alias), None, None, None)
    }

    /// Update alias and/or emoji in one write, inside one transaction.
    /// Returns whether the row existed.
    ///
    /// A single `UPDATE … COALESCE` rather than two writes: applied
    /// separately, a row deleted between them (concurrent `sync_worktrees`
    /// prune, or `DELETE /api/worktrees/{id}`) commits the rename and then
    /// reports "not found" — a partial write the API's contract denies.
    /// `None` leaves a column untouched; both `None` is a no-op write that
    /// still reports whether the row exists (the HTTP layer rejects an empty
    /// patch before reaching here).
    ///
    /// A new alias's **slug** must be free among the row's repo siblings, the
    /// same invariant [`unique_alias`] establishes at insert.
    ///
    /// Why per-repo rather than global: the alias becomes the default run name,
    /// and the run name feeds the hostname
    /// `{service}.{run}.{project}.localhost`, where `{project}` is the config's
    /// `name` (both slugified — see `veld_core::url`). Two checkouts of ONE repo
    /// share a `veld.json`, hence one `{project}` — equal aliases there mint
    /// byte-identical hostnames and collide in Caddy. Two *different* repos normally differ in `{project}`,
    /// so duplicate aliases across repos are harmless, and rejecting them would
    /// break importing two repos that are both on `main`. Emoji stay
    /// deliberately non-unique (see [`WorktreeRecord::emoji`]).
    pub fn patch_worktree(
        &self,
        id: i64,
        alias: Option<&str>,
        emoji: Option<&str>,
        marker_color: Option<&str>,
        lane: Option<&str>,
    ) -> Result<bool, DbError> {
        // Both marker channels are validated before either is written, for the
        // same both-or-neither reason the alias is: a patch carrying a good glyph
        // and a bad colour must not half-apply.
        if let Some(e) = emoji {
            if !is_worktree_emoji(e) {
                return Err(DbError::InvalidEmoji(e.to_owned()));
            }
        }
        if let Some(c) = marker_color {
            if !is_worktree_color(c) {
                return Err(DbError::InvalidColor(c.to_owned()));
            }
        }
        let mut conn = self.lock();
        // The collision check and the write share one IMMEDIATE transaction,
        // which is what makes them atomic against ANY concurrent writer.
        //
        // Not merely across processes: `Db::open` builds a fresh connection and a
        // fresh mutex per call and the daemon opens one per HTTP request, so two
        // concurrent PATCHes inside one daemon hold different mutexes over
        // different connections and race exactly like two processes would. The
        // connection mutex serializes nothing between them. Read-then-write as
        // two statements would let both observe the alias free; the loser of an
        // IMMEDIATE race waits out `busy_timeout` instead. There is no
        // `UNIQUE(repo_root, alias)` index to lean on, and adding a `slug`
        // column with a unique index — the SQL-checkable version of the
        // invariant below — was considered and rejected for #172: a database
        // that already holds `main-2` and `main_2` (both legal before that
        // check) would fail the index creation and brick on upgrade, and the
        // alternative, de-duplicating inside the migration, silently rewrites
        // the user's aliases. Enforcing it at the two write paths (here and
        // [`unique_alias`]), both already serialized by IMMEDIATE, holds the
        // invariant going forward and leaves existing rows untouched.
        //
        // One residual path is left open deliberately: `sync_worktrees` can
        // move a row to a different `repo_root` (the `UPDATE … SET repo_root`
        // on a row matched by `path` alone) carrying its alias unchanged. It is
        // unreachable through the API — `import_repo` resolves every import to
        // the repo's MAIN checkout, so two imports of one repo land on the same
        // root, and `git worktree move` changes the path, which prunes and
        // re-inserts through `unique_alias`.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(alias) = alias {
            // Scoped to the row's own repo, and excluding the row itself so
            // renaming a worktree to the alias it already has stays a no-op
            // rather than a self-collision.
            //
            // Compared as SLUGS, not as aliases (#172): the invariant is about
            // the hostname, and the hostname is `slugify(alias)`. `slugify`
            // lowercases AND maps every non-alphanumeric to `-`, so `Main`,
            // `main`, `main-2`, `main_2` and `main.2` are five legal aliases
            // (`is_safe_identifier` allows A-Z, `-`, `_`, `.`) minting exactly
            // two hostnames — `main` and `main-2`. An alias comparison — even
            // `COLLATE NOCASE` — reports the separator variants as free and the
            // collision in Caddy still happens.
            //
            // Done in Rust rather than SQL because SQLite cannot slugify. The
            // sibling set of one repo is small (checkouts of one repo), and the
            // read shares this IMMEDIATE transaction with the write, so it is
            // exactly as atomic as the `EXISTS` query it replaces.
            let sibling_aliases: Vec<String> = tx
                .prepare(
                    "SELECT alias FROM worktrees sibling
                      WHERE sibling.id != ?1
                        AND sibling.repo_root =
                            (SELECT repo_root FROM worktrees WHERE id = ?1)",
                )?
                .query_map(params![id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let slug = crate::url::slugify(alias);
            if sibling_aliases
                .iter()
                .any(|s| crate::url::slugify(s) == slug)
            {
                return Err(DbError::AliasTaken(alias.to_owned()));
            }
        }
        // A lane assignment must name a lane this repo actually defines (or `""`
        // to ungroup). Checked inside the same transaction as the write, so a
        // concurrent `delete_lane` cannot slip a dangling name past it. The read
        // path tolerates a dangling name anyway — belt and braces, because the
        // alternative is a worktree the user cannot see in any group.
        if let Some(lane) = lane {
            if !lane.is_empty() {
                let known: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM lanes
                      WHERE repo_root = (SELECT repo_root FROM worktrees WHERE id = ?1)
                        AND name = ?2)",
                    params![id, lane],
                    |r| r.get(0),
                )?;
                if !known {
                    return Err(DbError::UnknownLane(lane.to_owned()));
                }
            }
        }
        // A lane change clears the manual position.
        //
        // A position is only meaningful *within* a lane, and positions are dense
        // repo-wide once anything has been dragged (`reorder_worktrees` writes array
        // indices), so a worktree carrying its old lane's position into a new one
        // almost always ties an existing member — and `WT_ORDER` then falls through
        // to the alias, landing it somewhere in the middle rather than where "Move to
        // lane" implied. Unplaced is the honest state for a worktree the user moved
        // by menu rather than by dragging it to a position.
        let n = tx.execute(
            "UPDATE worktrees
                SET alias = COALESCE(?1, alias),
                    emoji = COALESCE(?2, emoji),
                    marker_color = COALESCE(?3, marker_color),
                    lane = COALESCE(?4, lane),
                    sort_position = CASE
                        WHEN ?4 IS NOT NULL AND ?4 != lane THEN NULL
                        ELSE sort_position
                    END
              WHERE id = ?5",
            params![alias, emoji, marker_color, lane, id],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Delete a worktree row (DB only — `git worktree remove` is the caller's
    /// job). Returns whether the row existed.
    ///
    /// **Not the way to delete a worktree.** This is the last step of a removal,
    /// not a removal: it skips stopping the runs in the checkout, the git removal
    /// itself, and the trash state that makes the operation visible and undoable.
    /// A handler that calls this directly leaves the checkout on disk with no row
    /// pointing at it — until the next reconcile poll re-registers it under a fresh
    /// alias, with its lane, marker and position gone.
    ///
    /// The two legitimate callers are `worktree_trash::process` (after git has
    /// actually removed it) and the forced branch of the `delete_worktree` handler.
    /// Anything else wants [`Self::trash_worktree`].
    pub fn remove_worktree(&self, id: i64) -> Result<bool, DbError> {
        let conn = self.lock();
        let n = conn.execute("DELETE FROM worktrees WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    // -----------------------------------------------------------------------
    // Rail lanes (§18)
    // -----------------------------------------------------------------------

    /// A repo's lanes, in rail order.
    pub fn list_lanes(&self, repo_root: &Path) -> Result<Vec<LaneRecord>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT repo_root, name, position, created_at FROM lanes
              WHERE repo_root = ?1 ORDER BY position, name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map(params![root_key(repo_root)], |r| {
            Ok(LaneRecord {
                repo_root: r.get(0)?,
                name: r.get(1)?,
                position: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Validate a lane name, returning the trimmed form.
    ///
    /// Control characters are rejected, not stripped. A lane name is rendered as a
    /// rail header and used as a client-side group key, so a name carrying a NUL or
    /// a newline is either invisible garbage in the UI or a collision with a
    /// sentinel — and silently rewriting what the user typed is worse than telling
    /// them it is not a name.
    fn valid_lane_name(name: &str) -> Result<&str, DbError> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > MAX_LANE_NAME_LEN
            || name.chars().any(char::is_control)
            // `.` and `..` are rejected because the name is a URL path segment:
            // `encodeURIComponent` leaves dots unescaped and the URL parser then
            // resolves them away, so `PATCH /api/lanes/..` arrives as `/api/` and
            // `DELETE /api/lanes/.` as `/api/lanes/`. A lane so named would sit in
            // the rail permanently, impossible to rename or delete.
            || name == "."
            || name == ".."
        {
            return Err(DbError::InvalidLaneName(name.to_owned()));
        }
        Ok(name)
    }

    /// Case-fold a lane name for collision checks.
    ///
    /// `to_lowercase`, not `eq_ignore_ascii_case`: the client checks with
    /// JavaScript's `toLowerCase`, which is Unicode-aware, so an ASCII-only compare
    /// here made the two disagree — the dialog refused `ärger` beside `Ärger` as a
    /// duplicate while the daemon would have accepted it, and the reverse for
    /// `K` (U+212A). The `lanes` primary key collates BINARY, so this comparison is
    /// the *only* thing enforcing case-insensitive uniqueness; it has to be the same
    /// rule on both sides.
    fn lane_fold(name: &str) -> String {
        name.to_lowercase()
    }

    /// Create a lane at the end of the repo's rail.
    ///
    /// Names are trimmed and compared case-insensitively: `Review` and `review`
    /// are the same lane, because two rail headers differing only in case is a
    /// mistake every time. Rejects a duplicate rather than silently returning the
    /// existing one, so the UI can say so.
    pub fn create_lane(&self, repo_root: &Path, name: &str) -> Result<LaneRecord, DbError> {
        let name = Self::valid_lane_name(name)?;
        let root = root_key(repo_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Vec<(String, i64)> = tx
            .prepare("SELECT name, position FROM lanes WHERE repo_root = ?1")?
            .query_map(params![root], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        if existing.len() >= MAX_LANES_PER_REPO {
            return Err(DbError::TooManyLanes(MAX_LANES_PER_REPO));
        }
        let folded = Self::lane_fold(name);
        if existing.iter().any(|(n, _)| Self::lane_fold(n) == folded) {
            return Err(DbError::LaneTaken(name.to_owned()));
        }
        let position = existing.iter().map(|(_, p)| *p).max().unwrap_or(-1) + 1;
        let created_at = now_str();
        tx.execute(
            "INSERT INTO lanes (repo_root, name, position, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![root, name, position, created_at],
        )?;
        tx.commit()?;
        Ok(LaneRecord {
            repo_root: root,
            name: name.to_owned(),
            position,
            created_at,
        })
    }

    /// Rename a lane, carrying its members with it.
    ///
    /// Two statements in one IMMEDIATE transaction. `worktrees.lane` stores the
    /// name rather than a surrogate id precisely because both tables live in this
    /// one database, so the "denormalised name means a non-atomic N-row rewrite"
    /// objection does not apply here — and skipping the surrogate id is what keeps
    /// rowid reuse out of a brand-new table.
    pub fn rename_lane(&self, repo_root: &Path, from: &str, to: &str) -> Result<bool, DbError> {
        let to = Self::valid_lane_name(to)?;
        let root = root_key(repo_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Vec<String> = tx
            .prepare("SELECT name FROM lanes WHERE repo_root = ?1")?
            .query_map(params![root], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !existing.iter().any(|n| n == from) {
            return Ok(false);
        }
        // A pure case change (`review` → `Review`) is a legal rename of the same
        // lane, so exclude the source before testing for a collision.
        let folded = Self::lane_fold(to);
        if existing
            .iter()
            .any(|n| n != from && Self::lane_fold(n) == folded)
        {
            return Err(DbError::LaneTaken(to.to_owned()));
        }
        tx.execute(
            "UPDATE lanes SET name = ?1 WHERE repo_root = ?2 AND name = ?3",
            params![to, root, from],
        )?;
        tx.execute(
            "UPDATE worktrees SET lane = ?1 WHERE repo_root = ?2 AND lane = ?3",
            params![to, root, from],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Delete a lane and ungroup its members, in one transaction.
    ///
    /// Explicit rather than leaning on a foreign key: `worktrees.lane` is a plain
    /// name column with no FK to cascade from, which is the trade for having no
    /// surrogate id. Doing it here means the two stores can never disagree about
    /// whether a lane exists.
    pub fn delete_lane(&self, repo_root: &Path, name: &str) -> Result<bool, DbError> {
        let root = root_key(repo_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE worktrees SET lane = '' WHERE repo_root = ?1 AND lane = ?2",
            params![root, name],
        )?;
        let n = tx.execute(
            "DELETE FROM lanes WHERE repo_root = ?1 AND name = ?2",
            params![root, name],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Rewrite lane order from a full ordered list of names.
    ///
    /// Takes the whole list rather than a move-this-one delta so the write is
    /// idempotent and there is no partial-state arithmetic to get wrong: names the
    /// caller omits keep their relative order after the ones it listed. Unknown
    /// names are ignored.
    pub fn reorder_lanes(&self, repo_root: &Path, order: &[String]) -> Result<(), DbError> {
        if order.len() > MAX_ORDER_LEN {
            return Err(DbError::OrderTooLong(order.len()));
        }
        let root = root_key(repo_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut next = 0i64;
        for name in order {
            let n = tx.execute(
                "UPDATE lanes SET position = ?1 WHERE repo_root = ?2 AND name = ?3",
                params![next, root, name],
            )?;
            if n > 0 {
                next += 1;
            }
        }
        // Anything the caller did not mention lands after the listed lanes,
        // keeping its own relative order.
        let rest: Vec<String> = tx
            .prepare(
                "SELECT name FROM lanes WHERE repo_root = ?1 AND name NOT IN
                   (SELECT value FROM json_each(?2)) ORDER BY position, name COLLATE NOCASE",
            )?
            .query_map(
                params![root, serde_json::to_string(order).unwrap_or_default()],
                |r| r.get(0),
            )?
            .collect::<rusqlite::Result<_>>()?;
        for name in rest {
            tx.execute(
                "UPDATE lanes SET position = ?1 WHERE repo_root = ?2 AND name = ?3",
                params![next, root, name],
            )?;
            next += 1;
        }
        tx.commit()?;
        Ok(())
    }

    /// Rewrite manual worktree order from a full ordered list of **paths**.
    ///
    /// Keyed on `path` (which is `UNIQUE`), never on `worktrees.id`: rowids are
    /// reused, so an id-keyed order outlives the worktree and lands on the next
    /// one created — the bug three stores shipped in #201.
    ///
    /// Paths the caller omits are reset to `sort_position = NULL`, i.e. back to the
    /// alias-sorted tail. That is what makes the write idempotent: the UI sends the
    /// order it is displaying, and the stored order becomes exactly that.
    pub fn reorder_worktrees(&self, repo_root: &Path, order: &[String]) -> Result<(), DbError> {
        // Bounded before taking the write lock. The body is a caller-supplied array
        // and each element costs one UPDATE inside an IMMEDIATE transaction, so an
        // oversized list stalls every other writer in the daemon — run bookkeeping,
        // PTY logging, the GC pass — for as long as it takes. Nothing legitimate
        // sends more entries than the repo has worktrees.
        if order.len() > MAX_ORDER_LEN {
            return Err(DbError::OrderTooLong(order.len()));
        }
        let root = root_key(repo_root);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Trashed rows are exempt from the reset. The UI omits them from the order
        // it sends (they are leaving, so a position for them is meaningless), and
        // without this exemption any unrelated drag would strip the position off a
        // worktree whose removal is still pending — so a removal that then failed
        // would put it back unplaced. That is what makes the v10 migration's claim
        // that a trashed worktree keeps its lane AND its position true.
        tx.execute(
            "UPDATE worktrees SET sort_position = NULL
              WHERE repo_root = ?1 AND sort_position IS NOT NULL AND trashed_at = ''",
            params![root],
        )?;
        for (i, path) in order.iter().enumerate() {
            tx.execute(
                "UPDATE worktrees SET sort_position = ?1 WHERE repo_root = ?2 AND path = ?3",
                params![i as i64, root, path],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Worktree trash (§19)
    // -----------------------------------------------------------------------

    /// Environment names of every live run rooted at `project_root`.
    ///
    /// The background remover's stop step needs two things from this: which runs to
    /// ask `veld stop` about, and — by polling until it returns empty — whether
    /// teardown has actually finished. Runs became two-phase in #162 precisely so
    /// that the persisted status is the thing to observe rather than the exit of a
    /// spawned command, and `veld stop` is fire-and-forget from the daemon's side,
    /// so there is nothing else to wait on.
    pub fn live_run_names(&self, project_root: &Path) -> Result<Vec<String>, DbError> {
        let live = super::state::LIVE_SET;
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT e.name FROM runs r JOIN environments e ON e.id = r.environment_id
              WHERE e.project_root = ?1 AND r.status IN {live}
              ORDER BY e.name"
        ))?;
        let rows = stmt.query_map(params![root_key(project_root)], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Move a worktree to the trash. Returns the row as it now stands, or `None` if
    /// there is no such worktree.
    ///
    /// **Nothing is deleted here.** The checkout stays on disk for the whole
    /// retention period (`worktree.trashRetentionDays`); this only starts its clock.
    /// The actual `git worktree remove` happens when
    /// [`Self::expired_trashed_worktrees`] surfaces the row, or when the user asks
    /// for it immediately.
    ///
    /// Refuses the main checkout — removing it means removing the repo, which is a
    /// different operation with a different confirmation.
    ///
    /// Idempotent: trashing an already-trashed worktree just refreshes the
    /// timestamp and clears any previous error, which is exactly what a retry
    /// wants.
    pub fn trash_worktree(&self, id: i64) -> Result<Option<WorktreeRecord>, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let is_main: Option<bool> = tx
            .query_row(
                "SELECT is_main FROM worktrees WHERE id = ?1",
                params![id],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .optional()?;
        match is_main {
            None => return Ok(None),
            Some(true) => return Err(DbError::RefusingMainWorktree),
            Some(false) => {}
        }
        tx.execute(
            "UPDATE worktrees SET trashed_at = ?1, trash_error = '' WHERE id = ?2",
            params![now_str(), id],
        )?;
        let wt = tx
            .query_row(
                &format!("SELECT {WT_COLS} FROM worktrees WHERE id = ?1"),
                params![id],
                wt_from_row,
            )
            .optional()?;
        tx.commit()?;
        Ok(wt)
    }

    /// Take a worktree back out of trash, recording why its removal failed.
    ///
    /// The two halves are one write on purpose: a row that is out of trash but
    /// carries no reason looks like nothing happened, and a row that stays in
    /// trash with a reason looks like pending work forever. Pass an empty `reason`
    /// to restore a worktree at the user's request rather than after a failure.
    pub fn untrash_worktree(&self, id: i64, reason: &str) -> Result<bool, DbError> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE worktrees SET trashed_at = '', trash_error = ?1 WHERE id = ?2",
            params![reason, id],
        )?;
        Ok(n > 0)
    }

    /// Clear a worktree's recorded removal failure (the user has read it).
    pub fn clear_trash_error(&self, id: i64) -> Result<bool, DbError> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE worktrees SET trash_error = '' WHERE id = ?1 AND trash_error != ''",
            params![id],
        )?;
        Ok(n > 0)
    }

    /// Every trashed worktree across all repos, oldest request first.
    ///
    /// This is how the background remover finds its work at daemon boot: the row
    /// state *is* the durable queue, so a crash between "user clicked delete" and
    /// "git finished" needs no separate journal to reconcile. Re-running a removal
    /// that already succeeded is safe — git errors, the path is gone from disk, and
    /// the caller's `git worktree prune` fallback finishes the job.
    pub fn list_trashed_worktrees(&self) -> Result<Vec<WorktreeRecord>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {WT_COLS} FROM worktrees WHERE trashed_at != '' ORDER BY trashed_at"
        ))?;
        let rows = stmt.query_map([], wt_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Trashed worktrees whose retention period has expired, oldest request first.
    ///
    /// The GC pass hands these to the background remover. `retention_secs` comes from
    /// `worktree.trashRetentionDays`; a value of zero means "keep until I purge" and
    /// the caller must not call this at all in that case.
    ///
    /// Unbounded on purpose. Every row here is one the user explicitly asked to
    /// delete and then left alone for the whole retention period, so there is nothing
    /// to ration and no candidate worth skipping — unlike a scan for *inactivity*,
    /// which is a different feature and deliberately not this one.
    pub fn expired_trashed_worktrees(
        &self,
        retention_secs: i64,
    ) -> Result<Vec<WorktreeRecord>, DbError> {
        let cutoff =
            super::ts_to_str(chrono::Utc::now() - chrono::Duration::seconds(retention_secs));
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {WT_COLS} FROM worktrees
              WHERE trashed_at != '' AND trashed_at < ?1
              ORDER BY trashed_at"
        ))?;
        let rows = stmt.query_map(params![cutoff], wt_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// Default alias for a branch: the segment after the last `/`, lowercased,
/// non-alphanumerics collapsed to `-` (`feat/Checkout V2` → `checkout-v2`).
/// Falls back to `"wt"` when nothing usable remains (all-symbol input
/// like `///`; a detached checkout's `(detached)` label becomes `detached`).
pub fn default_alias(branch: &str) -> String {
    let last = branch.rsplit('/').next().unwrap_or(branch);
    let mut out = String::with_capacity(last.len());
    let mut prev_dash = true; // suppress a leading dash
    for c in last.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "wt".into() } else { out }
}

/// Curated animal set for worktree identifiers — memorable, visually
/// distinct at small sizes, and single-glyph (no multi-codepoint sequences).
pub const WORKTREE_EMOJI: &[&str] = &[
    "🦊", "🐻", "🐼", "🐨", "🐯", "🦁", "🐮", "🐷", "🐸", "🐵", "🐔", "🐧", "🐦", "🦆", "🦅", "🦉",
    "🦇", "🐺", "🐗", "🐴", "🦄", "🐝", "🐛", "🦋", "🐌", "🐞", "🐜", "🦗", "🦂", "🐢", "🐍", "🦎",
    "🦖", "🦕", "🐙", "🦑", "🦐", "🦞", "🦀", "🐡", "🐠", "🐟", "🐬", "🐳", "🐋", "🦈", "🐊", "🐅",
    "🐆", "🦓", "🦍", "🦧", "🐘", "🦛", "🦏", "🐪", "🐫", "🦒", "🦘", "🐃", "🐂", "🐄", "🐎", "🐐",
];

/// Whether `emoji` is one of the curated glyphs.
///
/// An allowlist rather than a "is this a single grapheme?" test: it keeps the
/// rail visually uniform and leaves no room for a multi-codepoint sequence or
/// a zero-width payload. Every entry is a single code point, so plain string
/// equality is sufficient — no normalization surface.
///
/// Entries may be **appended but never removed or replaced**: the value is
/// persisted per worktree, so dropping one orphans every row holding it (the
/// picker would show no current selection and the glyph could never be
/// re-picked).
pub fn is_worktree_emoji(emoji: &str) -> bool {
    WORKTREE_EMOJI.contains(&emoji)
}

/// Colour half of the worktree marker.
///
/// **Eight, and eight on purpose.** Hue is a narrow channel: fewer and genuinely
/// distinct beats more and merely different, because past about eight steps
/// neighbours stop being tellable apart at rail size — and the honest limit is lower
/// still under a colour vision deficiency (~1 in 12 men). Distinctness is only ever
/// needed *within* one repo — measured on a real database, two repos held 9 and 7
/// checkouts, so a repo can exceed eight and the ninth simply repeats a colour via
/// [`pick_color`]'s fallback. That is the intended trade: a duplicate in a nine-row
/// rail costs less than four more colours nobody can tell apart.
///
/// **Identical in both themes**, by decision: a marker is an identity, and an
/// identity that changes appearance when the theme is switched is doing its job
/// badly. Consequence, stated rather than discovered — the pale members sit
/// low-contrast on a white panel, so the swatch's ring is strengthened there while
/// the fill stays exact.
///
/// **Colour is never the only channel.** The alias renders beside the badge
/// everywhere the badge appears, so a swatch is a scanning aid over a text label,
/// not the identifier. That is also the honest answer for colour vision deficiency,
/// rather than bolting on a second encoding.
///
/// This is the set the *picker offers*. It is not the set of storable values:
/// [`is_worktree_color`] accepts any `#rrggbb`, so a custom colour needs no
/// migration — which is the whole reason the column holds a colour and not an index
/// into this list. Entries may be added, removed or reordered freely for the same
/// reason: nothing persists a position here.
pub const WORKTREE_COLORS: &[&str] = &[
    "#008cff", "#41fffc", "#7dff1a", "#9719ff", "#ff17e0", "#ff3502", "#ffa31a", "#fff827",
];

/// Whether `color` is a storable marker colour: a lowercase `#rrggbb`.
///
/// Deliberately **wider than [`WORKTREE_COLORS`]**. The picker offers the curated
/// set, but any valid hex is accepted so a custom colour is a UI addition rather
/// than a schema change. Narrow enough to be safe: the value is written into a CSS
/// colour position, and `#` plus exactly six hex digits cannot escape it — which is
/// the same reasoning that bounds `terminal.fontFamily`, arrived at the hard way.
///
/// Rejects uppercase rather than normalising, so two rows cannot hold the same
/// colour in two spellings and compare unequal.
pub fn is_worktree_color(color: &str) -> bool {
    color.len() == 7
        && color.starts_with('#')
        && color[1..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Pick an emoji for a new worktree: hash the seed into the curated list and probe
/// forward for one not used **by the repo's other checkouts**.
///
/// Scoped per repo, not globally. The global search this replaced meant 64 glyphs
/// had to cover every worktree of every repo on the machine, so a developer with a
/// handful of repos exhausted the set and fell through to the duplicate path —
/// while the only place two markers are ever compared is one repo's rail, which
/// shows one repo at a time. Per-repo also makes the probe's cost proportional to
/// the checkouts of one repo instead of the whole table.
///
/// When a single repo really does hold more than 64 checkouts, fall back to the
/// hash slot: duplicates beat failing.
fn pick_emoji(
    conn: &rusqlite::Connection,
    repo_root: &str,
    seed: &str,
) -> rusqlite::Result<String> {
    let mut used = std::collections::HashSet::new();
    let mut stmt =
        conn.prepare_cached("SELECT emoji FROM worktrees WHERE repo_root = ?1 AND emoji != ''")?;
    let rows = stmt.query_map(params![repo_root], |r| r.get::<_, String>(0))?;
    for row in rows {
        used.insert(row?);
    }
    let h = marker_seed_hash(seed);
    for i in 0..WORKTREE_EMOJI.len() {
        let e = WORKTREE_EMOJI[(h + i) % WORKTREE_EMOJI.len()];
        if !used.contains(e) {
            return Ok(e.to_string());
        }
    }
    Ok(WORKTREE_EMOJI[h % WORKTREE_EMOJI.len()].to_string())
}

/// Pick a marker colour, by the same probe as [`pick_emoji`] and for the same
/// reasons — scoped to the repo, since the rail shows one repo at a time and eight
/// colours could not cover a machine.
///
/// The seed is offset from the emoji's so a worktree does not get colour *n* purely
/// because it got glyph *n*: the two faces are independent choices, and a user who
/// re-picks one should not find the other implied by it.
fn pick_color(
    conn: &rusqlite::Connection,
    repo_root: &str,
    seed: &str,
) -> rusqlite::Result<String> {
    let mut used = std::collections::HashSet::new();
    let mut stmt = conn.prepare_cached(
        "SELECT marker_color FROM worktrees WHERE repo_root = ?1 AND marker_color != ''",
    )?;
    let rows = stmt.query_map(params![repo_root], |r| r.get::<_, String>(0))?;
    for row in rows {
        used.insert(row?);
    }
    let h = marker_seed_hash(seed).wrapping_add(WORKTREE_COLORS.len() / 2);
    for i in 0..WORKTREE_COLORS.len() {
        let c = WORKTREE_COLORS[(h + i) % WORKTREE_COLORS.len()];
        if !used.contains(c) {
            return Ok(c.to_string());
        }
    }
    Ok(WORKTREE_COLORS[h % WORKTREE_COLORS.len()].to_string())
}

/// Shared hash for both marker channels. Kept as one function so the two probes
/// cannot drift into disagreeing about what "the same seed" means.
fn marker_seed_hash(seed: &str) -> usize {
    seed.bytes()
        .fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize))
}

/// Longest slug a numbered alias candidate may start from. `slugify` caps output
/// at 48 characters, so leaving 8 spare keeps `-2` … `-9999999` visible in the
/// slug — which is what makes [`unique_alias`]'s search terminate.
const SUFFIXABLE_SLUG_LEN: usize = 40;

/// Make `base` unique among a repo's aliases by appending `-2`, `-3`, … as
/// needed. Runs inside the sync transaction.
fn unique_alias(
    conn: &rusqlite::Connection,
    repo_root: &str,
    base: &str,
) -> rusqlite::Result<String> {
    // Slug comparison, matching `Db::patch_worktree` — the two must agree on
    // what "taken" means, or insert and rename disagree. Fetched once: the
    // candidate loop below would otherwise re-query per attempt, and the
    // sibling set cannot change inside the caller's transaction.
    let sibling_slugs: Vec<String> = conn
        .prepare("SELECT alias FROM worktrees WHERE repo_root = ?1")?
        .query_map(params![repo_root], |r| r.get::<_, String>(0))?
        .map(|a| a.map(|a| crate::url::slugify(&a)))
        .collect::<rusqlite::Result<_>>()?;
    let taken = |alias: &str| -> bool { sibling_slugs.contains(&crate::url::slugify(alias)) };
    if !taken(base) {
        return Ok(base.to_string());
    }
    // The numbered candidates must have DISTINCT slugs, or this loop cannot
    // finish: `slugify` truncates at 48 characters, so appending `-2` to a base
    // that already slugs to 48 produces the same slug forever — and this runs
    // inside the caller's IMMEDIATE transaction, so spinning here would hold the
    // SQLite write lock and wedge the daemon. Shorten the stem until the suffix
    // survives truncation. Bases short enough to be unaffected (the normal case)
    // are left exactly as they were.
    let mut stem = base.to_string();
    while crate::url::slugify(&stem).len() > SUFFIXABLE_SLUG_LEN {
        stem.pop();
    }
    for i in 2.. {
        let candidate = format!("{stem}-{i}");
        if !taken(&candidate) {
            return Ok(candidate);
        }
    }
    unreachable!("alias suffix search is unbounded")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    fn wt(path: &str, branch: &str, is_main: bool) -> DiscoveredWorktree {
        DiscoveredWorktree {
            path: path.into(),
            branch: branch.into(),
            is_main,
        }
    }

    #[test]
    fn repo_upsert_list_remove() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoA");
        db.upsert_repo(root, "repo-a").unwrap();
        db.upsert_repo(root, "repo-a-renamed").unwrap(); // idempotent, renames

        let repos = db.list_repos().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "repo-a-renamed");
        let created = repos[0].created_at.clone();

        // Re-upsert keeps created_at.
        db.upsert_repo(root, "x").unwrap();
        assert_eq!(db.get_repo(root).unwrap().unwrap().created_at, created);

        assert!(db.remove_repo(root).unwrap());
        assert!(!db.remove_repo(root).unwrap());
        assert!(db.list_repos().unwrap().is_empty());
    }

    #[test]
    fn sync_inserts_updates_and_prunes() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoB");
        db.upsert_repo(root, "repo-b").unwrap();

        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/repoB", "main", true),
                    wt("/tmp/wts/chk", "feat/checkout-v2", false),
                ],
            )
            .unwrap();
        assert_eq!(wts.len(), 2);
        assert!(wts[0].is_main, "main checkout sorts first");
        assert_eq!(wts[1].alias, "checkout-v2");

        // User renames, then a re-sync must preserve the alias and update the
        // branch; the vanished path is pruned.
        let id = wts[1].id;
        assert!(db.rename_worktree(id, "chk").unwrap());
        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/wts/chk", "feat/checkout-v3", false),
                    wt("/tmp/wts/auth", "fix/auth", false),
                ],
            )
            .unwrap();
        assert_eq!(wts.len(), 2, "main checkout row pruned (not rediscovered)");
        let chk = wts.iter().find(|w| w.path == "/tmp/wts/chk").unwrap();
        assert_eq!(chk.alias, "chk");
        assert_eq!(chk.branch, "feat/checkout-v3");
        assert_eq!(chk.id, id);
    }

    #[test]
    fn sync_deduplicates_aliases() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoC");
        db.upsert_repo(root, "repo-c").unwrap();
        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/wts/a", "feat/login", false),
                    wt("/tmp/wts/b", "fix/login", false),
                ],
            )
            .unwrap();
        let mut aliases: Vec<_> = wts.iter().map(|w| w.alias.as_str()).collect();
        aliases.sort();
        assert_eq!(aliases, vec!["login", "login-2"]);
    }

    #[test]
    fn rename_cannot_break_alias_uniqueness_within_a_repo() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoAliasDup");
        db.upsert_repo(root, "dup").unwrap();
        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/repoAliasDup", "main", true),
                    wt("/tmp/wts/dup-chk", "feat/checkout", false),
                ],
            )
            .unwrap();
        let main = wts.iter().find(|w| w.is_main).unwrap();
        let other = wts.iter().find(|w| !w.is_main).unwrap();

        // `unique_alias` establishes the invariant at insert; the rename path
        // must not be a hole in it — before this check the UPDATE went through
        // and left two checkouts of one repo answering to "main", which is two
        // environments defaulting to the same run name.
        let e = db.rename_worktree(other.id, &main.alias).unwrap_err();
        assert!(
            matches!(e, DbError::AliasTaken(ref a) if *a == main.alias),
            "expected AliasTaken, got {e:?}"
        );
        // Rejected before the write: neither column moved.
        assert_eq!(
            db.get_worktree(other.id).unwrap().unwrap().alias,
            other.alias
        );

        // Renaming to the alias it already holds is a no-op, not a collision
        // with itself.
        assert!(db.rename_worktree(other.id, &other.alias).unwrap());

        // A colliding alias also blocks the emoji half of the same patch —
        // "a good emoji and a bad alias must change neither".
        let glyph = WORKTREE_EMOJI
            .iter()
            .find(|g| **g != other.emoji)
            .expect("curated set has more than one glyph");
        assert!(
            db.patch_worktree(other.id, Some(&main.alias), Some(glyph), None, None)
                .is_err()
        );
        assert_eq!(
            db.get_worktree(other.id).unwrap().unwrap().emoji,
            other.emoji
        );

        // A missing row still reports "not found" rather than a collision.
        assert!(!db.rename_worktree(9_999, "whatever").unwrap());
    }

    #[test]
    fn alias_uniqueness_ignores_case_because_hostnames_do() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoAliasCase");
        db.upsert_repo(root, "case").unwrap();
        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/repoAliasCase", "main", true),
                    wt("/tmp/wts/case-other", "feat/other", false),
                ],
            )
            .unwrap();
        let other = wts.iter().find(|w| !w.is_main).unwrap();

        // `is_safe_identifier` allows A-Z, so "Main" is a legal alias — but it
        // would mint a hostname differing from "main"'s only in case, and DNS
        // and Caddy host matching are case-insensitive. A case-sensitive check
        // would pass here and the collision would still happen.
        let e = db.rename_worktree(other.id, "MAIN").unwrap_err();
        assert!(matches!(e, DbError::AliasTaken(_)), "got {e:?}");
        assert!(db.rename_worktree(other.id, "main-2").unwrap());
    }

    #[test]
    fn alias_uniqueness_compares_slugs_not_separators() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoAliasSlug");
        db.upsert_repo(root, "slug").unwrap();
        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/repoAliasSlug", "main-2", true),
                    wt("/tmp/wts/slug-other", "feat/other", false),
                ],
            )
            .unwrap();
        let other = wts.iter().find(|w| !w.is_main).unwrap();
        assert_eq!(
            wts.iter().find(|w| w.is_main).unwrap().alias,
            "main-2",
            "precondition: the main checkout holds `main-2`"
        );

        // `is_safe_identifier` allows `-`, `_` and `.`, and `slugify` maps all
        // three to `-`: these mint the SAME hostname as `main-2` (#172).
        for taken in ["main_2", "main.2", "MAIN_2", "main-2"] {
            let e = db.rename_worktree(other.id, taken).unwrap_err();
            assert!(
                matches!(e, DbError::AliasTaken(_)),
                "{taken} should collide with main-2, got {e:?}"
            );
        }
        // A genuinely different slug is still free.
        assert!(db.rename_worktree(other.id, "main_3").unwrap());
    }

    #[test]
    fn generated_aliases_skip_slug_variants_too() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoAliasGen");
        db.upsert_repo(root, "gen").unwrap();
        // Set up a repo holding `main` and `main_2`: the main checkout keeps
        // `main`, a sibling is renamed to `main_2` (legal, and it mints the
        // hostname `main-2`).
        let seeded = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/repoAliasGen", "main", true),
                    wt("/tmp/wts/gen-a", "feat/a", false),
                ],
            )
            .unwrap();
        let a = seeded.iter().find(|w| !w.is_main).unwrap();
        db.rename_worktree(a.id, "main_2").unwrap();

        // A third checkout on `main` derives the alias `main`: taken. `main-2`
        // is the next candidate, and it must be skipped too — `main_2` already
        // mints that hostname.
        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/repoAliasGen", "main", true),
                    wt("/tmp/wts/gen-a", "feat/a", false),
                    wt("/tmp/wts/gen-b", "main", false),
                ],
            )
            .unwrap();
        let generated = wts
            .iter()
            .find(|w| w.path == "/tmp/wts/gen-b")
            .expect("third checkout");
        assert_eq!(generated.alias, "main-3", "must skip the `main_2` slug");
    }

    #[test]
    fn long_branch_names_still_produce_a_unique_alias() {
        // Regression: comparing SLUGS means `{base}-2` is not automatically a new
        // name — `slugify` truncates at 48 chars, so for a base that already
        // slugs to 48 every numbered candidate slugs identically. The suffix
        // search used to spin forever on that, inside the IMMEDIATE transaction.
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoAliasLong");
        db.upsert_repo(root, "long").unwrap();
        let long = "feature/".to_owned() + &"a".repeat(54);

        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/repoAliasLong", &long, true),
                    wt("/tmp/wts/long-b", &long, false),
                    wt("/tmp/wts/long-c", &long, false),
                ],
            )
            .unwrap();

        let slugs: std::collections::HashSet<String> =
            wts.iter().map(|w| crate::url::slugify(&w.alias)).collect();
        assert_eq!(
            slugs.len(),
            3,
            "each checkout needs its own hostname: {slugs:?}"
        );
        // Only the *suffixed* candidates are bounded by `SUFFIXABLE_SLUG_LEN`.
        // The first holder keeps `base` verbatim, and `default_alias` has no
        // length cap of its own — an uncapped default is pre-existing and tracked
        // separately, so assert what this change is responsible for.
        let base = default_alias(&long);
        for w in wts.iter().filter(|w| w.alias != base) {
            assert!(
                w.alias.len() <= 64,
                "a generated alias must stay a legal identifier: {}",
                w.alias
            );
        }
    }

    #[test]
    fn alias_uniqueness_is_scoped_to_one_repo() {
        let (_dir, db) = test_db();
        let a = Path::new("/tmp/repoAliasScopeA");
        let b = Path::new("/tmp/repoAliasScopeB");
        db.upsert_repo(a, "a").unwrap();
        db.upsert_repo(b, "b").unwrap();
        let wa = db
            .sync_worktrees(a, &[wt(a.to_str().unwrap(), "main", true)])
            .unwrap();
        let wb = db
            .sync_worktrees(b, &[wt(b.to_str().unwrap(), "release", true)])
            .unwrap();
        assert_eq!(wa[0].alias, "main");

        // Cross-repo duplicates stay legal — that is exactly the state the
        // run-addressing fix handles, and forbidding it would make importing
        // two repos both on `main` fail.
        assert!(db.rename_worktree(wb[0].id, "main").unwrap());
        assert_eq!(db.get_worktree(wb[0].id).unwrap().unwrap().alias, "main");
    }

    #[test]
    fn sync_with_empty_list_is_a_noop_not_a_wipe() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoE");
        db.upsert_repo(root, "repo-e").unwrap();
        db.sync_worktrees(root, &[wt("/tmp/repoE", "main", true)])
            .unwrap();
        let wts = db.sync_worktrees(root, &[]).unwrap();
        assert_eq!(wts.len(), 1, "empty discovery must not delete rows");
    }

    #[test]
    fn every_offered_colour_is_a_storable_one() {
        // The palette the picker offers must pass the validator the API enforces,
        // or a curated swatch would 400 on click. This replaces the Rust↔CSS drift
        // gate the index design needed: the stylesheet no longer knows the palette.
        for c in WORKTREE_COLORS {
            assert!(is_worktree_color(c), "{c} is not storable");
        }
        // Distinct, or two entries would be one choice wearing two positions.
        let unique: std::collections::HashSet<_> = WORKTREE_COLORS.iter().collect();
        assert_eq!(unique.len(), WORKTREE_COLORS.len());
    }

    #[test]
    fn a_custom_colour_is_storable_without_a_migration() {
        // The point of holding a colour rather than an index: the picker offers a
        // curated set, and a colour outside it is still a legal value, so allowing
        // custom colours later is a UI change and not a schema change.
        assert!(is_worktree_color("#123abc"));
        assert!(
            !is_worktree_color("#123ABC"),
            "uppercase must not be a second spelling"
        );
        for bad in [
            "", "#12345", "#1234567", "123abc", "#12345g", "red", "#12 abc",
        ] {
            assert!(!is_worktree_color(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn markers_are_assigned_and_distinct_within_a_repo() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoMarkerA");
        db.upsert_repo(root, "a").unwrap();
        // Three checkouts of ONE repo: this is the set that is ever compared,
        // because the rail shows one repo at a time.
        let wts = db
            .sync_worktrees(
                root,
                &[
                    wt("/tmp/repoMarkerA", "main", true),
                    wt("/tmp/repoMarkerA-two", "feat/two", false),
                    wt("/tmp/repoMarkerA-three", "feat/three", false),
                ],
            )
            .unwrap();
        assert_eq!(wts.len(), 3);
        let glyphs: std::collections::HashSet<_> = wts.iter().map(|w| &w.emoji).collect();
        let colors: std::collections::HashSet<_> =
            wts.iter().map(|w| w.marker_color.clone()).collect();
        assert_eq!(glyphs.len(), 3, "glyphs distinct among a repo's checkouts");
        assert_eq!(colors.len(), 3, "colours distinct among a repo's checkouts");
        assert!(wts.iter().all(|w| is_worktree_color(&w.marker_color)));

        // Rename must not change either face — the marker is a stable
        // identifier, and both halves of it are.
        let before = &wts[0];
        db.rename_worktree(before.id, "renamed").unwrap();
        let after = db.get_worktree(before.id).unwrap().unwrap();
        assert_eq!(after.emoji, before.emoji);
        assert_eq!(after.marker_color, before.marker_color);
    }

    #[test]
    fn markers_may_repeat_across_repos() {
        // Deliberate, and a change from the original global probe: 64 glyphs (and
        // eight colours) cannot cover every worktree of every repo on a machine, so a
        // global search exhausted the set and fell through to duplicates anyway.
        // Two markers are only ever compared within one repo's rail, so that is
        // where distinctness is bought — and re-using a glyph in a *different*
        // repo costs nothing a user can see.
        let (_dir, db) = test_db();
        let a = Path::new("/tmp/repoMarkerX");
        let b = Path::new("/tmp/repoMarkerY");
        db.upsert_repo(a, "x").unwrap();
        db.upsert_repo(b, "y").unwrap();
        // Same branch name in both, so the same hash seed: with a per-repo probe
        // and no sibling to avoid, both land on the same slot.
        let wa = db
            .sync_worktrees(a, &[wt("/tmp/repoMarkerX", "main", true)])
            .unwrap();
        let wb = db
            .sync_worktrees(b, &[wt("/tmp/repoMarkerY", "main", true)])
            .unwrap();
        assert!(!wa[0].emoji.is_empty());
        assert!(!wb[0].emoji.is_empty());
        assert_eq!(wa[0].emoji, wb[0].emoji);
        assert_eq!(wa[0].marker_color, wb[0].marker_color);
    }

    #[test]
    fn a_marker_colour_is_backfilled_for_a_pre_v9_row() {
        // The upgrade path: a row that came through v6 has a glyph and `-1` for
        // its colour. The next sync must fill it without disturbing the
        // glyph the user may have chosen.
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoMarkerBackfill");
        db.upsert_repo(root, "bf").unwrap();
        let wts = db
            .sync_worktrees(root, &[wt("/tmp/repoMarkerBackfill", "main", true)])
            .unwrap();
        let id = wts[0].id;
        let glyph = wts[0].emoji.clone();
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE worktrees SET marker_color = '' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        let after = db
            .sync_worktrees(root, &[wt("/tmp/repoMarkerBackfill", "main", true)])
            .unwrap();
        assert!(
            is_worktree_color(&after[0].marker_color),
            "colour backfilled"
        );
        assert_eq!(
            after[0].emoji, glyph,
            "glyph untouched by the colour backfill"
        );
    }

    #[test]
    fn an_explicit_colour_survives_sync_and_is_validated() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoMarkerPick");
        db.upsert_repo(root, "pick").unwrap();
        let wts = db
            .sync_worktrees(root, &[wt("/tmp/repoMarkerPick", "main", true)])
            .unwrap();
        let id = wts[0].id;
        let chosen = WORKTREE_COLORS
            .iter()
            .find(|c| **c != wts[0].marker_color)
            .expect("palette has more than one colour");
        assert!(
            db.patch_worktree(id, None, None, Some(chosen), None)
                .unwrap()
        );
        // A user's explicit colour must not be clobbered by the UI's refresh poll.
        db.sync_worktrees(root, &[wt("/tmp/repoMarkerPick", "main", true)])
            .unwrap();
        assert_eq!(&db.get_worktree(id).unwrap().unwrap().marker_color, chosen);

        for bad in ["", "#12345", "nope"] {
            assert!(
                matches!(
                    db.patch_worktree(id, None, None, Some(bad), None),
                    Err(DbError::InvalidColor(_))
                ),
                "{bad} must be rejected"
            );
        }
        // A rejected colour must not commit the alias that travelled with it.
        assert!(
            db.patch_worktree(id, Some("nope"), None, Some("#12345"), None)
                .is_err()
        );
        assert_ne!(db.get_worktree(id).unwrap().unwrap().alias, "nope");
    }

    #[test]
    fn explicit_emoji_survives_sync_and_rename() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoEmojiSet");
        db.upsert_repo(root, "set").unwrap();
        let wts = db
            .sync_worktrees(root, &[wt("/tmp/repoEmojiSet", "main", true)])
            .unwrap();
        let id = wts[0].id;

        // Pick a glyph the assigner did not hand out, so the assertion can't
        // pass by coincidence.
        let chosen = WORKTREE_EMOJI
            .iter()
            .find(|e| **e != wts[0].emoji)
            .expect("curated set has more than one glyph");
        assert!(
            db.patch_worktree(id, None, Some(chosen), None, None)
                .unwrap()
        );
        assert_eq!(&db.get_worktree(id).unwrap().unwrap().emoji, chosen);

        // Reconciliation backfills only empty emoji — an explicit choice must
        // not be clobbered by the UI's 5s refresh poll.
        db.sync_worktrees(root, &[wt("/tmp/repoEmojiSet", "main", true)])
            .unwrap();
        assert_eq!(&db.get_worktree(id).unwrap().unwrap().emoji, chosen);

        db.rename_worktree(id, "renamed").unwrap();
        assert_eq!(&db.get_worktree(id).unwrap().unwrap().emoji, chosen);
    }

    #[test]
    fn patch_rejects_glyphs_outside_the_curated_set() {
        // The EMOJI rule lives here, not only in the HTTP handler, so a CLI
        // or IPC caller can't persist an arbitrary glyph. Note the asymmetry:
        // `alias` is still validated only by the handler (`validate_alias`).
        let (_dir, db) = test_db();
        for bad in ["", "🍕", "🦊🦊", "👨‍👩‍👧", "not-an-emoji"] {
            assert!(
                matches!(
                    db.patch_worktree(1, None, Some(bad), None, None),
                    Err(DbError::InvalidEmoji(_))
                ),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn curated_emoji_set_has_no_duplicates() {
        // `pick_emoji`'s uniqueness probe and the picker's React `key` both
        // assume this; a duplicate would be a silent runtime defect.
        let unique: std::collections::HashSet<_> = WORKTREE_EMOJI.iter().collect();
        assert_eq!(unique.len(), WORKTREE_EMOJI.len());
    }

    #[test]
    fn patch_worktree_applies_fields_independently_and_atomically() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoPatch");
        db.upsert_repo(root, "patch").unwrap();
        let id = db
            .sync_worktrees(root, &[wt("/tmp/repoPatch", "main", true)])
            .unwrap()[0]
            .id;
        let original = db.get_worktree(id).unwrap().unwrap();
        let glyph = WORKTREE_EMOJI
            .iter()
            .find(|e| **e != original.emoji)
            .unwrap();

        // None leaves a column untouched.
        assert!(
            db.patch_worktree(id, Some("renamed"), None, None, None)
                .unwrap()
        );
        let after = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(after.alias, "renamed");
        assert_eq!(after.emoji, original.emoji);

        assert!(
            db.patch_worktree(id, None, Some(glyph), None, None)
                .unwrap()
        );
        let after = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(after.alias, "renamed");
        assert_eq!(&after.emoji, glyph);

        // Both at once.
        assert!(
            db.patch_worktree(id, Some("both"), Some(&original.emoji), None, None)
                .unwrap()
        );
        let after = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(after.alias, "both");
        assert_eq!(after.emoji, original.emoji);

        // A rejected emoji must not commit the alias that travelled with it.
        assert!(
            db.patch_worktree(id, Some("nope"), Some("🍕"), None, None)
                .is_err()
        );
        assert_eq!(db.get_worktree(id).unwrap().unwrap().alias, "both");

        // Neither field: a no-op write that still reports row existence.
        assert!(db.patch_worktree(id, None, None, None, None).unwrap());
        let after = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(after.alias, "both");
        assert_eq!(after.emoji, original.emoji);

        assert!(
            !db.patch_worktree(4242, Some("x"), None, None, None)
                .unwrap()
        );
        assert!(!db.patch_worktree(4242, None, None, None, None).unwrap());
    }

    #[test]
    fn worktrees_cascade_delete_with_repo() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoD");
        db.upsert_repo(root, "repo-d").unwrap();
        db.sync_worktrees(root, &[wt("/tmp/repoD", "main", true)])
            .unwrap();
        db.remove_repo(root).unwrap();
        let n: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM worktrees", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn default_alias_shapes() {
        assert_eq!(default_alias("main"), "main");
        assert_eq!(default_alias("feat/Checkout V2"), "checkout-v2");
        assert_eq!(default_alias("fix/auth-retry"), "auth-retry");
        assert_eq!(default_alias("///"), "wt");
        assert_eq!(default_alias("(detached)"), "detached");
    }

    // -----------------------------------------------------------------------
    // Lanes and manual order (§18)
    // -----------------------------------------------------------------------

    #[test]
    fn lane_names_are_trimmed_bounded_and_case_insensitively_unique() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/lanes");
        db.upsert_repo(root, "lanes").unwrap();

        let lane = db.create_lane(root, "  review  ").unwrap();
        assert_eq!(lane.name, "review", "the name is stored trimmed");
        assert_eq!(lane.position, 0);

        // Case-insensitive, because two rail headers differing only in case is a
        // mistake every time.
        assert!(matches!(
            db.create_lane(root, "Review"),
            Err(DbError::LaneTaken(_))
        ));
        assert!(matches!(
            db.create_lane(root, ""),
            Err(DbError::InvalidLaneName(_))
        ));
        assert!(matches!(
            db.create_lane(root, &"x".repeat(MAX_LANE_NAME_LEN + 1)),
            Err(DbError::InvalidLaneName(_))
        ));
        // Control characters are rejected rather than stripped: the name renders as
        // a rail header and is the client's group key, where the UI reserves a
        // NUL-prefixed sentinel for pending removals.
        for bad in ["\u{0}trash", "two\nlines", "tab\there"] {
            assert!(
                matches!(db.create_lane(root, bad), Err(DbError::InvalidLaneName(_))),
                "expected {bad:?} to be rejected"
            );
        }

        assert_eq!(db.create_lane(root, "spikes").unwrap().position, 1);
    }

    #[test]
    fn renaming_a_lane_carries_its_members() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/renlane");
        db.upsert_repo(root, "renlane").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/renlane", "main", true),
                wt("/tmp/wt-a", "a", false),
            ],
        )
        .unwrap();
        db.create_lane(root, "review").unwrap();
        let id = db.get_worktree_by_path("/tmp/wt-a").unwrap().unwrap().id;
        db.patch_worktree(id, None, None, None, Some("review"))
            .unwrap();

        assert!(db.rename_lane(root, "review", "in review").unwrap());
        // The whole point of storing the NAME on the row: the rename has to carry
        // membership, and it does so in one transaction.
        assert_eq!(db.get_worktree(id).unwrap().unwrap().lane, "in review");
        assert_eq!(db.list_lanes(root).unwrap()[0].name, "in review");

        // A pure case change is a legal rename of the same lane, not a collision.
        assert!(db.rename_lane(root, "in review", "In Review").unwrap());
        assert_eq!(db.get_worktree(id).unwrap().unwrap().lane, "In Review");
        assert!(!db.rename_lane(root, "nope", "x").unwrap());
    }

    #[test]
    fn deleting_a_lane_ungroups_its_members() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/dellane");
        db.upsert_repo(root, "dellane").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/dellane", "main", true),
                wt("/tmp/wt-a", "a", false),
            ],
        )
        .unwrap();
        db.create_lane(root, "spikes").unwrap();
        let id = db.get_worktree_by_path("/tmp/wt-a").unwrap().unwrap().id;
        db.patch_worktree(id, None, None, None, Some("spikes"))
            .unwrap();

        assert!(db.delete_lane(root, "spikes").unwrap());
        // No FK to cascade from — `delete_lane` clears the assignments itself, in
        // the same transaction, so the two can never disagree.
        assert_eq!(db.get_worktree(id).unwrap().unwrap().lane, "");
        assert!(!db.delete_lane(root, "spikes").unwrap());
    }

    #[test]
    fn a_lane_assignment_must_name_a_lane_of_this_repo() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/badlane");
        db.upsert_repo(root, "badlane").unwrap();
        db.sync_worktrees(root, &[wt("/tmp/badlane", "main", true)])
            .unwrap();
        let id = db.get_worktree_by_path("/tmp/badlane").unwrap().unwrap().id;

        assert!(matches!(
            db.patch_worktree(id, None, None, None, Some("ghost")),
            Err(DbError::UnknownLane(_))
        ));
        // Ungrouping is always legal.
        assert!(db.patch_worktree(id, None, None, None, Some("")).unwrap());
    }

    #[test]
    fn rail_order_is_ungrouped_first_then_lanes_then_manual_position() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/order");
        db.upsert_repo(root, "order").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/order", "main", true),
                wt("/tmp/zeta", "zeta", false),
                wt("/tmp/alpha", "alpha", false),
                wt("/tmp/beta", "beta", false),
            ],
        )
        .unwrap();

        // With no lanes and nothing placed, the order is v9's exactly.
        let aliases: Vec<String> = db
            .list_worktrees(root)
            .unwrap()
            .into_iter()
            .map(|w| w.alias)
            .collect();
        assert_eq!(aliases, vec!["main", "alpha", "beta", "zeta"]);

        // A manual order puts placed worktrees first, unplaced alias-sorted after.
        db.reorder_worktrees(root, &["/tmp/zeta".into(), "/tmp/beta".into()])
            .unwrap();
        let aliases: Vec<String> = db
            .list_worktrees(root)
            .unwrap()
            .into_iter()
            .map(|w| w.alias)
            .collect();
        assert_eq!(aliases, vec!["main", "zeta", "beta", "alpha"]);

        // A lane moves its members below every ungrouped worktree.
        db.create_lane(root, "review").unwrap();
        let zeta = db.get_worktree_by_path("/tmp/zeta").unwrap().unwrap().id;
        db.patch_worktree(zeta, None, None, None, Some("review"))
            .unwrap();
        let rows = db.list_worktrees(root).unwrap();
        let aliases: Vec<&str> = rows.iter().map(|w| w.alias.as_str()).collect();
        assert_eq!(aliases, vec!["main", "beta", "alpha", "zeta"]);
        assert_eq!(rows.last().unwrap().lane, "review");
    }

    #[test]
    fn manual_order_is_keyed_on_path_so_a_reused_rowid_inherits_nothing() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/reuse");
        db.upsert_repo(root, "reuse").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/reuse", "main", true),
                wt("/tmp/gone", "gone", false),
            ],
        )
        .unwrap();
        let old = db.get_worktree_by_path("/tmp/gone").unwrap().unwrap();
        db.reorder_worktrees(root, &["/tmp/gone".into()]).unwrap();
        assert_eq!(
            db.get_worktree(old.id).unwrap().unwrap().sort_position,
            Some(0)
        );

        // The worktree vanishes and a different one takes its place. SQLite reuses
        // rowids, so this is the #201 hazard — an id-keyed position would be
        // inherited here.
        db.sync_worktrees(root, &[wt("/tmp/reuse", "main", true)])
            .unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/reuse", "main", true),
                wt("/tmp/fresh", "fresh", false),
            ],
        )
        .unwrap();
        let fresh = db.get_worktree_by_path("/tmp/fresh").unwrap().unwrap();
        assert_eq!(
            fresh.sort_position, None,
            "a newly discovered worktree is unplaced, whatever rowid it landed on"
        );
        assert_eq!(fresh.lane, "");
    }

    #[test]
    fn reorder_omissions_go_back_to_unplaced() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/omit");
        db.upsert_repo(root, "omit").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/omit", "main", true),
                wt("/tmp/a", "a", false),
                wt("/tmp/b", "b", false),
            ],
        )
        .unwrap();
        db.reorder_worktrees(root, &["/tmp/a".into(), "/tmp/b".into()])
            .unwrap();
        // Sending a shorter list is how the UI expresses "b is no longer placed";
        // the write is the whole order, so it must not leave b's old position.
        db.reorder_worktrees(root, &["/tmp/a".into()]).unwrap();
        assert_eq!(
            db.get_worktree_by_path("/tmp/b")
                .unwrap()
                .unwrap()
                .sort_position,
            None
        );
    }

    #[test]
    fn reorder_lanes_appends_unmentioned_lanes() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/lorder");
        db.upsert_repo(root, "lorder").unwrap();
        for name in ["a", "b", "c"] {
            db.create_lane(root, name).unwrap();
        }
        db.reorder_lanes(root, &["c".into(), "a".into()]).unwrap();
        let names: Vec<String> = db
            .list_lanes(root)
            .unwrap()
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    // -----------------------------------------------------------------------
    // Trash (§19)
    // -----------------------------------------------------------------------

    #[test]
    fn sync_does_not_resurrect_a_trashed_worktree() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/trash");
        db.upsert_repo(root, "trash").unwrap();
        let discovered = [
            wt("/tmp/trash", "main", true),
            wt("/tmp/doomed", "doomed", false),
        ];
        db.sync_worktrees(root, &discovered).unwrap();
        let id = db.get_worktree_by_path("/tmp/doomed").unwrap().unwrap().id;
        assert!(db.trash_worktree(id).unwrap().is_some());

        // The checkout still exists on disk until `git worktree remove` runs, so
        // git still reports it — this is the pass that used to resurrect the row it
        // was just asked to delete.
        db.sync_worktrees(root, &discovered).unwrap();
        let row = db.get_worktree(id).unwrap().unwrap();
        assert!(!row.trashed_at.is_empty(), "still trashed after a sync");

        // Once removal succeeds the path leaves git's list and the existing prune
        // branch reaps the row. No third outcome needed for the success case.
        db.sync_worktrees(root, &[wt("/tmp/trash", "main", true)])
            .unwrap();
        assert!(db.get_worktree(id).unwrap().is_none());
    }

    #[test]
    fn a_trashed_worktree_still_tracks_its_branch() {
        // The checkout stays on disk for the whole retention period, which defaults
        // to "until emptied" — so skipping the update for trashed rows meant a branch
        // switch made inside one was invisible in the rail indefinitely.
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/track");
        db.upsert_repo(root, "track").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/track", "main", true),
                wt("/tmp/wt", "before", false),
            ],
        )
        .unwrap();
        let id = db.get_worktree_by_path("/tmp/wt").unwrap().unwrap().id;
        db.trash_worktree(id).unwrap();

        db.sync_worktrees(
            root,
            &[
                wt("/tmp/track", "main", true),
                wt("/tmp/wt", "after", false),
            ],
        )
        .unwrap();
        let row = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(row.branch, "after", "a trashed row still follows git");
        assert!(
            !row.trashed_at.is_empty(),
            "and updating it must not take it out of the trash"
        );
    }

    #[test]
    fn trashing_refuses_the_main_checkout() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/mainwt");
        db.upsert_repo(root, "mainwt").unwrap();
        db.sync_worktrees(root, &[wt("/tmp/mainwt", "main", true)])
            .unwrap();
        let id = db.get_worktree_by_path("/tmp/mainwt").unwrap().unwrap().id;
        assert!(matches!(
            db.trash_worktree(id),
            Err(DbError::RefusingMainWorktree)
        ));
        assert!(db.trash_worktree(9_999).unwrap().is_none());
    }

    #[test]
    fn a_failed_removal_comes_back_out_of_trash_with_its_reason() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/failwt");
        db.upsert_repo(root, "failwt").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/failwt", "main", true),
                wt("/tmp/dirty", "d", false),
            ],
        )
        .unwrap();
        let id = db.get_worktree_by_path("/tmp/dirty").unwrap().unwrap().id;
        db.trash_worktree(id).unwrap();
        assert_eq!(db.list_trashed_worktrees().unwrap().len(), 1);

        db.untrash_worktree(id, "contains modified files").unwrap();
        let row = db.get_worktree(id).unwrap().unwrap();
        assert!(row.trashed_at.is_empty(), "out of trash");
        assert_eq!(row.trash_error, "contains modified files");
        // A row out of trash is no longer work — otherwise the worker would spin on
        // a removal git has already refused.
        assert!(db.list_trashed_worktrees().unwrap().is_empty());

        // Retrying clears the previous reason, so a stale error can't outlive it.
        let retried = db.trash_worktree(id).unwrap().unwrap();
        assert_eq!(retried.trash_error, "");

        db.untrash_worktree(id, "again").unwrap();
        assert!(db.clear_trash_error(id).unwrap());
        assert!(!db.clear_trash_error(id).unwrap(), "idempotent");
    }

    #[test]
    fn trashing_preserves_lane_and_position() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/keeps");
        db.upsert_repo(root, "keeps").unwrap();
        db.sync_worktrees(
            root,
            &[wt("/tmp/keeps", "main", true), wt("/tmp/wt", "wt", false)],
        )
        .unwrap();
        db.create_lane(root, "review").unwrap();
        let id = db.get_worktree_by_path("/tmp/wt").unwrap().unwrap().id;
        db.patch_worktree(id, None, None, None, Some("review"))
            .unwrap();
        db.reorder_worktrees(root, &["/tmp/wt".into()]).unwrap();

        db.trash_worktree(id).unwrap();
        // The reason lane membership lives on the row: a restore after a failed
        // removal must not silently ungroup the worktree.
        db.untrash_worktree(id, "nope").unwrap();
        let row = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(row.lane, "review");
        assert_eq!(row.sort_position, Some(0));
    }

    #[test]
    fn a_sync_poll_does_not_reset_user_chosen_columns() {
        // The trap a new column walks into: `sync_worktrees`'s UPDATE runs on every
        // discovery poll, so a user-choice column listed there is silently reset
        // seconds after the user sets it — having appeared to work when tested by
        // hand. This pins the whole set, so adding a column to that statement fails
        // here rather than in someone's afternoon.
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/survive");
        db.upsert_repo(root, "survive").unwrap();
        let discovered = [wt("/tmp/survive", "main", true), wt("/tmp/wt", "wt", false)];
        db.sync_worktrees(root, &discovered).unwrap();
        db.create_lane(root, "review").unwrap();
        let id = db.get_worktree_by_path("/tmp/wt").unwrap().unwrap().id;
        db.patch_worktree(
            id,
            Some("chosen"),
            Some("🦊"),
            Some("#123456"),
            Some("review"),
        )
        .unwrap();
        db.reorder_worktrees(root, &["/tmp/wt".into()]).unwrap();

        // Several polls, including one where git reports a different branch — the
        // case that actually takes the write path.
        db.sync_worktrees(root, &discovered).unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/survive", "main", true),
                wt("/tmp/wt", "renamed-branch", false),
            ],
        )
        .unwrap();

        let row = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(row.alias, "chosen");
        assert_eq!(row.emoji, "🦊");
        assert_eq!(row.marker_color, "#123456");
        assert_eq!(row.lane, "review");
        assert_eq!(row.sort_position, Some(0));
        // ...while the git-derived column DID follow git.
        assert_eq!(row.branch, "renamed-branch");
    }

    #[test]
    fn only_trashed_worktrees_past_their_retention_expire() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/retain");
        db.upsert_repo(root, "retain").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/retain", "main", true),
                wt("/tmp/live", "live", false),
                wt("/tmp/binned", "binned", false),
            ],
        )
        .unwrap();
        let binned = db.get_worktree_by_path("/tmp/binned").unwrap().unwrap().id;

        // Nothing is trashed yet, so nothing can expire however long the horizon.
        assert!(db.expired_trashed_worktrees(0).unwrap().is_empty());

        db.trash_worktree(binned).unwrap();
        // Inside the retention window the checkout stays put — that is the whole
        // point of the holding area, and what makes restore an undo and not a race.
        assert!(db.expired_trashed_worktrees(3600).unwrap().is_empty());

        // Past it, and only the trashed one is offered. A worktree nobody binned is
        // never a candidate, whatever its activity: this is retention, not a scan for
        // idleness.
        let expired = db.expired_trashed_worktrees(0).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].path, "/tmp/binned");

        // Restoring takes it back out of the queue entirely.
        db.untrash_worktree(binned, "").unwrap();
        assert!(db.expired_trashed_worktrees(0).unwrap().is_empty());
    }

    #[test]
    fn reordering_does_not_unplace_a_trashed_worktree() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/keeppos");
        db.upsert_repo(root, "keeppos").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/keeppos", "main", true),
                wt("/tmp/going", "going", false),
                wt("/tmp/staying", "staying", false),
            ],
        )
        .unwrap();
        db.reorder_worktrees(root, &["/tmp/going".into(), "/tmp/staying".into()])
            .unwrap();
        let going = db.get_worktree_by_path("/tmp/going").unwrap().unwrap();
        db.trash_worktree(going.id).unwrap();

        // The UI omits pending removals from the order it sends, so without the
        // `trashed_at = ''` exemption any unrelated drag would strip the position off
        // a worktree mid-removal — and a removal that then failed would put it back
        // unplaced. This is what makes v10's "keeps its lane AND its position" true.
        db.reorder_worktrees(root, &["/tmp/staying".into()])
            .unwrap();
        assert_eq!(
            db.get_worktree(going.id).unwrap().unwrap().sort_position,
            Some(0),
            "a trashed worktree keeps its position through an unrelated reorder"
        );
    }

    #[test]
    fn an_oversized_reorder_is_rejected_before_the_write_lock() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/toolong");
        db.upsert_repo(root, "toolong").unwrap();
        let huge: Vec<String> = (0..=MAX_ORDER_LEN).map(|i| format!("/p/{i}")).collect();
        assert!(matches!(
            db.reorder_worktrees(root, &huge),
            Err(DbError::OrderTooLong(_))
        ));
        assert!(matches!(
            db.reorder_lanes(root, &huge),
            Err(DbError::OrderTooLong(_))
        ));
    }

    #[test]
    fn moving_a_worktree_to_another_lane_clears_its_manual_position() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/lanepos");
        db.upsert_repo(root, "lanepos").unwrap();
        db.sync_worktrees(
            root,
            &[
                wt("/tmp/lanepos", "main", true),
                wt("/tmp/a", "a", false),
                wt("/tmp/b", "b", false),
            ],
        )
        .unwrap();
        db.create_lane(root, "review").unwrap();
        let a = db.get_worktree_by_path("/tmp/a").unwrap().unwrap().id;
        db.reorder_worktrees(root, &["/tmp/a".into(), "/tmp/b".into()])
            .unwrap();
        assert_eq!(db.get_worktree(a).unwrap().unwrap().sort_position, Some(0));

        // Positions are dense repo-wide once anything has been dragged, so carrying
        // one into another lane would tie an existing member and land the worktree
        // mid-lane instead of where "Move to lane" implied.
        db.patch_worktree(a, None, None, None, Some("review"))
            .unwrap();
        let row = db.get_worktree(a).unwrap().unwrap();
        assert_eq!(row.lane, "review");
        assert_eq!(row.sort_position, None);

        // A patch that does NOT change the lane leaves the position alone.
        db.reorder_worktrees(root, &["/tmp/a".into()]).unwrap();
        db.patch_worktree(a, Some("renamed"), None, None, Some("review"))
            .unwrap();
        assert_eq!(db.get_worktree(a).unwrap().unwrap().sort_position, Some(0));
    }

    #[test]
    fn lane_names_that_would_break_a_url_path_are_rejected() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/dots");
        db.upsert_repo(root, "dots").unwrap();
        // `encodeURIComponent` leaves dots unescaped and the URL parser resolves
        // them away, so a lane named `..` could never be renamed or deleted.
        for bad in [".", ".."] {
            assert!(
                matches!(db.create_lane(root, bad), Err(DbError::InvalidLaneName(_))),
                "expected {bad:?} to be rejected"
            );
        }
        // A name merely containing dots is fine.
        assert!(db.create_lane(root, "v1.2").is_ok());
    }

    #[test]
    fn lane_collisions_fold_case_the_way_the_client_does() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/fold");
        db.upsert_repo(root, "fold").unwrap();
        db.create_lane(root, "Ärger").unwrap();
        // Non-ASCII too: the client compares with JavaScript's Unicode-aware
        // `toLowerCase`, and an ASCII-only compare here made the two disagree about
        // what counts as a duplicate.
        assert!(matches!(
            db.create_lane(root, "ärger"),
            Err(DbError::LaneTaken(_))
        ));
        assert!(db.create_lane(root, "andere").is_ok());
    }
}
