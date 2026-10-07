//! What a worktree has changed, for the changed-files list and the file pane's
//! gutter markers and inline diff.
//!
//! Both answers are against **the merge-base with the default branch**, not
//! against `HEAD`: the question a reviewer has is "what does this branch do",
//! and a branch with three commits and a clean tree would otherwise show nothing.
//! Uncommitted and untracked work is included on top, because that is what an
//! agent has just written.
//!
//! Read-only git, run with `kill_on_drop` under a deadline (see
//! `desktop::git_cancellable`): a timed-out request stops its git rather than
//! leaving it walking a huge tree for nobody.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query},
    http::{HeaderMap, StatusCode},
    routing::{MethodRouter, get},
};
use serde::{Deserialize, Serialize};

use super::desktop::{ApiError, db_err, err, git_cancellable, git_raw_cancellable};
use super::management::{check_csrf, open_db};

/// How long one answer may take. Generous for a cold cache on a big repository,
/// short of a spinner nobody believes.
const BUDGET: Duration = Duration::from_secs(10);

/// Most rows the list returns. A branch that touches more than this is not
/// reviewed file by file; the answer says it was cut short.
const MAX_ROWS: usize = 2000;

/// Every route checks CSRF despite being GETs: each spawns git on a worktree id
/// anyone can guess — the reasoning of `files::list_viewable`. And `Host`, like
/// `file_pane`'s routes: a path list, a diff and a base text are file contents.
pub fn routes() -> Router {
    route_table()
        .into_iter()
        .fold(Router::new(), |router, (path, method)| {
            router.route(path, method)
        })
}

