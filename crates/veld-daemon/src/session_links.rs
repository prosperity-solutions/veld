//! Session links: a URL an agent ends a pull request with, leading back to the
//! terminal — and the conversation in it — that the pull request came from.
//!
//! Three routes:
//!
//! - `POST /api/agent/link` — what `veld agent link` sends from inside a terminal.
//!   Mints (or reuses) the link for that terminal and answers with its URL. With `terminal.sessionLinks` off it answers `enabled: false`
//!   and no link; the CLI then tells the agent, in a sentence, to add none.
//! - `GET /session/{id}` — the page the link opens. Static: it carries nothing
//!   about the session, so the response is byte-identical for every id, and a
//!   link that is not this machine's still lands on a Veld page saying so.
//! - `POST /api/session-links/{id}/open` — what that page, and Veld Desktop on a
//!   `veld://session/<id>` link, call to find the session and send a window to it.
//!
//! # What a third party can do with a link
//!
//! The links are public — they sit in pull requests — and any site can navigate a
//! tab to one. The page's own script calls `open` from Veld's origin, so the CSRF
//! header does not tell that call apart from a real click, and nothing here
//! pretends it does. Two things bound it instead:
//!
//! - **Showing is gated on where the visitor came from.** A jump moves the user's
//!   Veld window, which the sending page can observe, so a site loading public
//!   links one by one could learn which pull request the visitor wrote. The page
//!   jumps on load only when it was reached from GitHub (`document.referrer`);
//!   from anywhere else it waits for a click on itself.
//! - **Starting** a process — resuming a closed conversation — needs
//!   `resume: true`, which the page sends only from a button press, and the page
//!   refuses to be framed, so neither press can be borrowed by another site.
//!   [`plan`] is the one place that turns `resume` into a [`ResumeRequest`].

use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::desktop::{ApiError, db_err, open_desktop_db};
use super::file_pane::{coded, require_local_host};
use super::ide::{ClientKind, ResumeRequest};

const PAGE_HTML: &str = include_str!("../assets/session-link.html");

pub fn routes() -> Router {
    Router::new()
        .route("/session/{id}", get(page))
        .route("/api/agent/link", post(mint))
        .route("/api/session-links/{id}/open", post(open))
}

async fn page() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        PAGE_HTML,
    )
        .into_response()
}

