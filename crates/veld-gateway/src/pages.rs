//! Branded gateway pages — the small set of HTML responses the gateway
//! serves itself (apex index, not-found, and the login shell used by
//! [`crate::auth`]).
//!
//! All pages follow the Veld brand (docs/branding.md): the dark token
//! palette and embedded wordmark of the daemon's management UI
//! (`crates/veld-daemon/assets/management-ui.html`), fully self-contained
//! (inline CSS, data-URI favicon, no external assets) so they render under
//! any CSP and leak no requests. The served bytes carry no share metadata,
//! no counts — nothing an anonymous viewer can enumerate; the only dynamic
//! behavior is client-side, from tab-local state the viewer's own tab minted
//! earlier (see [`SHARE_SEEN_KEY`]).

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

/// The `veld.` wordmark (same paths as the management UI header). Colored by
/// the shell CSS: letters `var(--text)`, the dot `var(--accent)`.
const WORDMARK_SVG: &str = r#"<svg class="wordmark" viewBox="0 0 3837 1463" fill="none" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="veld">
<path d="M383 1463L0 423H183L470 1278H474L762 423H943L561 1463H383Z"/>
<path d="M1507 1463C1407.67 1463 1322.17 1441 1250.5 1397C1178.83 1353 1123.83 1290.83 1085.5 1210.5C1047.17 1130.17 1028 1035.67 1028 927V926C1028 818.667 1047.33 724.167 1086 642.5C1124.67 560.833 1179 497.167 1249 451.5C1319 405.833 1401.33 383 1496 383C1590.67 383 1672.17 404.833 1740.5 448.5C1808.83 492.167 1861.33 553.333 1898 632C1934.67 710.667 1953 802 1953 906V970H1115V834H1867L1779 960V893C1779 812.333 1766.83 745.667 1742.5 693C1718.17 640.333 1684.67 601.167 1642 575.5C1599.33 549.833 1550.33 537 1495 537C1439.67 537 1390 550.5 1346 577.5C1302 604.5 1267.33 644.5 1242 697.5C1216.67 750.5 1204 815.667 1204 893V960C1204 1033.33 1216.5 1096 1241.5 1148C1266.5 1200 1302 1239.83 1348 1267.5C1394 1295.17 1448.33 1309 1511 1309C1555 1309 1594.33 1302.33 1629 1289C1663.67 1275.67 1692.67 1257.33 1716 1234C1739.33 1210.67 1756 1184 1766 1154L1769 1145H1940L1938 1155C1929.33 1197.67 1912.83 1237.67 1888.5 1275C1864.17 1312.33 1833 1345.17 1795 1373.5C1757 1401.83 1713.67 1423.83 1665 1439.5C1616.33 1455.17 1563.67 1463 1507 1463Z"/>
<path d="M2137 1463V20H2311V1463H2137Z"/>
<path d="M2937 1463C2848.33 1463 2770.83 1440.83 2704.5 1396.5C2638.17 1352.17 2586.67 1289.5 2550 1208.5C2513.33 1127.5 2495 1032.33 2495 923V922C2495 812.667 2513.5 717.667 2550.5 637C2587.5 556.333 2639 493.833 2705 449.5C2771 405.167 2847.33 383 2934 383C2983.33 383 3029.17 390.833 3071.5 406.5C3113.83 422.167 3151.67 444.5 3185 473.5C3218.33 502.5 3245.67 537 3267 577H3271V0H3445V1443H3271V1267H3267C3245.67 1307.67 3218.67 1342.5 3186 1371.5C3153.33 1400.5 3116.17 1423 3074.5 1439C3032.83 1455 2987 1463 2937 1463ZM2971 1309C3029.67 1309 3081.67 1293 3127 1261C3172.33 1229 3207.83 1184 3233.5 1126C3259.17 1068 3272 1000.33 3272 923V922C3272 844.667 3259 777.167 3233 719.5C3207 661.833 3171.5 617 3126.5 585C3081.5 553 3029.67 537 2971 537C2909.67 537 2856.67 552.667 2812 584C2767.33 615.333 2733 659.667 2709 717C2685 774.333 2673 842.667 2673 922V923C2673 1002.33 2685 1071 2709 1129C2733 1187 2767.33 1231.5 2812 1262.5C2856.67 1293.5 2909.67 1309 2971 1309Z"/>
<path d="M3757 1463C3801.18 1463 3837 1427.18 3837 1383C3837 1338.82 3801.18 1303 3757 1303C3712.82 1303 3677 1338.82 3677 1383C3677 1427.18 3712.82 1463 3757 1463Z"/>
</svg>"#;

