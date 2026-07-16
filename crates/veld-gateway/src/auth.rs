//! Viewer access control for password-protected slugs (SHARING_V2.md §6.1).
//!
//! The gateway holds no session store: a viewer session is a signed token in
//! a cookie, verifiable from the registration alone. The signing key is
//! derived from the share **capability** — which arrives with every
//! registration/heartbeat — so sessions survive gateway restarts statelessly
//! and are invalidated automatically when the share rotates (new capability).
//!
//! Flow: an unauthenticated request to a password-mode slug gets a `401` with
//! a self-contained login page; the form (or the `#veld-key=…` URL-fragment
//! auto-submit) POSTs to the reserved path `/__veld_gateway__/auth`; a correct
//! password sets the session cookie and redirects to the originally requested
//! path. The cookie is host-only per slug, `HttpOnly; Secure; SameSite=Lax`,
//! and is stripped before proxying so the origin service never sees it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::Request;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use veld_core::share::Capability;

use crate::registry::SlugTarget;
use crate::state::AppState;

/// Reserved path prefix on slug hosts for the gateway's own endpoints.
/// Documented namespace theft — a real app path colliding with this is
/// practically impossible.
pub const RESERVED_PREFIX: &str = "/__veld_gateway__/";
/// The login form target (under the reserved prefix).
const AUTH_PATH: &str = "/__veld_gateway__/auth";
/// Session cookie name (host-only per slug; never forwarded upstream).
pub const SESSION_COOKIE: &str = "__veld_gw_sess";

/// Cap on a viewer session's lifetime; the share's own expiry caps it further.
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// Max password attempts per client IP per window.
const IP_LIMIT: u32 = 10;
/// Max password attempts per slug per window (all IPs — bounds a distributed
/// guess even when each bot stays under the per-IP limit).
const SLUG_LIMIT: u32 = 60;
/// Rate-limit window.
const LIMIT_WINDOW: Duration = Duration::from_secs(60);
/// Bound on limiter map size (evicts stale windows when exceeded).
const LIMITER_MAX_KEYS: usize = 10_000;
/// Bound on the login form body (password + path — anything bigger is abuse).
const MAX_FORM_BYTES: usize = 8 * 1024;

/// Outcome of the access gate for one request.
pub enum Gate {
    /// Authorized (or the node is link-access): proxy it.
    Allow(Request),
    /// The gate answered the request itself (login page, auth POST, 429…).
    Respond(Response),
}

/// Decide whether `req` may reach the tunnel.
pub async fn gate(state: &AppState, target: &SlugTarget, req: Request) -> Gate {
    use veld_core::config::WebAccessMode;
    if target.access == WebAccessMode::Link {
        // Fully transparent for link-access nodes — no reserved paths either.
        return Gate::Allow(req);
    }

    let path = req.uri().path().to_owned();
    if path.starts_with(RESERVED_PREFIX) {
        if path == AUTH_PATH {
            return Gate::Respond(handle_auth(state, target, req).await);
        }
        return Gate::Respond((StatusCode::NOT_FOUND, "not found").into_response());
    }

    let reg = &target.registration;
    let now = chrono::Utc::now().timestamp();
    if let Some(token) = session_cookie(req.headers()) {
        if verify_token(&reg.session_key(), &target.slug, now, &token) {
            return Gate::Allow(req);
        }
    }

    let next = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_owned();
    Gate::Respond(login_page(StatusCode::UNAUTHORIZED, &next, None))
}

