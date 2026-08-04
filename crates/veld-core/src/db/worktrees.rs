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
    /// The colour half of the marker: an index into [`WORKTREE_HUES`], or `-1`
    /// for "not assigned yet" (backfilled on the next sync, exactly as `emoji`
    /// is).
    ///
    /// A worktree's marker is a **composite** — this colour and the `emoji`
    /// glyph — and the `worktree.markerStyle` setting picks which face renders.
    /// Both faces are stored permanently and neither is ever cleared by a style
    /// change, which is what makes switching colour → emoji and back lossless.
    /// Do not "simplify" this into a tagged union: that turns a rendering choice
    /// back into a data migration, and the user's hand-picked glyph becomes
    /// something a preference can destroy.
    pub marker_hue: i64,
    pub is_main: bool,
    pub created_at: String,
}

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
const WT_COLS: &str = "id, repo_root, path, branch, alias, emoji, is_main, created_at, marker_hue";

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
        marker_hue: row.get(8)?,
    })
}

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
                let existing: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM worktrees WHERE path = ?1",
                        params![d.path],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(id) = existing {
                    // Write only on change: steady-state syncs (the UI polls
                    // refresh every few seconds) must not take the write path
                    // and append WAL frames for identical rows.
                    tx.execute(
                        "UPDATE worktrees SET branch = ?1, is_main = ?2, repo_root = ?3
                         WHERE id = ?4
                           AND (branch != ?1 OR is_main != ?2 OR repo_root != ?3)",
                        params![d.branch, d.is_main as i64, root, id],
                    )?;
                    // Backfill both marker faces: `emoji` for rows created
                    // before v6, `marker_hue` for rows created before v9. They
                    // are read together and backfilled independently, because a
                    // row upgraded through v6 already has a glyph and needs only
                    // the colour.
                    let (cur_emoji, cur_hue): (String, i64) = tx.query_row(
                        "SELECT emoji, marker_hue FROM worktrees WHERE id = ?1",
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
                    if !is_worktree_hue(cur_hue) {
                        let hue = pick_hue(&tx, &root, &d.branch)?;
                        tx.execute(
                            "UPDATE worktrees SET marker_hue = ?1 WHERE id = ?2",
                            params![hue, id],
                        )?;
                    }
                } else {
                    let alias = unique_alias(&tx, &root, &default_alias(&d.branch))?;
                    let emoji = pick_emoji(&tx, &root, &alias)?;
                    let hue = pick_hue(&tx, &root, &alias)?;
                    tx.execute(
                        "INSERT INTO worktrees
                            (repo_root, path, branch, alias, emoji, is_main, created_at,
                             marker_hue)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            root,
                            d.path,
                            d.branch,
                            alias,
                            emoji,
                            d.is_main as i64,
                            now_str(),
                            hue
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        drop(conn);
        self.list_worktrees(repo_root)
    }

    /// All worktrees of a repo — main checkout first, then alias-sorted.
    pub fn list_worktrees(&self, repo_root: &Path) -> Result<Vec<WorktreeRecord>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {WT_COLS} FROM worktrees WHERE repo_root = ?1
             ORDER BY is_main DESC, alias COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map(params![root_key(repo_root)], wt_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
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
        self.patch_worktree(id, Some(alias), None, None)
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
        marker_hue: Option<i64>,
    ) -> Result<bool, DbError> {
        // Both marker channels are validated before either is written, for the
        // same both-or-neither reason the alias is: a patch carrying a good glyph
        // and a bad hue must not half-apply.
        if let Some(e) = emoji {
            if !is_worktree_emoji(e) {
                return Err(DbError::InvalidEmoji(e.to_owned()));
            }
        }
        if let Some(h) = marker_hue {
            if !is_worktree_hue(h) {
                return Err(DbError::InvalidHue(h));
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
        let n = tx.execute(
            "UPDATE worktrees
                SET alias = COALESCE(?1, alias),
                    emoji = COALESCE(?2, emoji),
                    marker_hue = COALESCE(?3, marker_hue)
              WHERE id = ?4",
            params![alias, emoji, marker_hue, id],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Delete a worktree row (DB only — `git worktree remove` is the caller's
    /// job). Returns whether the row existed.
    pub fn remove_worktree(&self, id: i64) -> Result<bool, DbError> {
        let conn = self.lock();
        let n = conn.execute("DELETE FROM worktrees WHERE id = ?1", params![id])?;
        Ok(n > 0)
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
/// **Eight hues, and eight rather than more on purpose.** Hue is a narrow channel:
/// fewer and genuinely distinct beats more and merely different, because past about
/// eight steps the neighbours stop being tellable apart at rail size — and the
/// honest limit is lower still under a colour vision deficiency (~1 in 12 men). The
/// four that were dropped were buying collision headroom nobody needs: measured on a
/// real database, the repos held 9 and 7 checkouts, and distinctness is only ever
/// needed *within* one repo.
///
/// Index order is the palette's given order. [`pick_hue`] hashes the seed before
/// probing forward, so two worktrees created back to back start from unrelated
/// slots rather than adjacent ones — the ordering only becomes visible once a single
/// repo has nearly as many checkouts as there are hues.
///
/// **Colour is never the only channel.** The alias renders beside the badge
/// everywhere the badge appears, so the swatch is a scanning aid over a text label
/// rather than the identifier itself. A user who cannot separate two hues loses
/// decoration, not function — which is why this does not need a pattern or a
/// monogram bolted on.
///
/// Stored as an **index**, not a colour string: the values are CSS custom properties
/// (`--wt-hue-N`) and the stylesheet owns them, so the palette can be retuned
/// without a migration and the schema never carries a colour it would then have to
/// keep in step with a theme.
///
/// The palette is **identical in both themes**, by decision. An earlier version
/// deepened each hue for the light theme, which meant a worktree changed appearance
/// when the theme was switched — a marker is an identity, and an identity that
/// depends on the surface behind it is doing its job badly.
///
/// Entries may be **appended, never reordered** — the index is persisted per
/// worktree, so a reorder silently repaints every rail. Shrinking the set is safe
/// only because `sync_worktrees` re-picks any hue that fails [`is_worktree_hue`],
/// so a row left holding a dropped index heals on the next sync rather than
/// rendering an undefined variable.
pub const WORKTREE_HUES: usize = 8;

/// Whether `hue` is an assignable marker colour index. `-1` (unassigned) is
/// deliberately **not** valid here: it is a sentinel the backfill clears, not a
/// value a client may write.
pub fn is_worktree_hue(hue: i64) -> bool {
    (0..WORKTREE_HUES as i64).contains(&hue)
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

/// Pick a marker hue, by the same probe as [`pick_emoji`] and for the same
/// reasons — scoped to the repo, since the rail shows one repo at a time and 12
/// hues could not cover a machine.
///
/// The seed is offset from the emoji's so a worktree does not get hue *n* purely
/// because it got glyph *n*: the two faces are independent choices and a user who
/// re-picks one should not find the other implied by it.
fn pick_hue(conn: &rusqlite::Connection, repo_root: &str, seed: &str) -> rusqlite::Result<i64> {
    let mut used = std::collections::HashSet::new();
    let mut stmt = conn.prepare_cached(
        "SELECT marker_hue FROM worktrees WHERE repo_root = ?1 AND marker_hue >= 0",
    )?;
    let rows = stmt.query_map(params![repo_root], |r| r.get::<_, i64>(0))?;
    for row in rows {
        used.insert(row?);
    }
    let h = marker_seed_hash(seed).wrapping_add(WORKTREE_HUES / 2);
    for i in 0..WORKTREE_HUES {
        let candidate = ((h + i) % WORKTREE_HUES) as i64;
        if !used.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Ok((h % WORKTREE_HUES) as i64)
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
            db.patch_worktree(other.id, Some(&main.alias), Some(glyph), None)
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
    fn every_hue_has_a_css_variable_in_both_themes() {
        // `WORKTREE_HUES` is a Rust constant and the ink lives in CSS, with nothing
        // in either language tying them together — so appending a 13th hue here
        // (which the constant's own doc invites) would emit
        // `var(--wt-hue-12)`, resolve to nothing, and render an invisible marker
        // and a blank picker cell. Same drift-gate shape as
        // `documented_detach_grace_matches_the_default`.
        let css = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("veld-daemon/ui/src/styles.css"),
        )
        .expect("read styles.css");
        // **One** definition each: the palette is deliberately identical in both
        // themes, so there is no `body[data-theme="light"]` override. A second
        // definition would mean a worktree's marker changes appearance when the
        // theme is switched, which is the thing that reversal was for.
        for hue in 0..WORKTREE_HUES {
            let n = css.matches(&format!("--wt-hue-{hue}:")).count();
            assert_eq!(
                n, 1,
                "--wt-hue-{hue} is defined {n} times in styles.css; expected exactly \
                 one, since the palette is theme-independent"
            );
        }
        // And nothing beyond the range, which would be a hue the picker can never
        // offer and the allowlist would reject.
        assert!(
            !css.contains(&format!("--wt-hue-{}:", WORKTREE_HUES)),
            "styles.css defines --wt-hue-{} but WORKTREE_HUES is {WORKTREE_HUES}",
            WORKTREE_HUES
        );
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
        let hues: std::collections::HashSet<_> = wts.iter().map(|w| w.marker_hue).collect();
        assert_eq!(glyphs.len(), 3, "glyphs distinct among a repo's checkouts");
        assert_eq!(hues.len(), 3, "hues distinct among a repo's checkouts");
        assert!(wts.iter().all(|w| is_worktree_hue(w.marker_hue)));

        // Rename must not change either face — the marker is a stable
        // identifier, and both halves of it are.
        let before = &wts[0];
        db.rename_worktree(before.id, "renamed").unwrap();
        let after = db.get_worktree(before.id).unwrap().unwrap();
        assert_eq!(after.emoji, before.emoji);
        assert_eq!(after.marker_hue, before.marker_hue);
    }

    #[test]
    fn markers_may_repeat_across_repos() {
        // Deliberate, and a change from the original global probe: 64 glyphs (and
        // 12 hues) cannot cover every worktree of every repo on a machine, so a
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
        assert_eq!(wa[0].marker_hue, wb[0].marker_hue);
    }

    #[test]
    fn a_marker_hue_is_backfilled_for_a_pre_v9_row() {
        // The upgrade path: a row that came through v6 has a glyph and `-1` for
        // its hue. The next sync must fill the colour without disturbing the
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
                "UPDATE worktrees SET marker_hue = -1 WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        let after = db
            .sync_worktrees(root, &[wt("/tmp/repoMarkerBackfill", "main", true)])
            .unwrap();
        assert!(is_worktree_hue(after[0].marker_hue), "hue backfilled");
        assert_eq!(after[0].emoji, glyph, "glyph untouched by the hue backfill");
    }

    #[test]
    fn an_explicit_hue_survives_sync_and_is_range_checked() {
        let (_dir, db) = test_db();
        let root = Path::new("/tmp/repoMarkerPick");
        db.upsert_repo(root, "pick").unwrap();
        let wts = db
            .sync_worktrees(root, &[wt("/tmp/repoMarkerPick", "main", true)])
            .unwrap();
        let id = wts[0].id;
        let chosen = (wts[0].marker_hue + 1) % WORKTREE_HUES as i64;
        assert!(db.patch_worktree(id, None, None, Some(chosen)).unwrap());
        // A user's explicit colour must not be clobbered by the UI's refresh poll.
        db.sync_worktrees(root, &[wt("/tmp/repoMarkerPick", "main", true)])
            .unwrap();
        assert_eq!(db.get_worktree(id).unwrap().unwrap().marker_hue, chosen);

        for bad in [-1, WORKTREE_HUES as i64, 9999] {
            assert!(
                matches!(
                    db.patch_worktree(id, None, None, Some(bad)),
                    Err(DbError::InvalidHue(_))
                ),
                "{bad} must be rejected"
            );
        }
        // A rejected hue must not commit the alias that travelled with it.
        assert!(
            db.patch_worktree(id, Some("nope"), None, Some(9999))
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
        assert!(db.patch_worktree(id, None, Some(chosen), None).unwrap());
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
                    db.patch_worktree(1, None, Some(bad), None),
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
        assert!(db.patch_worktree(id, Some("renamed"), None, None).unwrap());
        let after = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(after.alias, "renamed");
        assert_eq!(after.emoji, original.emoji);

        assert!(db.patch_worktree(id, None, Some(glyph), None).unwrap());
        let after = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(after.alias, "renamed");
        assert_eq!(&after.emoji, glyph);

        // Both at once.
        assert!(
            db.patch_worktree(id, Some("both"), Some(&original.emoji), None)
                .unwrap()
        );
        let after = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(after.alias, "both");
        assert_eq!(after.emoji, original.emoji);

        // A rejected emoji must not commit the alias that travelled with it.
        assert!(
            db.patch_worktree(id, Some("nope"), Some("🍕"), None)
                .is_err()
        );
        assert_eq!(db.get_worktree(id).unwrap().unwrap().alias, "both");

        // Neither field: a no-op write that still reports row existence.
        assert!(db.patch_worktree(id, None, None, None).unwrap());
        let after = db.get_worktree(id).unwrap().unwrap();
        assert_eq!(after.alias, "both");
        assert_eq!(after.emoji, original.emoji);

        assert!(!db.patch_worktree(4242, Some("x"), None, None).unwrap());
        assert!(!db.patch_worktree(4242, None, None, None).unwrap());
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
}