/// Page skeleton. Assembled by ordered literal replacement (`{title}`, then
/// `{wordmark}`, then `{body}`) rather than `format!` so the CSS braces need
/// no escaping. Both
/// substituted values are trusted constants at every call site; anything
/// viewer-controlled is escaped (braces included) before it can reach a page
/// — see `html_escape` in `auth.rs`.
const SHELL: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 48 48'><path d='M40 0H8C3.58 0 0 3.58 0 8V40C0 44.42 3.58 48 8 48H40C44.42 48 48 44.42 48 40V8C48 3.58 44.42 0 40 0Z' fill='%230f1117'/><path d='M21.1 36L11.9 12H16.3L23.6 31.8H23.7L31 12H35.4L26.2 36H21.1Z' fill='white'/><path d='M32.5 36C33.88 36 35 34.88 35 33.5C35 32.12 33.88 31 32.5 31C31.12 31 30 32.12 30 33.5C30 34.88 31.12 36 32.5 36Z' fill='%23C4F56A'/></svg>">
<title>{title}</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
:root{
  --bg:#0f1117;--surface:#181a24;--surface2:#1f2233;--border:#2a2d3e;
  --text:#e0e0e6;--text2:#8b8fa3;
  --accent:#C4F56A;--blue:#6c8cff;--red:#f06060;--dim:#555870;
}
html{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;font-size:15px}
body{min-height:100vh;display:flex;align-items:center;justify-content:center;background:var(--bg);color:var(--text);padding:24px}
.card{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:36px 40px;max-width:26rem;width:100%}
header{display:flex;align-items:baseline;gap:10px;margin-bottom:22px}
.wordmark{height:20px;width:auto;flex-shrink:0;position:relative;top:2px}
.wordmark path{fill:var(--text)}
.wordmark path:last-child{fill:var(--accent)}
.sep{color:var(--dim);font-weight:300;font-size:18px}
.subtitle{color:var(--text2);font-size:14px}
h1{font-size:1.1rem;font-weight:600;margin:0 0 10px}
p{color:var(--text2);font-size:.9rem;line-height:1.6;margin:0 0 12px}
p:last-child{margin-bottom:0}
a{color:var(--blue);text-decoration:none}
a:hover{text-decoration:underline}
.err{color:var(--red)}
.hint{color:var(--dim);font-size:.78rem;margin:14px 0 0}
input[type=password]{width:100%;padding:.55rem .75rem;font-size:1rem;color:var(--text);background:var(--surface2);border:1px solid var(--border);border-radius:8px;margin-bottom:12px;font-family:inherit}
input[type=password]:focus{outline:none;border-color:var(--accent)}
button{width:100%;padding:.6rem;font-size:.95rem;font-weight:600;border:0;border-radius:8px;background:var(--accent);color:var(--bg);cursor:pointer;font-family:inherit}
button:hover{filter:brightness(1.06)}
</style>
</head>
<body>
<main class="card">
<header>{wordmark}<span class="sep">/</span><span class="subtitle">Gateway</span></header>
{body}
</main>
</body>
</html>
"#;

/// Wrap `body` in the branded page shell. Both arguments must be trusted
/// HTML/text — no escaping happens here. Run anything viewer-controlled
/// through [`html_escape`] before it gets anywhere near a page.
pub fn shell(title: &str, body: &str) -> String {
    SHELL
        .replace("{title}", title)
        .replace("{wordmark}", WORDMARK_SVG)
        .replace("{body}", body)
}

/// sessionStorage key marking "this tab saw this share's gateway pages while
/// it was registered". Set by the gateway-GENERATED pages that only render
/// with a live registration (the login page, the viewer-facing error pages)
/// and read by the share 404, which then swaps its copy from "Share not
/// found" to "Sharing has stopped".
///
/// Scope, honestly: proxied upstream responses are never touched (bodies are
/// never rewritten — docs/gateway.md), so a tab that only ever saw the app
/// itself (a link-access share, or a password share entered via an existing
/// session cookie) carries no marker and gets the plain 404. The marker also
/// lives on the same origin as the proxied app, whose own JS may legally
/// clear it (`sessionStorage.clear()`); every such miss degrades to the
/// anonymous copy — never to a false "stopped" claim.
///
/// Why sessionStorage: each slug is its own subdomain, hence its own web
/// origin — the browser scopes the key to that share automatically — and
/// sessionStorage lives exactly as long as the tab, which is the requested
/// semantics (a fresh tab on a dead slug sees the plain 404). Slugs are
/// capability-derived (slug.rs), so a different share can never inherit the
/// origin and a re-registered share maps back to it.
///
/// Privacy: this does NOT weaken the "404s never confirm a share existed"
/// stance — the server response is byte-identical for every viewer; the swap
/// happens client-side from state only a tab that itself witnessed the live
/// share can hold.
pub(crate) const SHARE_SEEN_KEY: &str = "veld-gw-share-seen";