/// Every route [`routes`] mounts, as a table — see `file_pane::route_table`, which
/// this mirrors for the same reason: the Host and CSRF tests walk it.
fn route_table() -> [(&'static str, MethodRouter); 3] {
    [
        ("/api/worktrees/{id}/changes", get(changes)),
        ("/api/worktrees/{id}/file-changes", get(file_changes)),
        ("/api/worktrees/{id}/file-base", get(file_base)),
    ]
}

/// The checkout path of worktree `id`, read on the blocking pool.
async fn worktree_dir(id: i64) -> Result<PathBuf, ApiError> {
    crate::offload::blocking(move || {
        let db = open_db().map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
        db.get_worktree(id)
            .map_err(db_err)?
            .map(|wt| PathBuf::from(wt.path))
            .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such worktree"))
    })
    .await
}

/// The default branch, as a ref git can resolve: `origin/HEAD`'s target, else
/// the first of the usual names that exists.
async fn default_ref(dir: &Path) -> Option<String> {
    if let Ok(full) = git_cancellable(
        dir,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .await
        && let Some(short) = full.strip_prefix("refs/remotes/")
        && !short.is_empty()
    {
        return Some(short.to_owned());
    }
    for candidate in ["origin/main", "main", "origin/master", "master"] {
        let spec = format!("{candidate}^{{commit}}");
        if git_cancellable(dir, &["rev-parse", "--verify", "--quiet", &spec])
            .await
            .is_ok()
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

/// The commit a branch's changes are measured from, and the ref it came from.
async fn merge_base(dir: &Path) -> Option<(String, String)> {
    let base_ref = default_ref(dir).await?;
    let sha = git_cancellable(dir, &["merge-base", "HEAD", &base_ref])
        .await
        .ok()
        .filter(|s| !s.is_empty())?;
    Some((sha, base_ref))
}

/// What to diff the working tree against: the merge-base, else `HEAD`, else
/// nothing (a repository with no commits, where everything is untracked).
async fn diff_base(dir: &Path) -> (Option<(String, String)>, Option<String>) {
    let base = merge_base(dir).await;
    let against = match &base {
        Some((sha, _)) => Some(sha.clone()),
        None => git_cancellable(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .await
            .ok()
            .map(|_| "HEAD".to_owned()),
    };
    (base, against)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Change {
    path: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_path: Option<String>,
}

/// Parse `git diff --name-status -z` output.
///
/// Records are `<status>\0<path>\0`, except a rename or copy, which carries two
/// paths: `R087\0<old>\0<new>\0`. A copy is reported as `added` — the list has no
/// row kind for it, and the new file is what there is to read.
fn parse_name_status(raw: &str) -> Vec<Change> {
    let mut out = Vec::new();
    let mut fields = raw.split('\0').filter(|f| !f.is_empty());
    while let Some(code) = fields.next() {
        let Some(first) = fields.next() else { break };
        let (status, old_path, path) = match code.as_bytes().first() {
            Some(b'R') | Some(b'C') => {
                let Some(new) = fields.next() else { break };
                if code.starts_with('R') {
                    ("renamed", Some(first.to_owned()), new)
                } else {
                    ("added", None, new)
                }
            }
            Some(b'A') => ("added", None, first),
            Some(b'D') => ("deleted", None, first),
            // `M`, `T` (type change) and `U` (unmerged) all read as "modified" —
            // there is something different to look at in each.
            _ => ("modified", None, first),
        };
        out.push(Change {
            path: path.to_owned(),
            status,
            old_path,
        });
    }
    out
}

/// `GET /api/worktrees/{id}/changes` — files this branch changed, plus uncommitted
/// and untracked work.
async fn changes(
    headers: HeaderMap,
    UrlPath(id): UrlPath<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    super::file_pane::require_local_host(&headers)?;
    let dir = worktree_dir(id).await?;
    tokio::time::timeout(BUDGET, list_changes(&dir))
        .await
        .map_err(|_| err(StatusCode::GATEWAY_TIMEOUT, "git took too long to answer"))?
        .map(Json)
}

async fn list_changes(dir: &Path) -> Result<serde_json::Value, ApiError> {
    let (base, against) = diff_base(dir).await;
    let mut files = match &against {
        Some(rev) => {
            let raw = git_raw_cancellable(
                dir,
                &[
                    "diff",
                    "--name-status",
                    "-z",
                    "-M",
                    "--no-ext-diff",
                    rev,
                    "--",
                ],
            )
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            parse_name_status(&String::from_utf8_lossy(&raw))
        }
        None => Vec::new(),
    };
    let untracked = git_raw_cancellable(dir, &["ls-files", "--others", "--exclude-standard", "-z"])
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    for path in String::from_utf8_lossy(&untracked)
        .split('\0')
        .filter(|p| !p.is_empty())
    {
        files.push(Change {
            path: path.to_owned(),
            status: "untracked",
            old_path: None,
        });
    }
    let truncated = files.len() > MAX_ROWS;
    files.truncate(MAX_ROWS);
    Ok(serde_json::json!({
        "base": base.as_ref().map(|(sha, _)| sha),
        "baseRef": base.as_ref().map(|(_, r)| r),
        "files": files,
        "truncated": truncated,
    }))
}

/// Which lines of a file changed, as gutter markers want them.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
struct LineChanges {
    /// Inclusive 1-based ranges of new-side lines that did not exist before.
    added: Vec<[u32; 2]>,
    /// Inclusive 1-based ranges of new-side lines that replaced old ones.
    modified: Vec<[u32; 2]>,
    /// Lines were removed **after** this new-side line; `0` means above line 1.
    deleted: Vec<u32>,
}

/// One `-U0` range, `start,count` or a bare `start` (count 1).
fn hunk_range(spec: &str) -> Option<(u32, u32)> {
    match spec.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((spec.parse().ok()?, 1)),
    }
}

/// Parse the hunk headers of `git diff -U0`.
///
/// With no context, each header says everything: an empty old side is an
/// addition, an empty new side a deletion, and both non-empty a replacement. The
/// body lines are never read — with `-U0` every one starts with `+`, `-` or `\`,
/// so none can be mistaken for a header.
fn parse_hunks(diff: &str) -> LineChanges {
    let mut out = LineChanges::default();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ -") else {
            continue;
        };
        let mut parts = rest.split(' ');
        let (Some(old), Some(new)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Some((_, old_count)), Some((start, count))) =
            (hunk_range(old), new.strip_prefix('+').and_then(hunk_range))
        else {
            continue;
        };
        if count == 0 {
            out.deleted.push(start);
        } else if old_count == 0 {
            out.added.push([start, start + count - 1]);
        } else {
            out.modified.push([start, start + count - 1]);
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct FileChangesQuery {
    path: String,
}

/// `GET /api/worktrees/{id}/file-changes?path=<rel>` — gutter markers for one file.
///
/// Empty for anything git does not track or that is not a file in the worktree —
/// a file pane showing a file outside it asks too, and "no markers" is the honest
/// answer rather than an error the pane has to handle.
async fn file_changes(
    headers: HeaderMap,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<FileChangesQuery>,
) -> Result<Json<LineChanges>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    super::file_pane::require_local_host(&headers)?;
    let dir = worktree_dir(id).await?;
    let Some(rel) = super::files::normalize_relative(&q.path) else {
        return Ok(Json(LineChanges::default()));
    };
    tokio::time::timeout(BUDGET, line_changes(&dir, &rel))
        .await
        .map_err(|_| err(StatusCode::GATEWAY_TIMEOUT, "git took too long to answer"))
        .map(Json)
}

async fn line_changes(dir: &Path, rel: &str) -> LineChanges {
    // `--literal-pathspecs`: the path is a file name, and `:(glob)*` or `*` in one
    // must not turn into a pattern.
    let tracked = git_raw_cancellable(dir, &["--literal-pathspecs", "ls-files", "-z", "--", rel])
        .await
        .is_ok_and(|out| !out.is_empty());
    if !tracked {
        let untracked = git_raw_cancellable(
            dir,
            &[
                "--literal-pathspecs",
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                rel,
            ],
        )
        .await
        .is_ok_and(|out| !out.is_empty());
        return if untracked {
            whole_file_added(dir, rel).await
        } else {
            LineChanges::default()
        };
    }
    let (_, against) = diff_base(dir).await;
    let Some(rev) = against else {
        return LineChanges::default();
    };
    match git_raw_cancellable(
        dir,
        &[
            "--literal-pathspecs",
            "diff",
            "-U0",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &rev,
            "--",
            rel,
        ],
    )
    .await
    {
        Ok(raw) => parse_hunks(&String::from_utf8_lossy(&raw)),
        Err(_) => LineChanges::default(),
    }
}

/// An untracked file is all added — if it is text the pane could show at all.
///
/// Through `file_pane::resolve_readable` (no folders, no grants) before anything is
/// read, as `file-base` is: an untracked symlink to `~/.ssh/id_rsa.txt`, or an
/// untracked `.env.md`, is a file whose line count is not this route's to report.
/// And under a read slot, because this reads the whole file like `file-text` does.
async fn whole_file_added(dir: &Path, rel: &str) -> LineChanges {
    let (root, requested) = (dir.to_path_buf(), rel.to_owned());
    let Some(full) = crate::offload::blocking(move || {
        super::file_pane::resolve_readable(&root, &requested, &[], |_| false)
            .map(|r| r.full().to_path_buf())
    })
    .await
    else {
        return LineChanges::default();
    };
    let Ok(meta) = tokio::fs::metadata(&full).await else {
        return LineChanges::default();
    };
    if !meta.is_file() || meta.len() > super::file_pane::MAX_TEXT_BYTES {
        return LineChanges::default();
    }
    let Ok(_slot) = super::files::READ_SLOTS.acquire().await else {
        return LineChanges::default();
    };
    let Ok(bytes) = tokio::fs::read(&full).await else {
        return LineChanges::default();
    };
    if veld_core::files::looks_binary(&bytes) {
        return LineChanges::default();
    }
    let lines = bytes.split(|b| *b == b'\n').count() - usize::from(bytes.ends_with(b"\n"));
    let lines = u32::try_from(lines).unwrap_or(u32::MAX);
    LineChanges {
        added: if lines == 0 { vec![] } else { vec![[1, lines]] },
        ..LineChanges::default()
    }
}

/// `GET /api/worktrees/{id}/file-base?path=<rel>` — a file's text at the base the
/// gutter markers are measured from, for the file pane's "Show deletions" diff.
///
/// This hands out file **content**, so it is gated like `file-text` rather than
/// like its siblings here: the current file must be one a pane of this worktree
/// could read from inside it (`file_pane::resolve_readable` with no folders and
/// no grants — a file outside the worktree has no base), the same size cap, the
/// same binary refusal, the same read slots. Every refusal is the one 404. The
/// blob is named by the requested path, not the resolved one, so it matches the
/// `file-changes` markers the pane already shows.
///
/// The text is in **working-tree form** (`cat-file --filters`): the pane diffs it
/// against the file on disk, so a CRLF checkout or an LFS-tracked file must come
/// back the way a checkout would write it, or every line differs.
///
/// `text` is null when there is nothing to diff against — no commits, an
/// untracked file, or one that did not exist at the base — and the pane then
/// shows the file as it does today, which is the honest picture of "all added".
async fn file_base(
    headers: HeaderMap,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<FileChangesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_csrf(&headers)
        .map_err(|_| err(StatusCode::FORBIDDEN, "missing X-Veld-Request header"))?;
    super::file_pane::require_local_host(&headers)?;
    let dir = worktree_dir(id).await?;
    let root = dir.clone();
    let rel = crate::offload::blocking(move || base_readable(&root, &q.path))
        .await
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such file"))?;
    tokio::time::timeout(BUDGET, base_text(&dir, &rel))
        .await
        .map_err(|_| err(StatusCode::GATEWAY_TIMEOUT, "git took too long to answer"))?
        .map(Json)
}

/// The worktree-relative path whose base `file-base` may hand out, or `None` for
/// every refusal. Touches the filesystem (it resolves symlinks), so it runs on the
/// blocking pool.
fn base_readable(root: &Path, requested: &str) -> Option<String> {
    let rel = super::files::normalize_relative(requested)?;
    super::file_pane::resolve_readable(root, &rel, &[], |_| false)?;
    Some(rel)
}

async fn base_text(dir: &Path, rel: &str) -> Result<serde_json::Value, ApiError> {
    let (_, against) = diff_base(dir).await;
    let Some(rev) = against else {
        return Ok(serde_json::json!({ "base": null, "text": null }));
    };
    let none = serde_json::json!({ "base": rev, "text": null });
    // Untracked is "all added" in `line_changes`, so it has no base here either —
    // even when a file of that name existed at the merge-base and was deleted since.
    let tracked = git_raw_cancellable(dir, &["--literal-pathspecs", "ls-files", "-z", "--", rel])
        .await
        .is_ok_and(|out| !out.is_empty());
    if !tracked {
        return Ok(none);
    }
    // `<rev>:./<rel>` is a revision expression, not a pathspec, so no glob magic
    // applies; `./` makes it relative to the worktree rather than the repository
    // top, and the leading sha means it can never read as an option.
    let spec = format!("{rev}:./{rel}");
    // Size first, from the object header, so a huge blob is refused before git
    // pours it into memory. Failing here means the path is not at the base.
    let Ok(size) = git_cancellable(dir, &["cat-file", "-s", &spec]).await else {
        return Ok(none);
    };
    if size
        .parse::<u64>()
        .map_or(true, |n| n > super::file_pane::MAX_TEXT_BYTES)
    {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the base version of this file is too large to show",
        ));
    }
    let too_large = || {
        err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the base version of this file is too large to show",
        )
    };
    let _slot = super::files::READ_SLOTS
        .acquire()
        .await
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "shutting down"))?;
    // `cat-file blob` fails on a tree or a submodule's commit, which is "no base".
    let Ok(raw) = git_raw_cancellable(dir, &["cat-file", "blob", &spec]).await else {
        return Ok(none);
    };
    // The size above is the *stored* blob's. For an LFS pointer that is ~130 bytes
    // whatever the file is, so the pointer's own `size` is checked before the
    // smudge below pours the real object into memory (or fetches it).
    if lfs_pointer_size(&raw).is_some_and(|n| n > super::file_pane::MAX_TEXT_BYTES) {
        return Err(too_large());
    }
    // **Working-tree form**, to match the file on disk the pane diffs it against:
    // eol conversion and smudge filters applied, as a checkout would. This runs
    // the repository's configured smudge drivers — not new exposure: the drivers
    // come from git config (`.gitattributes` can only name one), the same ones
    // `git worktree add` and `apply_captured`'s `read-tree -u` already run here,
    // and `git diff`/`git status` already run their clean halves. A filter that
    // fails is "no base" rather than an error, like a missing path.
    let Ok(bytes) = git_raw_cancellable(dir, &["cat-file", "--filters", &spec]).await else {
        return Ok(none);
    };
    // Again after filtering: CRLF conversion can grow a file under the cap past it.
    if bytes.len() as u64 > super::file_pane::MAX_TEXT_BYTES {
        return Err(too_large());
    }
    if veld_core::files::looks_binary(&bytes) {
        return Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "the base version of this file looks binary",
        ));
    }
    // Lossy for the reason `file_pane::file_text` gives.
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    Ok(serde_json::json!({ "base": rev, "text": text }))
}