/// Handle `POST /__veld_gateway__/auth` (and render the form for GET).
async fn handle_auth(state: &AppState, target: &SlugTarget, req: Request) -> Response {
    if req.method() == axum::http::Method::GET {
        return login_page(StatusCode::OK, "/", None);
    }
    if req.method() != axum::http::Method::POST {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response();
    }

    let reg = &target.registration;
    let Some(expected) = reg.password() else {
        // Password-mode slug without a password never registers; belt-and-braces.
        return (StatusCode::UNAUTHORIZED, "not accepting logins").into_response();
    };
    let expected = expected.to_owned();

    let client_ip = client_ip(&req, state.config.trust_forwarded_headers);

    // Throttle BEFORE reading the body or comparing anything.
    if !state.limiter.allow(client_ip.as_deref(), &target.slug) {
        return login_page(
            StatusCode::TOO_MANY_REQUESTS,
            "/",
            Some("Too many attempts. Wait a minute and try again."),
        );
    }

    let (_parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_FORM_BYTES).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "form too large").into_response(),
    };
    let form: LoginForm = match serde_urlencoded::from_bytes(&bytes) {
        Ok(f) => f,
        Err(_) => return login_page(StatusCode::BAD_REQUEST, "/", Some("Invalid request.")),
    };
    let next = safe_next(form.next.as_deref());

    if !ct_eq(form.password.trim().as_bytes(), expected.as_bytes()) {
        return login_page(
            StatusCode::UNAUTHORIZED,
            &next,
            Some("Wrong password. Check it and try again."),
        );
    }

    // Session expiry: bounded by both the TTL cap and the share's own expiry.
    let now = chrono::Utc::now().timestamp();
    let expiry = (now + SESSION_TTL.as_secs() as i64).min(reg.expires_at());
    if expiry <= now {
        return login_page(
            StatusCode::UNAUTHORIZED,
            &next,
            Some("This share has expired."),
        );
    }
    let token = mint_token(&reg.session_key(), &target.slug, expiry);
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; Max-Age={}; Secure; HttpOnly; SameSite=Lax",
        expiry - now
    );

    let mut resp = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, next)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

#[derive(serde::Deserialize)]
struct LoginForm {
    #[serde(default)]
    password: String,
    #[serde(default)]
    next: Option<String>,
}

/// The client IP for rate limiting: the socket peer, unless the operator
/// opted into trusting a sanitising upstream LB (`trust_forwarded_headers`),
/// in which case the **last** `X-Forwarded-For` entry — the one appended by
/// that trusted LB — is the real client. Never trust earlier entries (the
/// client controls them).
fn client_ip(req: &Request, trust_forwarded: bool) -> Option<String> {
    if trust_forwarded {
        if let Some(xff) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(last) = xff.rsplit(',').next() {
                let last = last.trim();
                if last.parse::<IpAddr>().is_ok() {
                    return Some(last.to_owned());
                }
            }
        }
    }
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
}

/// Validate the post-login redirect target: a same-origin relative path only
/// (no `//host`, no backslash tricks) — never an open redirect.
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.contains('\\') => n.to_owned(),
        _ => "/".to_owned(),
    }
}

/// The session cookie's value from a request, if present.
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(s) = value.to_str() else { continue };
        for pair in s.split(';') {
            let pair = pair.trim();
            if let Some(v) = pair.strip_prefix(SESSION_COOKIE) {
                if let Some(v) = v.strip_prefix('=') {
                    return Some(v.to_owned());
                }
            }
        }
    }
    None
}

/// Remove the gateway session cookie from a `Cookie` header value; `None`
/// when nothing is left. The origin service must never see gateway-internal
/// credentials.
pub fn strip_session_cookie(cookie_header: &str) -> Option<String> {
    let kept: Vec<&str> = cookie_header
        .split(';')
        .map(str::trim)
        .filter(|pair| {
            !pair
                .strip_prefix(SESSION_COOKIE)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .filter(|p| !p.is_empty())
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join("; "))
    }
}

// ---------------------------------------------------------------------------
// Session tokens — stateless, capability-derived (SHARING_V2.md §6.1)
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

/// Derive the per-share session-signing key from the capability. One-way, so
/// the key leaks nothing about the capability; share rotation (a new
/// capability) invalidates every outstanding session by construction.
pub fn session_key(capability: &Capability) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(capability.as_bytes()).expect("any key length works");
    mac.update(b"veld-gateway-viewer-session/1");
    mac.finalize().into_bytes().into()
}

/// Mint a session token bound to `slug`, valid until `expiry` (unix secs).
pub fn mint_token(key: &[u8; 32], slug: &str, expiry: i64) -> String {
    let mac = token_mac(key, slug, expiry);
    let mut raw = Vec::with_capacity(8 + 32);
    raw.extend_from_slice(&expiry.to_be_bytes());
    raw.extend_from_slice(&mac);
    data_encoding::BASE64URL_NOPAD.encode(&raw)
}

/// Verify a session token for `slug` at time `now`.
pub fn verify_token(key: &[u8; 32], slug: &str, now: i64, token: &str) -> bool {
    let Ok(raw) = data_encoding::BASE64URL_NOPAD.decode(token.as_bytes()) else {
        return false;
    };
    if raw.len() != 8 + 32 {
        return false;
    }
    let expiry = i64::from_be_bytes(raw[..8].try_into().expect("checked length"));
    if expiry <= now {
        return false;
    }
    ct_eq(&raw[8..], &token_mac(key, slug, expiry))
}