/// Inline snippet that stamps [`SHARE_SEEN_KEY`]. Appended to gateway pages
/// that are only ever served while the slug's share is registered.
pub(crate) fn mark_share_seen_script() -> String {
    format!("<script>try{{sessionStorage.setItem('{SHARE_SEEN_KEY}','1')}}catch(e){{}}</script>")
}

/// Escape for HTML text/attribute contexts. `{`/`}` are escaped too: pages
/// are assembled by ordered string replacement (see [`shell`] and the login
/// page's `{next}`/`{error}` passes in `auth.rs`), so braces surviving into
/// an earlier substitution's VALUE (e.g. a viewer-supplied `next` of
/// literally `/{error}`) must never be re-expanded by a later pass.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('{', "&#123;")
        .replace('}', "&#125;")
}

/// Which flavor of 404 to render — a slug host whose share is gone reads
/// differently from a path that never existed.
#[derive(Clone, Copy)]
pub enum NotFound {
    /// A slug subdomain with no live registration behind it.
    Share,
    /// Anything else (apex fallback, unknown hosts, reserved paths).
    Generic,
}

/// The apex index page: says what this server is, links to the project.
/// Static on purpose — an anonymous viewer learns the gateway's identity
/// (that is the point of branding it) but nothing about its shares.
pub fn index() -> Response {
    let body = "<h1>This is a Veld gateway</h1>\
        <p>It publishes temporary preview links for <a href=\"https://veld.oss.life.li\">Veld</a> \
        development environments. Every share lives on its own subdomain and disappears \
        when the developer stops sharing.</p>\
        <p>There is nothing to browse here &mdash; if someone sent you a preview link, \
        open that link directly.</p>";
    html_response(StatusCode::OK, shell("Veld Gateway", body))
}

/// A branded 404. Still deliberately vague **as served**: neither variant's
/// HTTP response confirms whether a share ever existed at the address. The
/// share variant additionally carries a hidden "Sharing has stopped" block
/// that an inline script reveals only when the tab itself saw the share
/// alive earlier ([`SHARE_SEEN_KEY`]) — no server state, no new information
/// for anyone else.
pub fn not_found(kind: NotFound) -> Response {
    let (title, body) = match kind {
        NotFound::Share => ("Share not found", share_not_found_body()),
        NotFound::Generic => (
            "Not found",
            "<h1>Not found</h1>\
             <p>Nothing lives at this address.</p>"
                .to_owned(),
        ),
    };
    html_response(StatusCode::NOT_FOUND, shell(title, &body))
}

/// The share-404 body: the default "not found" copy, plus the tab-local
/// "sharing stopped" alternative revealed by the [`SHARE_SEEN_KEY`] check.
fn share_not_found_body() -> String {
    format!(
        r#"<div id="nf">
<h1>Share not found</h1>
<p>There is no active share at this address. The preview may have expired or been stopped by its owner.</p>
<p>Ask whoever sent you the link for a fresh one.</p>
</div>
<div id="stopped" hidden>
<h1>Sharing has stopped</h1>
<p>The owner of this preview has stopped sharing it (or the share expired), so it is no longer available.</p>
<p>If you still need access, ask whoever sent you the link to share again.</p>
</div>
<script>
try {{
  if (sessionStorage.getItem('{SHARE_SEEN_KEY}')) {{
    document.getElementById('nf').hidden = true;
    document.getElementById('stopped').hidden = false;
    document.title = 'Sharing has stopped';
  }}
}} catch (e) {{}}
</script>"#
    )
}

