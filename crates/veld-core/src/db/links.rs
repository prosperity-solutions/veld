//! Links an agent pins to its worktree — the pull request it opened, the preview
//! it deployed, the issue it works on, a page the human keeps coming back to.
//!
//! **Veld stores what the agent says and never works anything out from a URL.**
//! Which forge a URL belongs to, whether a pull request is merged, what to call
//! it — all of that is the agent's, because core never learns a provider's name
//! (`docs/extensions-vision.md`). The one thing derived here is a fallback label
//! from the URL's host and path, which is provider-blind.
//!
//! Three rules shape the API:
//!
//! - **A `set` merges.** A field the caller leaves out keeps its stored value, so
//!   marking a pull request merged (`--tone success`) does not reset the label it
//!   was given when it was opened. An empty string resets a field to its default.
//! - **Defaults are stored as empty and resolved on read.** An empty `label` is
//!   the URL-derived one and an empty `icon` is the kind's glyph (resolved by the
//!   UI), so a link whose URL changes under the same id does not keep a label
//!   naming the old one, and a default can change without a migration.
//! - **Strict on write, lenient on read.** A `set` refuses an unknown kind, tone,
//!   or icon, so the agent hears about a typo; a row a newer binary
//!   wrote reads back with the unknown parts as their defaults.

use rusqlite::{OptionalExtension as _, params};
use serde::Serialize;

use super::{Db, DbError, now_str};
use crate::ide::{PaneIcon, is_web_url, parse_icon_name};

/// Links one worktree may carry. A cap, not a target: past a handful the top bar
/// folds them into a menu, and twenty is far beyond what a human scans.
pub const MAX_LINKS_PER_WORKTREE: usize = 20;
/// Characters of a label, the same bound an extension badge's text has.
pub const MAX_LINK_LABEL_CHARS: usize = 60;
/// Characters of a tooltip, the same bound an extension badge's has.
pub const MAX_LINK_TOOLTIP_CHARS: usize = 400;
/// Bytes of a URL.
pub const MAX_LINK_URL_LEN: usize = 2048;
/// Characters of an explicit `--id`.
pub const MAX_LINK_ID_CHARS: usize = 128;

/// What a link is, which decides its default glyph and how the top bar groups it.
/// `pr` links are also the ones the rail shows beside the worktree's name.
pub const LINK_KINDS: &[&str] = &["pr", "preview", "issue", "doc", "link"];
/// A link's colour — the vocabulary extension badges use.
pub const LINK_TONES: &[&str] = &["neutral", "info", "success", "warning", "danger"];

/// One stored link, resolved for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeLink {
    pub id: String,
    /// One of [`LINK_KINDS`]; an unknown stored kind reads as `link`.
    pub kind: String,
    pub url: String,
    /// Never empty: the agent's label, or one derived from the URL.
    pub label: String,
    /// `None` means the kind's default glyph.
    pub icon: Option<PaneIcon>,
    /// One of [`LINK_TONES`]; an unknown stored tone reads as `neutral`.
    pub tone: String,
    /// Empty when there is none.
    pub tooltip: String,
    /// The worktree's branch when the link was last set — empty if unknown. A
    /// link set on another branch is about other work, which the UI shows dimmed.
    pub branch: String,
    /// When the link was last set (RFC 3339), so a reader can tell how old the
    /// agent's word on it is.
    pub updated_at: String,
}

/// One `veld ide link set`. Every field but `url` is optional: `None` keeps the
/// stored value (or takes the default when the link is new), `Some("")` resets
/// it to the default.
#[derive(Debug, Clone, Default)]
pub struct LinkInput {
    pub url: String,
    pub id: Option<String>,
    pub kind: Option<String>,
    pub label: Option<String>,
    pub icon: Option<String>,
    pub tone: Option<String>,
    pub tooltip: Option<String>,
}

