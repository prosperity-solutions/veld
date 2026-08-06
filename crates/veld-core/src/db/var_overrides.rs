//! This machine's answers for vars the config declared overridable.
//!
//! The config carries the declaration and the default; a row here carries what
//! *this* machine answered. Two rules run through the module:
//!
//! - **A row stores a `ConfigValue`, not a string.** An override may be a
//!   pointer (`{"env": "PGPASS"}`, `{"shell": "op read op://…"}`), which is what
//!   lets a `secret: true` var be overridable while veld keeps carrying a
//!   *pointer plus a sensitivity flag* rather than taking custody of a secret.
//! - **Rows are keyed by [`ProjectId`], not by project root.** Every worktree of
//!   one repo resolves to one id, so the answer is given once per machine. The
//!   narrower per-checkout answer is a second scope, not a second table.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::params;

use super::{Db, DbError, ts_to_str};
use crate::config::ConfigValue;
use crate::project_id::ProjectId;

/// Which answer a row is: the project's, or this one checkout's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverrideScope {
    /// Shared by every worktree of the project. The default, and the case the
    /// feature exists for.
    Project,
    /// This checkout only — sizing one worktree down while another runs, or
    /// pointing one at a different mirror. Wins over [`OverrideScope::Project`].
    Worktree,
}

impl OverrideScope {
    /// The stored discriminant.
    pub fn as_str(self) -> &'static str {
        match self {
            OverrideScope::Project => "project",
            OverrideScope::Worktree => "worktree",
        }
    }

    /// Parse a stored discriminant. `None` for anything a newer binary wrote:
    /// an unreadable row is skipped rather than failing the read, so a downgrade
    /// keeps working on the scopes it does understand.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "project" => Some(OverrideScope::Project),
            "worktree" => Some(OverrideScope::Worktree),
            _ => None,
        }
    }
}

impl std::fmt::Display for OverrideScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One stored answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarOverride {
    pub name: String,
    pub scope: OverrideScope,
    /// The checkout a [`OverrideScope::Worktree`] row belongs to; empty for a
    /// project-scoped row.
    pub scope_key: String,
    /// What to resolve — a literal or a pointer.
    pub value: ConfigValue,
    pub updated_at: String,
}

/// The `scope_key` a scope stores. A project row has none, so every worktree
/// reads the same row; a worktree row carries its checkout.
fn scope_key(scope: OverrideScope, worktree: &Path) -> String {
    match scope {
        OverrideScope::Project => String::new(),
        OverrideScope::Worktree => canonical_key(worktree),
    }
}

