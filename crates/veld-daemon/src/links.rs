//! `POST /api/ide/links/{set,list,remove}` — what `veld ide link` calls.
//!
//! The links themselves live in the database (`veld_core::db::links`) and reach
//! the window on the rail's `/api/repos` poll, as `WorktreeView::links`. These
//! routes only write and read them for the worktree the caller is in.
//!
//! **The caller's worktree is its terminal's first, its directory second** — the
//! reverse of `veld ide worktree new`. A link belongs to the workspace the agent
//! is working for, and an agent working across repositories runs `gh pr create`
//! in a sibling checkout: resolving by directory would pin that pull request to
//! the sibling (or refuse, when Veld has not imported it) instead of to the
//! worktree whose rail row the human is looking at.

use axum::Json;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use veld_core::db::{LinkInput, LinkSet};

use super::desktop::{ApiError, db_err, open_desktop_db, resolve_caller};
use super::file_pane::{coded, require_local_host};

/// Where the request comes from, as `veld ide worktree name` sends it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Caller {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkFields {
    url: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    tone: Option<String>,
    #[serde(default)]
    tooltip: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetRequest {
    #[serde(flatten)]
    caller: Caller,
    link: LinkFields,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveRequest {
    #[serde(flatten)]
    caller: Caller,
    /// An id, or a URL that finds the link filed under it.
    #[serde(default)]
    id: Option<String>,
    /// Every link of the worktree.
    #[serde(default)]
    all: bool,
}

/// The terminal's worktree when the session is one Veld has, else the one
/// containing `cwd`.
async fn owner(caller: &Caller) -> Result<i64, ApiError> {
    if let Some(session) = caller.session_id.as_deref().filter(|s| !s.is_empty())
        && let Some(id) = super::pty::session_worktree(session).await
    {
        return Ok(id);
    }
    resolve_caller(None, caller.cwd.as_deref()).await
}

/// The URLs of this worktree's live runs — what the top bar's run-URL control
/// already opens. Liveness is gated here, as `/api/environments` gates it.
fn run_urls(db: &veld_core::db::Db, path: &str) -> Vec<String> {
    let Ok(registry) = db.registry() else {
        return Vec::new();
    };
    let mut urls: Vec<String> = registry
        .projects
        .get(path)
        .into_iter()
        .flat_map(|entry| entry.runs.values())
        .filter(|run| run.status.is_live())
        .flat_map(|run| run.urls.values().cloned())
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

/// Scheme, host and port — the part of a URL a run owns.
fn origin(url: &str) -> String {
    let normalized = veld_core::db::normalize_link_url(url);
    let after_scheme = normalized.find("://").map_or(0, |i| i + 3);
    match normalized[after_scheme..].find('/') {
        Some(i) => normalized[..after_scheme + i].to_owned(),
        None => normalized,
    }
}

/// What the worktree's top bar shows besides its links: the project's own
/// entries and the live runs' URLs. Returned by `set` and `list` so the agent can
/// see what not to pin a second time.
fn also_in_top_bar(db: &veld_core::db::Db, id: i64, path: &str) -> serde_json::Value {
    serde_json::json!({
        "project_entries": super::desktop::top_bar_entries(db, id),
        "run_urls": run_urls(db, path),
    })
}

/// `{"id", "path", "branch"}` for the response, so the caller can see which worktree it
/// changed.
fn worktree_json(db: &veld_core::db::Db, id: i64) -> Result<serde_json::Value, ApiError> {
    let wt = db
        .get_worktree(id)
        .map_err(db_err)?
        .ok_or_else(|| coded(StatusCode::NOT_FOUND, "not_in_worktree", "no such worktree"))?;
    Ok(serde_json::json!({ "id": id, "path": wt.path, "branch": wt.branch }))
}

/// `POST /api/ide/links/set` — add a link to the caller's worktree, or update the
/// one with the same id. Answers `{worktree, link, created}`.
pub(super) async fn set(
    headers: HeaderMap,
    Json(body): Json<SetRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_local_host(&headers)?;
    let id = owner(&body.caller).await?;
    let f = body.link;
    let input = LinkInput {
        url: f.url,
        id: f.id,
        kind: f.kind,
        label: f.label,
        icon: f.icon,
        tone: f.tone,
        tooltip: f.tooltip,
    };
    crate::offload::blocking(move || {
        let db = open_desktop_db()?;
        let path = db
            .get_worktree(id)
            .map_err(db_err)?
            .map(|wt| wt.path)
            .unwrap_or_default();
        let runs = run_urls(&db, &path);
        if veld_core::ide::is_web_url(input.url.trim())
            && let Some(run) = runs.iter().find(|r| origin(r) == origin(&input.url))
        {
            return Err(coded(
                StatusCode::CONFLICT,
                "already_shown",
                format!(
                    "{run} is a URL of this worktree's own run, which the top bar already \
                     opens — do not pin it"
                ),
            ));
        }
        let stored = db
            .set_worktree_link(id, input)
            .map_err(|e| coded(StatusCode::BAD_REQUEST, "bad_link", e.0))?
            .map_err(db_err)?;
        match stored {
            LinkSet::Stored { link, created } => Ok(Json(serde_json::json!({
                "worktree": worktree_json(&db, id)?,
                "link": link,
                "created": created,
                "also_in_top_bar": also_in_top_bar(&db, id, &path),
            }))),
            LinkSet::Full => Err(coded(
                StatusCode::CONFLICT,
                "links_full",
                format!(
                    "this worktree already has {} links — remove one first (`veld ide link \
                     rm <id>`)",
                    veld_core::db::MAX_LINKS_PER_WORKTREE
                ),
            )),
            LinkSet::Missing => Err(coded(
                StatusCode::NOT_FOUND,
                "not_in_worktree",
                "no such worktree",
            )),
        }
    })
    .await
}

/// `POST /api/ide/links/list` — the caller's worktree's links, in the order they
/// were first set. Answers `{worktree, links}`.
pub(super) async fn list(
    headers: HeaderMap,
    Json(body): Json<Caller>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_local_host(&headers)?;
    let id = owner(&body).await?;
    crate::offload::blocking(move || {
        let db = open_desktop_db()?;
        let links = db.worktree_links(id).map_err(db_err)?;
        let worktree = worktree_json(&db, id)?;
        let path = worktree["path"].as_str().unwrap_or_default().to_owned();
        Ok(Json(serde_json::json!({
            "also_in_top_bar": also_in_top_bar(&db, id, &path),
            "worktree": worktree,
            "links": links,
        })))
    })
    .await
}

/// `POST /api/ide/links/remove` — one link by id or URL, or every link with
/// `all`. Answers `{worktree, removed: [id, ..]}`.
pub(super) async fn remove(
    headers: HeaderMap,
    Json(body): Json<RemoveRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_local_host(&headers)?;
    let target = body.id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if target.is_some() == body.all {
        return Err(coded(
            StatusCode::BAD_REQUEST,
            "bad_link",
            "name one link to remove (an id or a URL), or pass --all",
        ));
    }
    let target = target.map(str::to_owned);
    let id = owner(&body.caller).await?;
    crate::offload::blocking(move || {
        let db = open_desktop_db()?;
        let removed: Vec<String> = match target {
            Some(target) => match db.remove_worktree_link(id, &target).map_err(db_err)? {
                Some(link) => vec![link.id],
                None => {
                    return Err(coded(
                        StatusCode::NOT_FOUND,
                        "no_such_link",
                        format!(
                            "this worktree has no link {target:?} — `veld ide link list` shows \
                             the ids"
                        ),
                    ));
                }
            },
            None => {
                let ids = db
                    .worktree_links(id)
                    .map_err(db_err)?
                    .into_iter()
                    .map(|l| l.id)
                    .collect();
                db.clear_worktree_links(id).map_err(db_err)?;
                ids
            }
        };
        Ok(Json(serde_json::json!({
            "worktree": worktree_json(&db, id)?,
            "removed": removed,
        })))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenTargetRequest {
    url: String,
}

/// `POST /api/worktrees/{id}/open-target` — where a click on a link shown for
/// this worktree opens: `{"target": "pane" | "system"}`.
///
/// **The answer a link in that worktree's terminal gets**, from the same
/// `veld_core::ide::route_url` and the same inputs: `terminal.openUrlsInApp`, and
/// the exempt list (`browser.externalOrigins` plus the project's
/// `ide.externalOrigins`). A link an agent pins is a link it would otherwise have
/// printed, so it must not open somewhere else for being shown in the top bar. The
/// modifier-click override is the window's, as it is for a terminal link.
pub(super) async fn open_target(
    Path(id): Path<i64>,
    Json(body): Json<OpenTargetRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Some(web) = veld_core::ide::parse_web_url(&body.url) else {
        return Err(coded(
            StatusCode::BAD_REQUEST,
            "bad_link",
            "not an http(s) URL with a host",
        ));
    };
    crate::offload::blocking(move || {
        let db = open_desktop_db()?;
        let mut external = db.external_origins();
        external.extend(super::pty::project_external_origins(&db, id));
        let target = veld_core::ide::route_url(
            web.canonical.as_str(),
            db.terminal_open_urls_in_app(),
            &external,
        );
        Ok(Json(serde_json::json!({ "target": target })))
    })
    .await
}