/// What a `set` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkSet {
    /// Stored. `created` is false when an existing link was updated.
    Stored {
        link: Box<WorktreeLink>,
        created: bool,
    },
    /// Refused: the worktree already carries [`MAX_LINKS_PER_WORKTREE`].
    Full,
    /// No such worktree.
    Missing,
}

/// A `set` that cannot be stored as asked — the sentence says why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRejected(pub String);

impl std::fmt::Display for LinkRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A validated [`LinkInput`]: the id resolved, each present field checked.
#[derive(Debug, Clone)]
struct CheckedLink {
    id: String,
    url: String,
    kind: Option<String>,
    label: Option<String>,
    icon: Option<String>,
    tone: Option<String>,
    tooltip: Option<String>,
}

fn one_of(field: &str, value: &str, allowed: &[&str]) -> Result<(), LinkRejected> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(LinkRejected(format!(
            "unknown {field} {value:?} — use one of: {}",
            allowed.join(", ")
        )))
    }
}

/// Check `input` and resolve its id, without touching the database.
fn check(input: LinkInput) -> Result<CheckedLink, LinkRejected> {
    let url = input.url.trim().to_owned();
    if !is_web_url(&url) {
        return Err(LinkRejected(format!(
            "{url:?} is not an http(s) URL — a link opens in a browser"
        )));
    }
    if url.len() > MAX_LINK_URL_LEN || url.chars().any(char::is_whitespace) {
        return Err(LinkRejected(format!(
            "not a usable URL: it must be at most {MAX_LINK_URL_LEN} bytes with no spaces"
        )));
    }
    let trimmed = |v: Option<String>| v.map(|s| s.trim().to_owned());
    let kind = trimmed(input.kind);
    if let Some(kind) = kind.as_deref() {
        one_of("kind", kind, LINK_KINDS)?;
    }
    let tone = trimmed(input.tone).map(|t| {
        if t.is_empty() {
            "neutral".to_owned()
        } else {
            t
        }
    });
    if let Some(tone) = tone.as_deref() {
        one_of("tone", tone, LINK_TONES)?;
    }
    let icon = trimmed(input.icon);
    if let Some(icon) = icon.as_deref().filter(|i| !i.is_empty())
        && parse_icon_name(icon).is_none()
    {
        return Err(LinkRejected(format!(
            "unknown icon {icon:?} — use an emoji, or one of: {}",
            crate::ide::PANE_ICON_NAMES.join(", ")
        )));
    }
    let label = trimmed(input.label);
    if let Some(label) = label.as_deref()
        && label.chars().count() > MAX_LINK_LABEL_CHARS
    {
        return Err(LinkRejected(format!(
            "the label is {} characters; at most {MAX_LINK_LABEL_CHARS} fit the top bar",
            label.chars().count()
        )));
    }
    let tooltip = trimmed(input.tooltip);
    if let Some(tooltip) = tooltip.as_deref()
        && tooltip.chars().count() > MAX_LINK_TOOLTIP_CHARS
    {
        return Err(LinkRejected(format!(
            "the tooltip is longer than {MAX_LINK_TOOLTIP_CHARS} characters"
        )));
    }
    let id = match trimmed(input.id).filter(|i| !i.is_empty()) {
        Some(id) => {
            check_link_id(&id)?;
            id
        }
        None => normalize_link_url(&url),
    };
    Ok(CheckedLink {
        id,
        url,
        kind,
        label,
        icon,
        tone,
        tooltip,
    })
}

/// An explicit id: short, and spelled so it survives a shell unquoted.
fn check_link_id(id: &str) -> Result<(), LinkRejected> {
    let ok = id.chars().count() <= MAX_LINK_ID_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/#:@-".contains(c));
    if ok {
        Ok(())
    } else {
        Err(LinkRejected(format!(
            "not a usable id {id:?}: at most {MAX_LINK_ID_CHARS} characters of letters, \
             digits and . _ / # : @ -"
        )))
    }
}