fn csrf(headers: &HeaderMap) -> Result<(), ApiError> {
    super::management::check_csrf(headers)
        .map_err(|_| coded(StatusCode::FORBIDDEN, "forbidden", "missing x-veld-request"))?;
    require_local_host(headers)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MintRequest {
    session_id: String,
}

/// `POST /api/agent/link` — the link for the terminal `session_id`.
async fn mint(
    headers: HeaderMap,
    Json(body): Json<MintRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    csrf(&headers)?;
    let worktree_id = super::pty::session_worktree(&body.session_id)
        .await
        .ok_or_else(|| {
            coded(
                StatusCode::NOT_FOUND,
                "not_in_worktree",
                "that terminal session is not one Veld has — run this inside a Veld terminal",
            )
        })?;
    let session = body.session_id;
    crate::offload::blocking(move || {
        let db = open_desktop_db()?;
        if !db.terminal_session_links() {
            return Ok(Json(serde_json::json!({ "enabled": false })));
        }
        let link = db.session_link_for(&session, worktree_id).map_err(db_err)?;
        Ok(Json(serde_json::json!({
            "enabled": true,
            "id": link.id,
            "url": link_url(&link.id),
            "conversation": link.conversation.is_some(),
        })))
    })
    .await
}

/// Where a link points: this instance's dashboard host, on the port Caddy serves
/// it on now. A link is minted for this machine only, so the one thing that can
/// move it is a later switch between privileged and unprivileged setup, which
/// changes the port. The setting's help and `veld agent skills ide` say so; a
/// redirector that would survive it would have to live somewhere public.
///
/// **A dev instance with no management host of its own links to its loopback
/// port.** `veld.localhost` is the installed daemon's, which has never heard of
/// this instance's links and would answer every one with "not found".
fn link_url(id: &str) -> String {
    let host = match veld_core::instance::management_host() {
        Some(host) => host,
        None if veld_core::db::Db::uses_installed_database() => {
            veld_core::instance::MANAGEMENT_HOST.to_owned()
        }
        None => return format!("{}/session/{id}", veld_core::instance::daemon_base()),
    };
    let (https, _) = super::pty::dashboard_ports();
    if https == 443 {
        format!("https://{host}/session/{id}")
    } else {
        format!("https://{host}:{https}/session/{id}")
    }
}

fn worktree_name(wt: &veld_core::db::WorktreeRecord) -> String {
    if wt.display_name.is_empty() {
        wt.branch.clone()
    } else {
        wt.display_name.clone()
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    /// Start the conversation again in a new tab when it is no longer running.
    /// Only ever sent from a button press on the link's page.
    #[serde(default)]
    resume: bool,
}

/// What a link resolved to, before a window is asked.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// The tab is open: bring it forward.
    Open(String),
    /// A project pane's conversation, no longer running in any tab, which can be
    /// started again: `spec` adopting `token`.
    Resumable { spec: String, token: String },
    /// Closed, with nothing to start again: a plain terminal, or a pane the
    /// project no longer declares as one that can resume. The worktree is all
    /// there is.
    Closed,
}

/// `POST /api/session-links/{id}/open`.
async fn open(
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<OpenRequest>>,
) -> Result<Response, ApiError> {
    csrf(&headers)?;
    let resume = body.map(|Json(b)| b.resume).unwrap_or(false);
    let found = crate::offload::blocking(move || {
        let db = open_desktop_db()?;
        let Some(link) = db.session_link(&id).map_err(db_err)? else {
            return Ok(None);
        };
        let Some(wt) = db.get_worktree(link.worktree_id).map_err(db_err)? else {
            return Ok(None);
        };
        let candidates = match &link.conversation {
            Some(c) => db
                .sessions_with_token(link.worktree_id, &c.token)
                .map_err(db_err)?,
            None => vec![link.session_id.clone()],
        };
        let can_resume = link
            .conversation
            .as_ref()
            .is_some_and(|c| super::desktop::pane_can_resume(&db, wt.clone(), &c.spec_id));
        Ok::<_, ApiError>(Some((link, wt, candidates, can_resume)))
    })
    .await?;
    let Some((link, wt, candidates, can_resume)) = found else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "found": false })),
        )
            .into_response());
    };
    let name = worktree_name(&wt);
    if !wt.trashed_at.is_empty() {
        return Ok(Json(serde_json::json!({
            "found": true,
            "worktree": name,
            "trashed": true,
        }))
        .into_response());
    }

    let mut live = None;
    for session in candidates {
        if super::pty::session_worktree(&session).await == Some(link.worktree_id) {
            live = Some(session);
            break;
        }
    }
    let target = resolve(live, link.conversation.as_ref().filter(|_| can_resume));

    let (session_id, resume_request, state) = plan(&target, resume);
    let delivered =
        super::ide::push_reveal(link.worktree_id, session_id.clone(), resume_request).await;
    // `repo` as well as `wt`: the IDE resolves `wt` only inside the project it is
    // showing, so a worktree of another project would otherwise land on that
    // project's main checkout.
    let mut ide_url = format!(
        "/ide?repo={}&wt={}",
        query_value(&wt.repo_root),
        link.worktree_id
    );
    if let Some(session) = &session_id {
        ide_url.push_str(&format!("&pane={session}"));
    }
    // Asked only when nobody took the jump: it is what tells the page to hand the
    // link to Veld Desktop (`veld://session/<id>`) rather than open the IDE in the
    // browser, and it costs a look at the disk.
    let desktop = match delivered {
        Some(_) => false,
        None => crate::offload::blocking(|| Ok::<_, ApiError>(desktop_installed())).await?,
    };
    Ok(Json(serde_json::json!({
        "found": true,
        "worktree": name,
        "state": state,
        "delivered": delivered.map(|k| match k {
            ClientKind::Electron => "electron",
            ClientKind::Browser => "browser",
        }),
        "desktop": desktop,
        "ide_url": ide_url,
    }))
    .into_response())
}