/// A branded error page for viewer-facing failures (dead tunnel,
/// unresponsive or timed-out upstream — upgrade requests included).
/// `title` and `message` are `&'static str` ON PURPOSE: it makes "trusted
/// constants only — never echo request data" a compile-time guarantee
/// instead of a doc-comment plea (a `&format!(…{host}…)` caller does not
/// compile). Machine-facing responses (the registration API, abuse guards
/// like 405/413, the pre-101 splice guard) stay plain text by design; see
/// docs/branding.md.
///
/// These pages are only reachable while a share is registered on the slug —
/// proxy.rs is the sole caller, and `share_` in the name is the contract:
/// they stamp [`SHARE_SEEN_KEY`]. The hazard to respect: any error served on
/// a SLUG origin whose registration is missing or already gone would stamp
/// the very origin the share 404 later reads, minting a false "Sharing has
/// stopped" — so only call this after a successful registry lookup (a live
/// `SlugTarget` in hand); for anything else add a separate unstamped helper.
pub fn share_error(status: StatusCode, title: &'static str, message: &'static str) -> Response {
    let body = format!(
        "<h1>{title}</h1><p>{message}</p>{}",
        mark_share_seen_script()
    );
    html_response(status, shell(title, &body))
}

/// Assemble an HTML response with the headers every gateway page shares.
/// `no-store` keeps intermediaries from pinning a 404 (or a stale index)
/// onto an address a share may occupy moments later.
fn html_response(status: StatusCode, page: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        page,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn index_is_branded_and_html() {
        let resp = index();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        let body = body_string(resp).await;
        assert!(body.contains("Veld gateway"), "{body}");
        assert!(body.contains("class=\"wordmark\""), "{body}");
        assert!(body.contains("noindex"), "{body}");
    }

    #[tokio::test]
    async fn not_found_variants_are_404_and_leak_nothing_dynamic() {
        for (kind, marker) in [
            (NotFound::Share, "Share not found"),
            (NotFound::Generic, "Nothing lives at this address"),
        ] {
            let resp = not_found(kind);
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                resp.headers().get(header::CACHE_CONTROL).unwrap(),
                "no-store"
            );
            let body = body_string(resp).await;
            assert!(body.contains(marker), "{body}");
            assert!(body.contains("class=\"wordmark\""), "{body}");
        }
    }

    #[tokio::test]
    async fn share_not_found_swaps_to_stopped_copy_only_client_side() {
        let resp = not_found(NotFound::Share);
        let body = body_string(resp).await;
        // Both copies ship, the stopped one hidden by default.
        assert!(body.contains("<div id=\"nf\">"), "{body}");
        assert!(body.contains("<div id=\"stopped\" hidden>"), "{body}");
        assert!(body.contains("Sharing has stopped"), "{body}");
        // The swap reads the tab-local marker and targets exactly the two
        // shipped ids — a drift between the divs and the script would leave
        // the page stuck on the default copy with every other assert green.
        assert!(
            body.contains(&format!("sessionStorage.getItem('{SHARE_SEEN_KEY}')")),
            "{body}"
        );
        assert!(body.contains("getElementById('nf')"), "{body}");
        assert!(body.contains("getElementById('stopped')"), "{body}");
        // …and the 404 itself must never SET it: a viewer bouncing off dead
        // slugs twice would otherwise mint the "stopped" copy from nothing.
        assert!(!body.contains("setItem"), "{body}");

        // The generic 404 has neither copy-swap nor marker.
        let generic = body_string(not_found(NotFound::Generic)).await;
        assert!(!generic.contains("sessionStorage"), "{generic}");
        assert!(!generic.contains("Sharing has stopped"), "{generic}");
    }

    #[tokio::test]
    async fn error_pages_stamp_the_share_seen_marker() {
        let resp = share_error(
            StatusCode::BAD_GATEWAY,
            "Share disconnected",
            "This share is no longer connected.",
        );
        let body = body_string(resp).await;
        assert!(
            body.contains(&format!("sessionStorage.setItem('{SHARE_SEEN_KEY}'")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn error_pages_are_branded_with_matching_status() {
        let resp = share_error(
            StatusCode::BAD_GATEWAY,
            "Share disconnected",
            "This share is no longer connected.",
        );
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let body = body_string(resp).await;
        assert!(body.contains("<h1>Share disconnected</h1>"), "{body}");
        assert!(body.contains("class=\"wordmark\""), "{body}");
    }

    #[test]
    fn shell_substitutes_title_and_body_and_keeps_css_braces() {
        let page = shell("T&lt;itle", "<h1>Hello</h1>");
        assert!(page.contains("<title>T&lt;itle</title>"));
        assert!(page.contains("<h1>Hello</h1>"));
        // The CSS survived the literal replacements.
        assert!(page.contains("--accent:#C4F56A"));
        // No unexpanded placeholders remain.
        assert!(
            !page.contains("{title}") && !page.contains("{body}") && !page.contains("{wordmark}")
        );
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
