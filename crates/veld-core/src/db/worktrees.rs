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
const WT_COLS: &str = "id, repo_root, path, branch, alias, is_main, created_at";

fn wt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeRecord> {
    Ok(WorktreeRecord {
        id: row.get(0)?,
        repo_root: row.get(1)?,
        path: row.get(2)?,
        branch: row.get(3)?,
        alias: row.get(4)?,
        is_main: row.get::<_, i64>(5)? != 0,
        created_at: row.get(6)?,
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
                } else {
                    let alias = unique_alias(&tx, &root, &default_alias(&d.branch))?;
                    tx.execute(
                        "INSERT INTO worktrees
                            (repo_root, path, branch, alias, is_main, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![root, d.path, d.branch, alias, d.is_main as i64, now_str()],
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

    /// Rename a worktree's alias. Returns whether the row existed.
    pub fn rename_worktree(&self, id: i64, alias: &str) -> Result<bool, DbError> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE worktrees SET alias = ?1 WHERE id = ?2",
            params![alias, id],
        )?;
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

/// Make `base` unique among a repo's aliases by appending `-2`, `-3`, … as
/// needed. Runs inside the sync transaction.
fn unique_alias(
    conn: &rusqlite::Connection,
    repo_root: &str,
    base: &str,
) -> rusqlite::Result<String> {
    let taken = |alias: &str| -> rusqlite::Result<bool> {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM worktrees WHERE repo_root = ?1 AND alias = ?2",
            params![repo_root, alias],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    };
    if !taken(base)? {
        return Ok(base.to_string());
    }
    for i in 2.. {
        let candidate = format!("{base}-{i}");
        if !taken(&candidate)? {
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