/// Canonicalized so two spellings of one checkout (a symlinked `/tmp`, a
/// case-different mount) do not become two rows the user cannot tell apart.
fn canonical_key(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

impl Db {
    /// Record this machine's answer for one var.
    ///
    /// Last write wins, which is right for a machine fact: there is one laptop
    /// and one person answering for it.
    pub fn set_var_override(
        &self,
        project: &ProjectId,
        scope: OverrideScope,
        worktree: &Path,
        name: &str,
        value: &ConfigValue,
    ) -> Result<(), DbError> {
        let encoded = serde_json::to_string(value)?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO var_overrides (project_id, scope, scope_key, name, value, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id, scope, scope_key, name) DO UPDATE SET
                 value      = excluded.value,
                 updated_at = excluded.updated_at",
            params![
                project.as_str(),
                scope.as_str(),
                scope_key(scope, worktree),
                name,
                encoded,
                ts_to_str(chrono::Utc::now())
            ],
        )?;
        Ok(())
    }

    /// Forget this machine's answer. Returns whether a row was there to forget,
    /// so the CLI can say "not set on this machine" instead of claiming success.
    pub fn unset_var_override(
        &self,
        project: &ProjectId,
        scope: OverrideScope,
        worktree: &Path,
        name: &str,
    ) -> Result<bool, DbError> {
        let conn = self.lock();
        let n = conn.execute(
            "DELETE FROM var_overrides
             WHERE project_id = ?1 AND scope = ?2 AND scope_key = ?3 AND name = ?4",
            params![
                project.as_str(),
                scope.as_str(),
                scope_key(scope, worktree),
                name
            ],
        )?;
        Ok(n > 0)
    }

    /// Every override that applies to this checkout, both scopes, ordered by
    /// name then scope. A row whose scope or value this binary cannot read is
    /// skipped with a warning rather than failing the whole read.
    pub fn var_overrides(
        &self,
        project: &ProjectId,
        worktree: &Path,
    ) -> Result<Vec<VarOverride>, DbError> {
        let wt = canonical_key(worktree);
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT name, scope, scope_key, value, updated_at FROM var_overrides
             WHERE project_id = ?1 AND (scope_key = '' OR scope_key = ?2)
             ORDER BY name, scope",
        )?;
        let rows = stmt
            .query_map(params![project.as_str(), wt], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut out = Vec::with_capacity(rows.len());
        for (name, scope, scope_key, value, updated_at) in rows {
            let Some(scope) = OverrideScope::parse(&scope) else {
                tracing::warn!(
                    var = %name,
                    scope = %scope,
                    "skipping a var override written by a newer veld"
                );
                continue;
            };
            // A project row's `scope_key` is '' and matched above; a worktree row
            // matched this checkout. Nothing else can reach here, but a stray
            // project-scoped row with a non-empty key would silently apply to
            // every checkout, so it is checked rather than assumed.
            if scope == OverrideScope::Project && !scope_key.is_empty() {
                tracing::warn!(var = %name, "skipping a project override with a checkout key");
                continue;
            }
            match serde_json::from_str::<ConfigValue>(&value) {
                Ok(value) => out.push(VarOverride {
                    name,
                    scope,
                    scope_key,
                    value,
                    updated_at,
                }),
                Err(e) => tracing::warn!(
                    var = %name,
                    error = %e,
                    "skipping a var override this veld cannot read"
                ),
            }
        }
        Ok(out)
    }

    /// The winning override per var for this checkout: `worktree` beats
    /// `project`, because the narrower answer is the one the user asked for
    /// *here*.
    pub fn effective_var_overrides(
        &self,
        project: &ProjectId,
        worktree: &Path,
    ) -> Result<BTreeMap<String, VarOverride>, DbError> {
        let mut out: BTreeMap<String, VarOverride> = BTreeMap::new();
        for row in self.var_overrides(project, worktree)? {
            match out.get(&row.name) {
                Some(existing) if existing.scope >= row.scope => {}
                _ => {
                    out.insert(row.name.clone(), row);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open_at(&dir.path().join("veld.db")).expect("open");
        (db, dir)
    }

    fn pid() -> ProjectId {
        ProjectId::from_stored("/repos/app")
    }

    fn literal(v: &str) -> ConfigValue {
        ConfigValue::literal(v)
    }

    #[test]
    fn setting_then_reading_round_trips_a_literal() {
        let (db, dir) = db();
        let wt = dir.path();
        db.set_var_override(
            &pid(),
            OverrideScope::Project,
            wt,
            "runtime",
            &literal("podman"),
        )
        .expect("set");
        let all = db.var_overrides(&pid(), wt).expect("read");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "runtime");
        assert_eq!(all[0].scope, OverrideScope::Project);
        assert_eq!(all[0].value, literal("podman"));
    }

    /// The property the whole custody decision rests on: an override may be a
    /// *pointer*, so a `secret: true` var can be answered per machine without
    /// veld storing the secret. If this stops round-tripping, the only way to
    /// answer a secret var becomes a literal in the database.
    #[test]
    fn an_override_may_be_a_pointer_and_keeps_its_secret_flag() {
        let (db, dir) = db();
        let wt = dir.path();
        let pointer: ConfigValue =
            serde_json::from_str(r#"{"shell":"op read op://dev/pg/url","secret":true}"#)
                .expect("parses");
        db.set_var_override(&pid(), OverrideScope::Project, wt, "db_url", &pointer)
            .expect("set");
        let back = &db.var_overrides(&pid(), wt).expect("read")[0].value;
        assert_eq!(back, &pointer);
        assert!(
            back.secret,
            "a stored pointer must not lose its sensitivity"
        );
    }

    #[test]
    fn a_worktree_override_beats_the_projects() {
        let (db, dir) = db();
        let wt = dir.path();
        db.set_var_override(&pid(), OverrideScope::Project, wt, "mem", &literal("8g"))
            .expect("set");
        db.set_var_override(&pid(), OverrideScope::Worktree, wt, "mem", &literal("2g"))
            .expect("set");

        let eff = db.effective_var_overrides(&pid(), wt).expect("read");
        assert_eq!(eff["mem"].value, literal("2g"));
        assert_eq!(eff["mem"].scope, OverrideScope::Worktree);
        // …and unsetting the narrower one exposes the project answer again
        // rather than leaving the var unset.
        assert!(
            db.unset_var_override(&pid(), OverrideScope::Worktree, wt, "mem")
                .expect("unset")
        );
        let eff = db.effective_var_overrides(&pid(), wt).expect("read");
        assert_eq!(eff["mem"].value, literal("8g"));
        assert_eq!(eff["mem"].scope, OverrideScope::Project);
    }

    /// A worktree row belongs to the checkout that set it. Without the
    /// `scope_key` filter the narrower answer would leak into every other
    /// worktree of the project — the exact bug the project scope exists to avoid
    /// having to work around.
    #[test]
    fn a_worktree_override_is_invisible_from_another_checkout() {
        let (db, dir) = db();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(&a).expect("mkdir");
        std::fs::create_dir_all(&b).expect("mkdir");

        db.set_var_override(&pid(), OverrideScope::Project, &a, "mem", &literal("8g"))
            .expect("set");
        db.set_var_override(&pid(), OverrideScope::Worktree, &a, "mem", &literal("2g"))
            .expect("set");

        assert_eq!(
            db.effective_var_overrides(&pid(), &b).expect("read")["mem"].value,
            literal("8g"),
            "checkout b must see the project answer, not checkout a's"
        );
        assert_eq!(
            db.effective_var_overrides(&pid(), &a).expect("read")["mem"].value,
            literal("2g")
        );
    }

    /// The reason a project id exists at all: two worktrees of one repo resolve
    /// to the same id, so the project-scoped answer is given once.
    #[test]
    fn one_project_id_serves_every_checkout() {
        let (db, dir) = db();
        let a = dir.path().join("wt-a");
        let b = dir.path().join("wt-b");
        std::fs::create_dir_all(&a).expect("mkdir");
        std::fs::create_dir_all(&b).expect("mkdir");
        db.set_var_override(
            &pid(),
            OverrideScope::Project,
            &a,
            "runtime",
            &literal("podman"),
        )
        .expect("set");
        assert_eq!(
            db.effective_var_overrides(&pid(), &b).expect("read")["runtime"].value,
            literal("podman")
        );
    }

    #[test]
    fn unsetting_something_never_set_reports_it() {
        let (db, dir) = db();
        assert!(
            !db.unset_var_override(&pid(), OverrideScope::Project, dir.path(), "nope")
                .expect("unset")
        );
    }

    #[test]
    fn setting_twice_replaces_rather_than_duplicates() {
        let (db, dir) = db();
        let wt = dir.path();
        db.set_var_override(
            &pid(),
            OverrideScope::Project,
            wt,
            "runtime",
            &literal("docker"),
        )
        .expect("set");
        db.set_var_override(
            &pid(),
            OverrideScope::Project,
            wt,
            "runtime",
            &literal("podman"),
        )
        .expect("set");
        let all = db.var_overrides(&pid(), wt).expect("read");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].value, literal("podman"));
    }

    /// A row from a newer veld must not fail the read for everything else — the
    /// same downgrade reasoning the settings store applies to unknown keys.
    #[test]
    fn a_scope_this_binary_does_not_know_is_skipped_not_fatal() {
        let (db, dir) = db();
        let wt = dir.path();
        db.set_var_override(&pid(), OverrideScope::Project, wt, "keep", &literal("yes"))
            .expect("set");
        db.lock()
            .execute(
                "INSERT INTO var_overrides (project_id, scope, scope_key, name, value, updated_at)
                 VALUES (?1, 'galaxy', '', 'skip', '\"x\"', '2026-01-01T00:00:00.000000Z')",
                params![pid().as_str()],
            )
            .expect("insert");
        let all = db.var_overrides(&pid(), wt).expect("read");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "keep");
    }

    #[test]
    fn another_projects_overrides_are_not_visible() {
        let (db, dir) = db();
        let wt = dir.path();
        db.set_var_override(
            &pid(),
            OverrideScope::Project,
            wt,
            "runtime",
            &literal("podman"),
        )
        .expect("set");
        let other = ProjectId::from_stored("/repos/other");
        assert!(db.var_overrides(&other, wt).expect("read").is_empty());
    }
}