/// Whether a Veld Desktop that answers `veld://` is installed where Veld would
/// find it.
///
/// **The app's own declaration, not its presence.** The daemon and the app update
/// separately, and an app from before session links has no handler: handing it
/// the link would leave the page on "Opening Veld Desktop…" with nothing opening.
/// So this reads what the installed app registers — the bundle's
/// `CFBundleURLSchemes` on macOS, the `.desktop` entry's `x-scheme-handler` on
/// Linux — and anything it cannot read reads as `false`, which opens the IDE in
/// the browser instead: a working answer either way. A Linux AppImage saved
/// somewhere arbitrary is not found, for the same result, and a dev instance is
/// never handed to the installed app at all.
fn desktop_installed() -> bool {
    // The installed app talks to the installed daemon; handed a dev instance's
    // link, it would ask the wrong daemon and report the session missing.
    if !veld_core::db::Db::uses_installed_database() {
        return false;
    }
    if let Some((bundle, _)) = veld_core::setup::desktop_app_status() {
        return std::fs::read_to_string(bundle.join("Contents/Info.plist"))
            .is_ok_and(|plist| plist_declares_veld(&plist));
    }
    if veld_core::setup::desktop_app_linux().is_some() {
        return ["/usr/share/applications", "/usr/local/share/applications"]
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("veld"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .any(|entry| entry.contains("x-scheme-handler/veld"));
    }
    false
}

/// Whether an XML `Info.plist` registers the `veld` URL scheme — the
/// `CFBundleURLSchemes` entry electron-builder writes from `protocols`.
fn plist_declares_veld(plist: &str) -> bool {
    plist
        .split("<key>CFBundleURLSchemes</key>")
        .skip(1)
        .any(|rest| {
            rest.split("</array>")
                .next()
                .is_some_and(|schemes| schemes.contains("<string>veld</string>"))
        })
}

/// What to ask a window for, and the state the page is told: the tab to bring
/// forward, the conversation to start again — **only when `resume` was asked
/// for**, since that starts a process — and which of the page's states this is.
fn plan(target: &Target, resume: bool) -> (Option<String>, Option<ResumeRequest>, &'static str) {
    match target {
        Target::Open(session) => (Some(session.clone()), None, "open"),
        Target::Resumable { spec, token } if resume => (
            None,
            Some(ResumeRequest {
                spec: spec.clone(),
                session: token.clone(),
            }),
            "resumed",
        ),
        Target::Resumable { .. } => (None, None, "resumable"),
        Target::Closed => (None, None, "closed"),
    }
}

/// `value` percent-encoded for a query string: everything but the unreserved set.
fn query_value(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Which of the three a link is now. A live tab running the conversation wins
/// wherever it is — a resume moves a conversation to a new tab, and following the
/// tab the link was made in would then open nothing, or a conversation started
/// from scratch in it since.
fn resolve(live: Option<String>, conversation: Option<&veld_core::db::Conversation>) -> Target {
    match (live, conversation) {
        (Some(session), _) => Target::Open(session),
        (None, Some(c)) => Target::Resumable {
            spec: c.spec_id.clone(),
            token: c.token.clone(),
        },
        (None, None) => Target::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    #[test]
    fn a_live_tab_wins_then_the_conversation_then_nothing() {
        let c = veld_core::db::Conversation {
            token: "tok".into(),
            spec_id: "claude".into(),
        };
        assert_eq!(
            resolve(Some("tab-b".into()), Some(&c)),
            Target::Open("tab-b".into())
        );
        assert_eq!(
            resolve(None, Some(&c)),
            Target::Resumable {
                spec: "claude".into(),
                token: "tok".into()
            }
        );
        assert_eq!(
            resolve(Some("tab".into()), None),
            Target::Open("tab".into())
        );
        assert_eq!(resolve(None, None), Target::Closed);
    }

    #[test]
    fn a_repo_root_survives_the_query_string() {
        assert_eq!(
            query_value("/Users/Jo Doe/a&b"),
            "%2FUsers%2FJo%20Doe%2Fa%26b"
        );
        assert_eq!(query_value("plain-name_1.2~"), "plain-name_1.2~");
    }

    /// Opening a link never starts anything on its own: only an explicit `resume`
    /// turns an ended conversation into a [`ResumeRequest`].
    #[test]
    fn a_conversation_is_resumed_only_when_asked() {
        let ended = Target::Resumable {
            spec: "claude".into(),
            token: "tok".into(),
        };
        let (session, request, state) = plan(&ended, false);
        assert_eq!((session, state), (None, "resumable"));
        assert!(request.is_none(), "no process without resume: true");
        let (_, request, state) = plan(&ended, true);
        assert_eq!(state, "resumed");
        let request = request.unwrap();
        assert_eq!(
            (request.spec.as_str(), request.session.as_str()),
            ("claude", "tok")
        );
        for target in [Target::Open("tab".into()), Target::Closed] {
            assert!(plan(&target, true).1.is_none(), "{target:?}");
        }
    }

    /// Only an app that registers the scheme gets the hand-off; one from before
    /// session links (no URL types, or another app's scheme) does not.
    #[test]
    fn only_an_app_declaring_the_scheme_is_handed_the_link() {
        let with = "<dict><key>CFBundleURLTypes</key><array><dict>\
                    <key>CFBundleURLSchemes</key><array><string>veld</string></array>\
                    </dict></array></dict>";
        assert!(plist_declares_veld(with));
        assert!(!plist_declares_veld(
            "<dict><key>CFBundleName</key><string>veld</string></dict>"
        ));
        let other = "<key>CFBundleURLSchemes</key><array><string>zoom</string></array>\
                     <key>CFBundleName</key><string>veld</string>";
        assert!(!plist_declares_veld(other));
    }

    /// The page is the same bytes for every id, refuses to be framed, and sends no
    /// referrer — a link in a public pull request must not tell anybody anything.
    #[tokio::test]
    async fn the_page_is_static_and_cannot_be_framed() {
        let res = routes()
            .oneshot(
                Request::get(format!("/session/{}", "a".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(
            res.headers()[header::CONTENT_SECURITY_POLICY],
            "frame-ancestors 'none'"
        );
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        assert_eq!(&bytes[..], PAGE_HTML.as_bytes());
    }

    #[tokio::test]
    async fn both_posts_require_the_csrf_header() {
        for (uri, body) in [
            ("/api/agent/link", r#"{"session_id":"s"}"#),
            (
                "/api/session-links/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/open",
                "{}",
            ),
        ] {
            let res = routes()
                .oneshot(
                    Request::post(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
        }
    }
}