/// The object size a Git LFS pointer file declares, or `None` for anything else.
///
/// Only the spec's own shape counts — a first line naming the LFS spec and a
/// `size <n>` line — so a text file that merely mentions LFS is not mistaken for one.
fn lfs_pointer_size(blob: &[u8]) -> Option<u64> {
    // The spec caps a pointer at 1024 bytes.
    if blob.len() > 1024 {
        return None;
    }
    let text = std::str::from_utf8(blob).ok()?;
    let mut lines = text.lines();
    if !lines
        .next()?
        .starts_with("version https://git-lfs.github.com/spec/")
    {
        return None;
    }
    lines.find_map(|l| l.strip_prefix("size ")?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_status_reads_renames_as_two_paths_and_everything_else_as_one() {
        let raw = "M\0src/a.rs\0R087\0old name.md\0new name.md\0A\0b.md\0D\0gone.txt\0C100\0x\0y\0";
        let got = parse_name_status(raw);
        let want = vec![
            Change {
                path: "src/a.rs".into(),
                status: "modified",
                old_path: None,
            },
            Change {
                path: "new name.md".into(),
                status: "renamed",
                old_path: Some("old name.md".into()),
            },
            Change {
                path: "b.md".into(),
                status: "added",
                old_path: None,
            },
            Change {
                path: "gone.txt".into(),
                status: "deleted",
                old_path: None,
            },
            Change {
                path: "y".into(),
                status: "added",
                old_path: None,
            },
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn zero_context_hunks_become_added_modified_and_deleted() {
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n\
                    @@ -0,0 +1,2 @@\n+a\n+b\n\
                    @@ -5 +7 @@ fn x\n-old\n+new\n\
                    @@ -9,3 +10,0 @@\n-x\n-y\n-z\n\
                    @@ -20,2 +20,4 @@\n-p\n-q\n+1\n+2\n+3\n+4\n";
        assert_eq!(
            parse_hunks(diff),
            LineChanges {
                added: vec![[1, 2]],
                modified: vec![[7, 7], [20, 23]],
                deleted: vec![10],
            }
        );
    }

    async fn run_git(dir: &Path, args: &[&str]) {
        let out = tokio::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// Against real git: a branch's commits, an uncommitted edit and an untracked
    /// file all show, measured from the merge-base with `main`.
    #[tokio::test]
    async fn a_branch_is_measured_from_its_merge_base_with_main() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        run_git(dir, &["init", "-q", "-b", "main"]).await;
        run_git(dir, &["config", "user.email", "t@example.com"]).await;
        run_git(dir, &["config", "user.name", "t"]).await;
        std::fs::write(dir.join("keep.md"), "1\n2\n3\n").unwrap();
        std::fs::write(dir.join("edit.md"), "a\nb\nc\n").unwrap();
        run_git(dir, &["add", "."]).await;
        run_git(dir, &["commit", "-q", "-m", "base"]).await;
        run_git(dir, &["checkout", "-q", "-b", "feat"]).await;
        std::fs::write(dir.join("new.md"), "x\n").unwrap();
        run_git(dir, &["add", "new.md"]).await;
        run_git(dir, &["commit", "-q", "-m", "feat"]).await;
        std::fs::write(dir.join("edit.md"), "a\nB\nc\nd\n").unwrap();
        std::fs::write(dir.join("loose.md"), "l1\nl2\n").unwrap();

        let body = list_changes(dir).await.unwrap();
        assert!(body["base"].is_string(), "{body}");
        assert_eq!(body["baseRef"], "main");
        let rows: Vec<(String, String)> = body["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_owned(),
                    f["status"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        for want in [
            ("edit.md", "modified"),
            ("new.md", "added"),
            ("loose.md", "untracked"),
        ] {
            assert!(
                rows.contains(&(want.0.to_owned(), want.1.to_owned())),
                "{want:?} missing from {rows:?}"
            );
        }
        assert!(!rows.iter().any(|(p, _)| p == "keep.md"), "{rows:?}");

        assert_eq!(
            line_changes(dir, "edit.md").await,
            LineChanges {
                added: vec![[4, 4]],
                modified: vec![[2, 2]],
                deleted: vec![],
            }
        );
        assert_eq!(
            line_changes(dir, "loose.md").await.added,
            vec![[1, 2]],
            "untracked is all added"
        );
        assert_eq!(line_changes(dir, "keep.md").await, LineChanges::default());
        assert_eq!(line_changes(dir, "nope.md").await, LineChanges::default());

        let edit = base_text(dir, "edit.md").await.unwrap();
        assert_eq!(edit["base"], body["base"], "the gutter's base, not HEAD");
        assert_eq!(edit["text"], "a\nb\nc\n");
        for no_base in ["new.md", "loose.md"] {
            let got = base_text(dir, no_base).await.unwrap();
            assert!(got["text"].is_null(), "{no_base}: {got}");
            assert!(got["base"].is_string(), "{no_base}: {got}");
        }
    }

    /// The old text of a file is refused exactly where its current text would be:
    /// secrets, escapes, unknown kinds and anything outside the worktree.
    #[test]
    fn a_base_is_only_offered_for_a_file_a_pane_could_read() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("wt");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        std::fs::write(root.join("deploy.pem"), "key\n").unwrap();
        std::fs::write(tmp.path().join("outside.md"), "x\n").unwrap();
        assert_eq!(
            base_readable(&root, "./src/a.rs").as_deref(),
            Some("src/a.rs")
        );
        for refused in [
            ".env",
            "deploy.pem",
            "../outside.md",
            "src/../.env",
            "/etc/hosts",
            "src",
            "gone.md",
        ] {
            assert_eq!(base_readable(&root, refused), None, "{refused}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(".env", root.join("notes.md")).unwrap();
            assert_eq!(
                base_readable(&root, "notes.md"),
                None,
                "judged by its target"
            );
        }
    }

    /// An untracked file is counted only if a pane could read it: a symlink out of
    /// the worktree and a secret both answer "no markers".
    #[tokio::test]
    async fn an_untracked_file_is_counted_only_if_a_pane_could_read_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("wt");
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q", "-b", "main"]).await;
        std::fs::write(dir.join("loose.md"), "l1\nl2\n").unwrap();
        std::fs::write(dir.join(".env.md"), "SECRET=1\n").unwrap();
        std::fs::write(tmp.path().join("outside.md"), "o1\no2\no3\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path().join("outside.md"), dir.join("link.md")).unwrap();

        assert_eq!(whole_file_added(&dir, "loose.md").await.added, vec![[1, 2]]);
        assert_eq!(
            whole_file_added(&dir, ".env.md").await,
            LineChanges::default()
        );
        #[cfg(unix)]
        assert_eq!(
            whole_file_added(&dir, "link.md").await,
            LineChanges::default(),
            "a link out of the worktree is not counted"
        );
    }

    /// The base comes back as a checkout would write it, so a CRLF file diffs
    /// against its old text line for line rather than on every line.
    #[tokio::test]
    async fn the_base_is_in_working_tree_form() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        run_git(dir, &["init", "-q", "-b", "main"]).await;
        run_git(dir, &["config", "user.email", "t@example.com"]).await;
        run_git(dir, &["config", "user.name", "t"]).await;
        std::fs::write(dir.join(".gitattributes"), "*.txt eol=crlf\n").unwrap();
        std::fs::write(dir.join("win.txt"), "a\r\nb\r\n").unwrap();
        run_git(dir, &["add", "."]).await;
        run_git(dir, &["commit", "-q", "-m", "base"]).await;
        let got = base_text(dir, "win.txt").await.unwrap();
        assert_eq!(got["text"], "a\r\nb\r\n");
    }

    #[test]
    fn an_lfs_pointer_says_how_big_its_object_is() {
        let pointer = b"version https://git-lfs.github.com/spec/v1\n\
                        oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                        size 12345678\n";
        assert_eq!(lfs_pointer_size(pointer), Some(12_345_678));
        assert_eq!(lfs_pointer_size(b"size 12\n"), None, "no spec line");
        assert_eq!(
            lfs_pointer_size(b"see version https://git-lfs.github.com/spec/v1\nsize 9\n"),
            None
        );
    }

    /// A URI each of [`route_table`]'s routes (all GETs) answers. A route with no
    /// sample here fails the two tests below.
    fn sample(route: &str) -> &'static str {
        match route {
            "/api/worktrees/{id}/changes" => "/api/worktrees/1/changes",
            "/api/worktrees/{id}/file-changes" => "/api/worktrees/1/file-changes?path=a.md",
            "/api/worktrees/{id}/file-base" => "/api/worktrees/1/file-base?path=a.md",
            other => panic!("new route {other}: give it a sample request here"),
        }
    }

    #[tokio::test]
    async fn every_route_here_refuses_a_host_that_is_not_local() {
        use tower::ServiceExt;
        for (route, _) in route_table() {
            let uri = sample(route);
            let res = routes()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(uri)
                        .header("X-Veld-Request", "1")
                        .header("Host", "evil.example")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
        }
    }

    #[tokio::test]
    async fn every_route_here_requires_the_csrf_header() {
        use tower::ServiceExt;
        for (route, _) in route_table() {
            let uri = sample(route);
            let res = routes()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(uri)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
        }
    }
}