fn token_mac(key: &[u8; 32], slug: &str, expiry: i64) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("any key length works");
    mac.update(slug.as_bytes());
    mac.update(&expiry.to_be_bytes());
    mac.finalize().into_bytes().into()
}

/// Constant-time byte equality (length leak is inherent and harmless here).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// Rate limiting — an in-memory throttle, not a ledger (resets on restart)
// ---------------------------------------------------------------------------

/// Fixed-window attempt counters keyed by client IP and by slug.
pub struct RateLimiter {
    windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
        }
    }
}

impl RateLimiter {
    /// Record one attempt; `false` when either the per-IP or per-slug budget
    /// for the current window is exhausted. A missing client IP (no socket
    /// info — shouldn't happen) still counts against the slug budget.
    pub fn allow(&self, client_ip: Option<&str>, slug: &str) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("limiter lock");
        if windows.len() > LIMITER_MAX_KEYS {
            windows.retain(|_, (start, _)| now.duration_since(*start) < LIMIT_WINDOW);
        }
        let mut check = |key: String, limit: u32| -> bool {
            let entry = windows.entry(key).or_insert((now, 0));
            if now.duration_since(entry.0) >= LIMIT_WINDOW {
                *entry = (now, 0);
            }
            entry.1 += 1;
            entry.1 <= limit
        };
        // Evaluate BOTH (an attempt always counts against both budgets).
        let ip_ok = match client_ip {
            Some(ip) => check(format!("ip:{ip}"), IP_LIMIT),
            None => true,
        };
        let slug_ok = check(format!("slug:{slug}"), SLUG_LIMIT);
        ip_ok && slug_ok
    }
}

// ---------------------------------------------------------------------------
// Login page
// ---------------------------------------------------------------------------

/// Render the self-contained login page. Deliberately generic: no share
/// metadata (project/run/hostnames) is leaked to an unauthenticated viewer.
fn login_page(status: StatusCode, next: &str, error: Option<&str>) -> Response {
    let next_attr = html_escape(&safe_next(Some(next)));
    let error_html = error
        .map(|e| format!("<p class=\"err\">{}</p>", html_escape(e)))
        .unwrap_or_default();
    let page = LOGIN_PAGE
        .replace("{next}", &next_attr)
        .replace("{error}", &error_html);
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        page,
    )
        .into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Self-contained (CSP-friendly, no external assets). The inline script