/// The id a URL is filed under when none is given — and what `rm <url>` looks
/// for: scheme and host lowercased, the query, fragment and trailing slash
/// dropped. So `…/pull/12/` and `…/pull/12#issuecomment-1` are the same link.
#[must_use]
pub fn normalize_link_url(url: &str) -> String {
    let url = url.trim();
    let url = url.split(['#', '?']).next().unwrap_or(url);
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let (host, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let path = path.trim_end_matches('/');
    format!(
        "{}://{}{path}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    )
}

/// A label for a link the agent gave none: the host, and the last two segments
/// of the path — `github.com › pull/12`. Provider-blind on purpose.
#[must_use]
pub fn default_link_label(url: &str) -> String {
    let normalized = normalize_link_url(url);
    let rest = normalized
        .split_once("://")
        .map_or(normalized.as_str(), |(_, r)| r);
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let host = parts.next().unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(host);
    let segments: Vec<&str> = parts.collect();
    let tail = segments[segments.len().saturating_sub(2)..].join("/");
    let label = if tail.is_empty() {
        host.to_owned()
    } else {
        format!("{host} › {tail}")
    };
    if label.chars().count() <= MAX_LINK_LABEL_CHARS {
        label
    } else {
        let kept: String = label.chars().take(MAX_LINK_LABEL_CHARS - 1).collect();
        format!("{kept}…")
    }
}

const LINK_COLUMNS: &str = "link_id, kind, url, label, icon, tone, tooltip, branch, updated_at";

fn read_link(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<WorktreeLink> {
    let get = |i: usize| row.get::<_, String>(offset + i);
    let kind = get(1)?;
    let url = get(2)?;
    let label = get(3)?;
    let tone = get(5)?;
    Ok(WorktreeLink {
        id: get(0)?,
        kind: if LINK_KINDS.contains(&kind.as_str()) {
            kind
        } else {
            "link".to_owned()
        },
        label: if label.is_empty() {
            default_link_label(&url)
        } else {
            label
        },
        url,
        icon: parse_icon_name(&get(4)?),
        tone: if LINK_TONES.contains(&tone.as_str()) {
            tone
        } else {
            "neutral".to_owned()
        },
        tooltip: get(6)?,
        branch: get(7)?,
        updated_at: get(8)?,
    })
}

impl Db {
    /// Add a link to a worktree, or update the one with the same id.
    ///
    /// # Errors
    ///
    /// [`LinkRejected`] (as the outer `Err`) when the input does not validate;
    /// the inner result is the database's.
    pub fn set_worktree_link(
        &self,
        worktree_id: i64,
        input: LinkInput,
    ) -> Result<Result<LinkSet, DbError>, LinkRejected> {
        let link = check(input)?;
        Ok(self.store_link(worktree_id, &link))
    }

    fn store_link(&self, worktree_id: i64, link: &CheckedLink) -> Result<LinkSet, DbError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let Some(branch) = tx
            .query_row(
                "SELECT branch FROM worktrees WHERE id = ?1",
                params![worktree_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        else {
            return Ok(LinkSet::Missing);
        };
        let exists = tx
            .query_row(
                "SELECT 1 FROM worktree_links WHERE worktree_id = ?1 AND link_id = ?2",
                params![worktree_id, link.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let now = now_str();
        if exists {
            // `COALESCE(?, column)`: a field the caller did not pass keeps what is
            // stored. `created_at` is never touched, so an update does not move
            // the link in the top bar's order.
            tx.execute(
                "UPDATE worktree_links SET
                    url = ?3,
                    kind = COALESCE(?4, kind),
                    label = COALESCE(?5, label),
                    icon = COALESCE(?6, icon),
                    tone = COALESCE(?7, tone),
                    tooltip = COALESCE(?8, tooltip),
                    branch = ?9,
                    updated_at = ?10
                 WHERE worktree_id = ?1 AND link_id = ?2",
                params![
                    worktree_id,
                    link.id,
                    link.url,
                    link.kind,
                    link.label,
                    link.icon,
                    link.tone,
                    link.tooltip,
                    branch,
                    now
                ],
            )?;
        } else {
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM worktree_links WHERE worktree_id = ?1",
                params![worktree_id],
                |r| r.get(0),
            )?;
            if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_LINKS_PER_WORKTREE {
                return Ok(LinkSet::Full);
            }
            tx.execute(
                "INSERT INTO worktree_links
                    (worktree_id, link_id, kind, url, label, icon, tone, tooltip, branch,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    worktree_id,
                    link.id,
                    link.kind.as_deref().unwrap_or("link"),
                    link.url,
                    link.label.as_deref().unwrap_or(""),
                    link.icon.as_deref().unwrap_or(""),
                    link.tone.as_deref().unwrap_or("neutral"),
                    link.tooltip.as_deref().unwrap_or(""),
                    branch,
                    now
                ],
            )?;
        }
        let stored = tx.query_row(
            &format!(
                "SELECT {LINK_COLUMNS} FROM worktree_links
                 WHERE worktree_id = ?1 AND link_id = ?2"
            ),
            params![worktree_id, link.id],
            |r| read_link(r, 0),
        )?;
        tx.commit()?;
        Ok(LinkSet::Stored {
            link: Box::new(stored),
            created: !exists,
        })
    }

    /// A worktree's links, in the order they were first set.
    pub fn worktree_links(&self, worktree_id: i64) -> Result<Vec<WorktreeLink>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {LINK_COLUMNS} FROM worktree_links WHERE worktree_id = ?1
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt
            .query_map(params![worktree_id], |r| read_link(r, 0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every worktree's links in one repository, by worktree id — one read for
    /// the rail's whole listing rather than one per row.
    pub fn links_in_repo(
        &self,
        repo_root: &std::path::Path,
    ) -> Result<std::collections::HashMap<i64, Vec<WorktreeLink>>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT l.worktree_id, {} FROM worktree_links l
             JOIN worktrees w ON w.id = l.worktree_id
             WHERE w.repo_root = ?1
             ORDER BY l.created_at, l.rowid",
            LINK_COLUMNS
                .split(", ")
                .map(|c| format!("l.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let mut out: std::collections::HashMap<i64, Vec<WorktreeLink>> =
            std::collections::HashMap::new();
        let rows = stmt.query_map(params![repo_root.to_string_lossy()], |r| {
            Ok((r.get::<_, i64>(0)?, read_link(r, 1)?))
        })?;
        for row in rows {
            let (id, link) = row?;
            out.entry(id).or_default().push(link);
        }
        Ok(out)
    }

    /// Remove one link by its id — or by its URL, which finds the link filed
    /// under it. Returns the removed link, `None` when nothing matched.
    pub fn remove_worktree_link(
        &self,
        worktree_id: i64,
        id_or_url: &str,
    ) -> Result<Option<WorktreeLink>, DbError> {
        let key = id_or_url.trim();
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut candidates = vec![key.to_owned()];
        if is_web_url(key) {
            candidates.push(normalize_link_url(key));
        }
        let mut found = None;
        for candidate in &candidates {
            found = tx
                .query_row(
                    &format!(
                        "SELECT {LINK_COLUMNS} FROM worktree_links
                         WHERE worktree_id = ?1 AND link_id = ?2"
                    ),
                    params![worktree_id, candidate],
                    |r| read_link(r, 0),
                )
                .optional()?;
            if found.is_some() {
                break;
            }
        }
        // A link filed under an explicit id (`--id web`) is still found by
        // its URL.
        if found.is_none() && is_web_url(key) {
            let wanted = normalize_link_url(key);
            let mut stmt = tx.prepare(&format!(
                "SELECT {LINK_COLUMNS} FROM worktree_links WHERE worktree_id = ?1"
            ))?;
            let all = stmt
                .query_map(params![worktree_id], |r| read_link(r, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            found = all
                .into_iter()
                .find(|l| normalize_link_url(&l.url) == wanted);
        }
        if let Some(link) = &found {
            tx.execute(
                "DELETE FROM worktree_links WHERE worktree_id = ?1 AND link_id = ?2",
                params![worktree_id, link.id],
            )?;
        }
        tx.commit()?;
        Ok(found)
    }

    /// Remove every link of a worktree. Returns how many there were.
    pub fn clear_worktree_links(&self, worktree_id: i64) -> Result<usize, DbError> {
        let conn = self.lock();
        Ok(conn.execute(
            "DELETE FROM worktree_links WHERE worktree_id = ?1",
            params![worktree_id],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::db::test_db;

    fn worktree(db: &Db) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO repos (root, name, created_at)
             VALUES ('/repo', 'repo', '2026-01-01T00:00:00.000000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (repo_root, path, branch, alias, is_main, created_at)
             VALUES ('/repo', '/repo', 'feat/x', 'repo', 1, '2026-01-01T00:00:00.000000Z')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn deleting_a_worktree_takes_its_links_with_it() {
        let (_d, db) = test_db();
        let wt = worktree(&db);
        set(&db, wt, url("https://a.dev"));
        db.lock()
            .execute("DELETE FROM worktrees WHERE id = ?1", params![wt])
            .unwrap();
        assert!(db.worktree_links(wt).unwrap().is_empty());
    }

    fn set(db: &Db, wt: i64, input: LinkInput) -> WorktreeLink {
        match db.set_worktree_link(wt, input).unwrap().unwrap() {
            LinkSet::Stored { link, .. } => *link,
            other => panic!("not stored: {other:?}"),
        }
    }

    fn url(u: &str) -> LinkInput {
        LinkInput {
            url: u.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn a_bare_url_is_a_link_with_a_label_from_its_host_and_path() {
        let (_d, db) = test_db();
        let wt = worktree(&db);
        let link = set(&db, wt, url("https://www.example.com/a/b/c/?x=1#y"));
        assert_eq!(link.kind, "link");
        assert_eq!(link.label, "example.com › b/c");
        assert_eq!(link.id, "https://www.example.com/a/b/c");
        assert_eq!(link.tone, "neutral");
        assert_eq!(link.branch, "feat/x");
        assert_eq!(link.icon, None);
    }

    #[test]
    fn setting_again_merges_and_keeps_what_was_not_passed() {
        let (_d, db) = test_db();
        let wt = worktree(&db);
        let pr = "https://git.example.com/acme/api/pull/12";
        set(
            &db,
            wt,
            LinkInput {
                kind: Some("pr".into()),
                label: Some("api#12".into()),
                tooltip: Some("Add login".into()),
                ..url(pr)
            },
        );
        let merged = set(
            &db,
            wt,
            LinkInput {
                tone: Some("success".into()),
                icon: Some("git-merge".into()),
                ..url(&format!("{pr}/"))
            },
        );
        assert_eq!(merged.label, "api#12");
        assert_eq!(merged.tooltip, "Add login");
        assert_eq!(merged.kind, "pr");
        assert_eq!(merged.tone, "success");
        assert_eq!(db.worktree_links(wt).unwrap().len(), 1);
        let reset = set(
            &db,
            wt,
            LinkInput {
                label: Some(String::new()),
                icon: Some(String::new()),
                ..url(pr)
            },
        );
        assert_eq!(reset.label, "git.example.com › pull/12");
        assert_eq!(reset.icon, None);
    }

    #[test]
    fn previews_of_several_services_stand_side_by_side_and_an_id_replaces_one() {
        let (_d, db) = test_db();
        let wt = worktree(&db);
        let preview = |u: &str, id: &str| LinkInput {
            kind: Some("preview".into()),
            id: Some(id.into()),
            ..url(u)
        };
        set(&db, wt, preview("https://web-1.preview.example.com", "web"));
        set(&db, wt, preview("https://api-1.preview.example.com", "api"));
        set(&db, wt, preview("https://web-2.preview.example.com", "web"));
        let links = db.worktree_links(wt).unwrap();
        let urls: Vec<_> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://web-2.preview.example.com",
                "https://api-1.preview.example.com"
            ]
        );
        assert_eq!(links[0].label, "web-2.preview.example.com");
        set(
            &db,
            wt,
            LinkInput {
                kind: Some("preview".into()),
                ..url("https://docs-preview.example.com")
            },
        );
        assert_eq!(db.worktree_links(wt).unwrap().len(), 3);
    }

    #[test]
    fn typos_are_refused_with_the_choices() {
        let (_d, db) = test_db();
        let wt = worktree(&db);
        for (input, needle) in [
            (url("ftp://x"), "http(s)"),
            (
                LinkInput {
                    kind: Some("pull".into()),
                    ..url("https://x.dev")
                },
                "pr, preview",
            ),
            (
                LinkInput {
                    tone: Some("green".into()),
                    ..url("https://x.dev")
                },
                "success",
            ),
            (
                LinkInput {
                    icon: Some("nonesuch".into()),
                    ..url("https://x.dev")
                },
                "git-pull-request",
            ),
            (
                LinkInput {
                    id: Some("has space".into()),
                    ..url("https://x.dev")
                },
                "usable id",
            ),
            (
                LinkInput {
                    label: Some("x".repeat(61)),
                    ..url("https://x.dev")
                },
                "at most 60",
            ),
        ] {
            let err = db.set_worktree_link(wt, input).unwrap_err();
            assert!(err.0.contains(needle), "{err} should mention {needle}");
        }
    }

    #[test]
    fn the_cap_refuses_a_new_link_but_not_an_update() {
        let (_d, db) = test_db();
        let wt = worktree(&db);
        for i in 0..MAX_LINKS_PER_WORKTREE {
            set(&db, wt, url(&format!("https://x.dev/{i}")));
        }
        assert_eq!(
            db.set_worktree_link(wt, url("https://x.dev/new"))
                .unwrap()
                .unwrap(),
            LinkSet::Full
        );
        set(
            &db,
            wt,
            LinkInput {
                tone: Some("danger".into()),
                ..url("https://x.dev/0")
            },
        );
    }

    #[test]
    fn remove_finds_a_link_by_id_or_by_url() {
        let (_d, db) = test_db();
        let wt = worktree(&db);
        set(
            &db,
            wt,
            LinkInput {
                kind: Some("preview".into()),
                ..url("https://p.example.com/")
            },
        );
        set(&db, wt, url("https://docs.example.com/spec"));
        assert!(
            db.remove_worktree_link(wt, "https://p.example.com")
                .unwrap()
                .is_some()
        );
        assert!(
            db.remove_worktree_link(wt, "https://DOCS.example.com/spec/#top")
                .unwrap()
                .is_some()
        );
        assert!(db.remove_worktree_link(wt, "nope").unwrap().is_none());
        assert!(db.worktree_links(wt).unwrap().is_empty());
    }

    #[test]
    fn links_keep_their_first_order_and_are_listed_per_repo() {
        let (_d, db) = test_db();
        let wt = worktree(&db);
        set(&db, wt, url("https://a.dev"));
        set(&db, wt, url("https://b.dev"));
        set(
            &db,
            wt,
            LinkInput {
                tone: Some("info".into()),
                ..url("https://a.dev")
            },
        );
        let by_repo = db.links_in_repo(Path::new("/repo")).unwrap();
        let ids: Vec<_> = by_repo[&wt].iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["https://a.dev", "https://b.dev"]);
        assert_eq!(db.clear_worktree_links(wt).unwrap(), 2);
    }

    #[test]
    fn a_missing_worktree_is_reported() {
        let (_d, db) = test_db();
        assert_eq!(
            db.set_worktree_link(999, url("https://a.dev"))
                .unwrap()
                .unwrap(),
            LinkSet::Missing
        );
    }
}