/// implements the one-link flow: a `#veld-key=…` fragment auto-fills and
/// submits the form, then strips itself from the URL — the fragment never
/// reaches the server or its logs.
const LOGIN_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>Password required</title>
<style>
  body { font-family: system-ui, sans-serif; display: flex; min-height: 100vh;
         margin: 0; align-items: center; justify-content: center; background: #f5f5f4; }
  form { background: #fff; padding: 2rem 2.5rem; border-radius: 12px;
         box-shadow: 0 1px 4px rgba(0,0,0,.12); max-width: 22rem; }
  h1 { font-size: 1.1rem; margin: 0 0 .5rem; }
  p { color: #555; font-size: .9rem; margin: 0 0 1rem; }
  .err { color: #b91c1c; }
  input[type=password] { width: 100%; box-sizing: border-box; padding: .5rem .75rem;
         font-size: 1rem; border: 1px solid #ccc; border-radius: 8px; margin-bottom: 1rem; }
  button { width: 100%; padding: .55rem; font-size: 1rem; border: 0; border-radius: 8px;
         background: #1d4ed8; color: #fff; cursor: pointer; }
</style>
</head>
<body>
<form id="f" method="post" action="/__veld_gateway__/auth" autocomplete="off">
  <h1>Password required</h1>
  <p>This preview is protected. Enter the password you were given.</p>
  {error}
  <input type="hidden" name="next" value="{next}">
  <input id="pw" type="password" name="password" autofocus aria-label="Password">
  <button type="submit">Open</button>
</form>
<script>
(function () {
  var m = location.hash.match(/[#&]veld-key=([^&]+)/);
  if (!m) return;
  document.getElementById('pw').value = decodeURIComponent(m[1]);
  var next = document.querySelector('input[name=next]');
  if (next.value === '/' && (location.pathname !== '/' || location.search))
    next.value = location.pathname + location.search;
  history.replaceState(null, '', location.pathname + location.search);
  document.getElementById('f').submit();
})();
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip_and_rejections() {
        let cap = Capability::generate();
        let key = session_key(&cap);
        let token = mint_token(&key, "abc123", 2_000_000_000);

        assert!(verify_token(&key, "abc123", 1_999_999_999, &token));
        // Expired.
        assert!(!verify_token(&key, "abc123", 2_000_000_000, &token));
        // Wrong slug (a sibling slug in the same share must not accept it).
        assert!(!verify_token(&key, "other0", 1_999_999_999, &token));
        // Wrong key (another share / rotated capability).
        let other = session_key(&Capability::generate());
        assert!(!verify_token(&other, "abc123", 1_999_999_999, &token));
        // Garbage.
        assert!(!verify_token(&key, "abc123", 0, "not-base64!!"));
        assert!(!verify_token(&key, "abc123", 0, ""));
    }

    #[test]
    fn token_cannot_be_extended_by_tampering_expiry() {
        let key = session_key(&Capability::generate());
        let token = mint_token(&key, "abc123", 1_000);
        let mut raw = data_encoding::BASE64URL_NOPAD
            .decode(token.as_bytes())
            .unwrap();
        // Bump the expiry field; the MAC no longer matches.
        raw[7] = raw[7].wrapping_add(1);
        let forged = data_encoding::BASE64URL_NOPAD.encode(&raw);
        assert!(!verify_token(&key, "abc123", 500, &forged));
    }

    #[test]
    fn session_key_is_deterministic_per_capability() {
        let cap = Capability::generate();
        assert_eq!(session_key(&cap), session_key(&cap));
        assert_ne!(session_key(&cap), session_key(&Capability::generate()));
    }

    #[test]
    fn safe_next_blocks_open_redirects() {
        assert_eq!(safe_next(Some("/x/y?z=1")), "/x/y?z=1");
        assert_eq!(safe_next(Some("//evil.example")), "/");
        assert_eq!(safe_next(Some("https://evil.example")), "/");
        assert_eq!(safe_next(Some("/\\evil")), "/");
        assert_eq!(safe_next(Some("relative")), "/");
        assert_eq!(safe_next(None), "/");
    }

    #[test]
    fn cookie_extraction_and_stripping() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; __veld_gw_sess=tok; b=2"),
        );
        assert_eq!(session_cookie(&headers).as_deref(), Some("tok"));

        assert_eq!(
            strip_session_cookie("a=1; __veld_gw_sess=tok; b=2").as_deref(),
            Some("a=1; b=2")
        );
        assert_eq!(strip_session_cookie("__veld_gw_sess=tok"), None);
        // A cookie whose name merely starts with ours is NOT stripped.
        assert_eq!(
            strip_session_cookie("__veld_gw_sess2=x").as_deref(),
            Some("__veld_gw_sess2=x")
        );
    }

    #[test]
    fn rate_limiter_blocks_after_budget() {
        let l = RateLimiter::default();
        for _ in 0..IP_LIMIT {
            assert!(l.allow(Some("1.2.3.4"), "slugx"));
        }
        // Attempt #limit+1 from the same IP is refused…
        assert!(!l.allow(Some("1.2.3.4"), "slugx"));
        // …but another IP still gets through (slug budget not yet exhausted).
        assert!(l.allow(Some("5.6.7.8"), "slugx"));
    }

    #[test]
    fn rate_limiter_slug_budget_covers_distributed_attempts() {
        let l = RateLimiter::default();
        let mut allowed = 0;
        for i in 0..(SLUG_LIMIT + 20) {
            // A fresh IP each attempt — only the slug budget can stop this.
            if l.allow(Some(&format!("10.0.0.{i}")), "slugy") {
                allowed += 1;
            }
        }
        assert_eq!(allowed, SLUG_LIMIT);
    }

    #[test]
    fn login_page_escapes_next_and_error() {
        let resp = login_page(
            StatusCode::UNAUTHORIZED,
            "/x\"><script>alert(1)</script>",
            None,
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // Can't easily read the body here without a runtime; escaping itself:
        assert_eq!(html_escape("\"><script>"), "&quot;&gt;&lt;script&gt;");
    }
}
