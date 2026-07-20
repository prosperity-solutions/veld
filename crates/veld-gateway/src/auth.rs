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

use crate::pages::html_escape;
use crate::registry::SlugTarget;
use crate::state::AppState;

/// Reserved path prefix on slug hosts for the gateway's own endpoints.
/// Documented namespace theft — a real app path colliding with this is
/// practically impossible.
pub const RESERVED_PREFIX: &str = "/__veld_gateway__/";
/// The login form target (under the reserved prefix).
const AUTH_PATH: &str = "/__veld_gateway__/auth";
/// Session cookie name. The `__Host-` prefix makes the browser itself reject
/// any variant carrying a `Domain` attribute — so a hostile co-tenant's
/// upstream on the same gateway domain cannot set a domain-scoped cookie that
/// shadows other slugs' host-only sessions. (Requires `Secure` + `Path=/` +
/// no `Domain`, all of which the mint below sets.)
pub const SESSION_COOKIE: &str = "__Host-veld_gw_sess";

/// Cap on a viewer session's lifetime; the share's own expiry caps it further.
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// Max password attempts per client IP per window.
const IP_LIMIT: u32 = 10;
/// Max password attempts per slug per window, across all IPs. This bounds a
/// distributed guess (botnet under the per-IP limit) — but any global cap is
/// also a lockout lever for a flooder, so it sits well above human login
/// rates: at ~59-bit generated passwords, 300/min of guessing is still
/// nothing, while accidental lockouts need a sustained deliberate flood.
const SLUG_LIMIT: u32 = 300;
/// Rate-limit window.
const LIMIT_WINDOW: Duration = Duration::from_secs(60);
/// Hard bound on the per-IP limiter map (the per-slug map is naturally
/// bounded by the number of live slugs).
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

/// The slice of a [`SlugTarget`] the gate needs — no live tunnel, so the auth
/// decisions are testable at the request level without an iroh `Connection`.
pub struct SlugAuth {
    pub slug: String,
    pub access: veld_core::config::WebAccessMode,
    pub password: Option<String>,
    pub session_key: [u8; 32],
    pub expires_at: i64,
}

impl SlugAuth {
    pub fn of(target: &SlugTarget) -> Self {
        Self {
            slug: target.slug.clone(),
            access: target.access,
            password: target.registration.password().map(str::to_owned),
            session_key: target.registration.session_key(),
            expires_at: target.registration.expires_at(),
        }
    }
}

/// Decide whether `req` may reach the tunnel.
pub async fn gate(state: &AppState, target: &SlugAuth, req: Request) -> Gate {
    use veld_core::config::WebAccessMode;
    // Exhaustive on purpose: a future access mode (e.g. per-viewer approval,
    // parked in SHARING_V2.md §6.1) must force a conscious decision here
    // rather than silently inheriting the password flow.
    match target.access {
        // Fully transparent for link-access nodes — no reserved paths either.
        WebAccessMode::Link => return Gate::Allow(req),
        WebAccessMode::Password => {}
    }

    let path = req.uri().path().to_owned();
    if path.starts_with(RESERVED_PREFIX) {
        if path == AUTH_PATH {
            return Gate::Respond(handle_auth(state, target, req).await);
        }
        return Gate::Respond(crate::pages::not_found(crate::pages::NotFound::Generic));
    }

    let now = chrono::Utc::now().timestamp();
    if let Some(token) = session_cookie(req.headers()) {
        if verify_token(&target.session_key, &target.slug, now, &token) {
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
async fn handle_auth(state: &AppState, target: &SlugAuth, req: Request) -> Response {
    if req.method() == axum::http::Method::GET {
        // An already-authenticated viewer who lands on the login URL (e.g. a
        // pasted link) goes straight in instead of being re-prompted.
        let now = chrono::Utc::now().timestamp();
        if let Some(token) = session_cookie(req.headers()) {
            if verify_token(&target.session_key, &target.slug, now, &token) {
                return Response::builder()
                    .status(StatusCode::SEE_OTHER)
                    .header(header::LOCATION, "/")
                    .body(Body::empty())
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
        }
        return login_page(StatusCode::OK, "/", None);
    }
    if req.method() != axum::http::Method::POST {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response();
    }

    let Some(expected) = target.password.clone() else {
        // Password-mode slug without a password never registers; belt-and-braces.
        return (StatusCode::UNAUTHORIZED, "not accepting logins").into_response();
    };

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

    // Session expiry: bounded by the TTL cap and the share's own expiry.
    // `expires_at` was stamped by the DAEMON's clock and `now` is the
    // gateway's — under clock skew (or a share in its final seconds) the min
    // could land in the past. Floor it to a short grace session instead of
    // refusing: share liveness is governed by the registration (an expired
    // share is torn down and its slug 404s), not by the cookie.
    let now = chrono::Utc::now().timestamp();
    let expiry = (now + SESSION_TTL.as_secs() as i64)
        .min(target.expires_at)
        .max(now + 300);
    let token = mint_token(&target.session_key, &target.slug, expiry);
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
/// (no `//host`, no backslash tricks, no control characters — belt-and-braces
/// against header injection should this value ever reach a sink without the
/// http crate's own validation) — never an open redirect.
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n)
            if n.starts_with('/')
                && !n.starts_with("//")
                && !n.contains('\\')
                && !n.chars().any(|c| c.is_control()) =>
        {
            n.to_owned()
        }
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
        .filter(|pair| !is_session_pair(pair.as_bytes()))
        .filter(|p| !p.is_empty())
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join("; "))
    }
}

/// True if `pair` (already `;`-split) is our session cookie: `<name>=<value>`.
/// Byte-level so a Cookie header that mixes an ASCII session pair with a
/// non-UTF-8 pair still matches — the session token must never reach the
/// origin just because some *other* cookie value isn't str-able.
fn is_session_pair(pair: &[u8]) -> bool {
    let pair = trim_ascii_ws(pair);
    pair.strip_prefix(SESSION_COOKIE.as_bytes())
        .is_some_and(|rest| trim_ascii_ws_start(rest).first() == Some(&b'='))
}

/// Strip the gateway session cookie from raw `Cookie` header bytes (for the
/// path where the whole value isn't valid UTF-8). Returns the surviving
/// pairs as bytes, `Some(vec![])` when only the session cookie was present
/// (send no Cookie header), or `None` when our cookie wasn't there at all
/// (caller forwards the original value untouched).
pub fn strip_session_cookie_bytes(cookie_header: &[u8]) -> Option<Vec<u8>> {
    let mut found = false;
    let mut kept: Vec<&[u8]> = Vec::new();
    for pair in cookie_header.split(|&b| b == b';') {
        if is_session_pair(pair) {
            found = true;
            continue;
        }
        let trimmed = trim_ascii_ws(pair);
        if !trimmed.is_empty() {
            kept.push(trimmed);
        }
    }
    if !found {
        return None;
    }
    Some(kept.join(&b"; "[..]))
}

fn trim_ascii_ws(b: &[u8]) -> &[u8] {
    trim_ascii_ws_end(trim_ascii_ws_start(b))
}
fn trim_ascii_ws_start(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}
fn trim_ascii_ws_end(mut b: &[u8]) -> &[u8] {
    while let [rest @ .., last] = b {
        if last.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
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

/// Fixed-window attempt counters, split into two maps so their bounds are
/// independent: the slug map's key count is naturally bounded by the number
/// of live slugs, and the IP map is hard-bounded — so a flood of distinct
/// fresh IPs can never grow memory without limit, and clearing the IP map
/// under such a flood never resets the (guessing-relevant) slug budget.
pub struct RateLimiter {
    ips: Mutex<HashMap<String, (Instant, u32)>>,
    slugs: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self {
            ips: Mutex::new(HashMap::new()),
            slugs: Mutex::new(HashMap::new()),
        }
    }
}

/// Bump one fixed-window counter and return its post-increment count.
fn bump(map: &mut HashMap<String, (Instant, u32)>, key: &str, now: Instant) -> u32 {
    let entry = map.entry(key.to_owned()).or_insert((now, 0));
    if now.duration_since(entry.0) >= LIMIT_WINDOW {
        *entry = (now, 0);
    }
    entry.1 += 1;
    entry.1
}

impl RateLimiter {
    /// Record one attempt; `false` when either budget for the current window
    /// is exhausted.
    ///
    /// The per-IP budget is checked FIRST, and the shared per-slug counter is
    /// bumped **only for an IP within its budget**. So a single flooding IP
    /// contributes at most `IP_LIMIT` to the slug counter and can never lock
    /// every viewer out of a slug; a distributed flood still reaches the slug
    /// cap through many within-budget IPs (the intended bound). A missing
    /// client IP (no socket info — shouldn't happen) skips the IP gate and
    /// counts only against the slug budget.
    pub fn allow(&self, client_ip: Option<&str>, slug: &str) -> bool {
        let now = Instant::now();
        if let Some(ip) = client_ip {
            let mut ips = self.ips.lock().expect("limiter lock");
            if ips.len() >= LIMITER_MAX_KEYS {
                ips.retain(|_, (start, _)| now.duration_since(*start) < LIMIT_WINDOW);
                // Still at the cap after dropping stale windows means a
                // distinct-fresh-IP flood: drop the map rather than grow
                // unbounded. Losing one window of per-IP history is the lesser
                // harm — the slug budget below still bounds guessing.
                if ips.len() >= LIMITER_MAX_KEYS {
                    ips.clear();
                }
            }
            if bump(&mut ips, ip, now) > IP_LIMIT {
                // Over IP budget → refused WITHOUT touching the shared slug
                // counter, so this IP can't inflate the slug lockout.
                return false;
            }
        }
        let mut slugs = self.slugs.lock().expect("limiter lock");
        // Paranoia sweep; live slugs already bound this map.
        if slugs.len() > LIMITER_MAX_KEYS {
            slugs.retain(|_, (start, _)| now.duration_since(*start) < LIMIT_WINDOW);
        }
        bump(&mut slugs, slug, now) <= SLUG_LIMIT
    }
}

// ---------------------------------------------------------------------------
// Login page
// ---------------------------------------------------------------------------

/// Render the self-contained login page (branded via [`crate::pages::shell`]).
/// Deliberately generic: no share metadata (project/run/hostnames) is leaked
/// to an unauthenticated viewer.
fn login_page(status: StatusCode, next: &str, error: Option<&str>) -> Response {
    let next_attr = html_escape(&safe_next(Some(next)));
    let error_html = error
        .map(|e| format!("<p class=\"err\">{}</p>", html_escape(e)))
        .unwrap_or_default();
    // The login page only renders while the share is registered, so it stamps
    // the tab-local "seen alive" marker the share 404 reads (pages.rs).
    let body = format!("{LOGIN_BODY}{}", crate::pages::mark_share_seen_script());
    // Shell placeholders expand first; the viewer-influenced values go in
    // last and are brace-escaped, so they can never re-trigger a placeholder.
    let page = crate::pages::shell("Password required", &body)
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

/// The login form + one-link script, rendered inside the branded page shell
/// (self-contained, CSP-friendly, no external assets). Two behaviours:
///
/// * **One-link flow** — a `#veld-key=…` fragment auto-fills and submits the
///   form, then strips itself from the URL, so the fragment never reaches the
///   server or its logs.
/// * **Loading state** — on submit (manual OR the auto-fill path) the form
///   enters a `loading` state: the field goes read-only and dims, the button
///   shows a spinner and "Unlocking…", so there is a visible cue that an
///   attempt is in flight. It is progressive: with JS disabled the form still
///   submits, just without the affordance. The field is made **read-only, not
///   disabled** — a disabled field is omitted from the POST body, which would
///   silently drop the password. No client-side reset is needed: a wrong
///   password re-renders this page fresh (server-side), which clears the
///   state; a correct one navigates away.
const LOGIN_BODY: &str = r#"<form id="f" method="post" action="/__veld_gateway__/auth" autocomplete="off">
  <h1>Password required</h1>
  <p>This preview is protected. Enter the password you were given.</p>
  {error}
  <input type="hidden" name="next" value="{next}">
  <input id="pw" type="password" name="password" autofocus aria-label="Password" required>
  <button id="unlock" type="submit">
    <span class="btn-idle">Open</span>
    <span class="btn-busy"><span class="spinner" aria-hidden="true"></span>Unlocking&hellip;</span>
  </button>
  <p class="hint">Your browser must allow cookies for this site.</p>
</form>
<style>
  .btn-busy{display:none;align-items:center;justify-content:center;gap:.5rem}
  #f.loading .btn-idle{display:none}
  #f.loading .btn-busy{display:inline-flex}
  #f.loading input[type=password]{opacity:.55}
  button:disabled{cursor:default;filter:none}
  .spinner{width:.85rem;height:.85rem;border:2px solid rgba(15,17,23,.3);border-top-color:var(--bg);border-radius:50%;display:inline-block;animation:veld-spin .6s linear infinite}
  @keyframes veld-spin{to{transform:rotate(360deg)}}
  @media (prefers-reduced-motion:reduce){.spinner{animation:none}}
</style>
<script>
(function () {
  var form = document.getElementById('f');
  var pw = document.getElementById('pw');
  // NB: the button's id must not be `submit` (or any HTMLFormElement member) —
  // a like-named child shadows `form.submit`, breaking the auto-submit below.
  var btn = document.getElementById('unlock');
  var loading = false;
  function setLoading() {
    if (loading) return;
    loading = true;
    form.classList.add('loading');
    form.setAttribute('aria-busy', 'true');
    // `readOnly` (not `disabled`) keeps the value in the POST body while
    // still blocking edits while the attempt is in flight.
    pw.readOnly = true;
    btn.disabled = true;
  }
  // Manual submit: the native submit event fires, so hook it. (The auto-fill
  // path below calls form.submit(), which does NOT fire this event, so it
  // sets the state itself.)
  form.addEventListener('submit', setLoading);

  var m = location.hash.match(/[#&]veld-key=([^&]+)/);
  if (!m) return;
  var key;
  try { key = decodeURIComponent(m[1]); } catch (e) { return; }
  pw.value = key;
  // Everything in the fragment except the key survives: it is stripped from
  // the visible URL and forwarded through `next` so the app's own hash (e.g.
  // a deep-link anchor) still arrives after the redirect.
  var rest = location.hash.slice(1).split('&').filter(function (p) {
    return p.indexOf('veld-key=') !== 0;
  }).join('&');
  var next = document.querySelector('input[name=next]');
  if (next.value === '/' && (location.pathname !== '/' || location.search))
    next.value = location.pathname + location.search;
  if (rest) next.value += '#' + rest;
  history.replaceState(null, '', location.pathname + location.search + (rest ? '#' + rest : ''));
  setLoading();
  form.submit();
})();
</script>"#;

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
    fn client_ip_selection_honors_trust_and_last_hop() {
        let with_peer_and_xff = |xff: Option<&str>| {
            let mut b = Request::builder().method("GET").uri("/");
            if let Some(v) = xff {
                b = b.header("x-forwarded-for", v);
            }
            let mut req = b.body(axum::body::Body::empty()).unwrap();
            req.extensions_mut().insert(ConnectInfo(
                "9.9.9.9:5000".parse::<std::net::SocketAddr>().unwrap(),
            ));
            req
        };

        // Untrusted edge: always the socket peer, XFF ignored.
        assert_eq!(
            client_ip(&with_peer_and_xff(Some("1.2.3.4, 5.6.7.8")), false).as_deref(),
            Some("9.9.9.9")
        );
        // Trusted LB: the LAST XFF hop (appended by the trusted immediate LB).
        assert_eq!(
            client_ip(&with_peer_and_xff(Some("1.2.3.4, 5.6.7.8")), true).as_deref(),
            Some("5.6.7.8")
        );
        // Trusted but a malformed last hop → fall back to the socket peer.
        assert_eq!(
            client_ip(&with_peer_and_xff(Some("1.2.3.4, not-an-ip")), true).as_deref(),
            Some("9.9.9.9")
        );
        // Trusted but no XFF at all → socket peer.
        assert_eq!(
            client_ip(&with_peer_and_xff(None), true).as_deref(),
            Some("9.9.9.9")
        );
    }

    #[test]
    fn safe_next_blocks_open_redirects() {
        assert_eq!(safe_next(Some("/x/y?z=1")), "/x/y?z=1");
        assert_eq!(safe_next(Some("//evil.example")), "/");
        assert_eq!(safe_next(Some("https://evil.example")), "/");
        assert_eq!(safe_next(Some("/\\evil")), "/");
        assert_eq!(safe_next(Some("relative")), "/");
        assert_eq!(safe_next(None), "/");
        // Control chars are rejected: browsers strip tab/newline during URL
        // parsing, so "/\t/evil.com" would become "//evil.com" — an open
        // redirect laundered through a header-legal value.
        assert_eq!(safe_next(Some("/\t/evil.com")), "/");
        assert_eq!(safe_next(Some("/x\r\ny")), "/");
    }

    #[test]
    fn cookie_extraction_and_stripping() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("a=1; {SESSION_COOKIE}=tok; b=2")).unwrap(),
        );
        assert_eq!(session_cookie(&headers).as_deref(), Some("tok"));

        assert_eq!(
            strip_session_cookie(&format!("a=1; {SESSION_COOKIE}=tok; b=2")).as_deref(),
            Some("a=1; b=2")
        );
        assert_eq!(strip_session_cookie(&format!("{SESSION_COOKIE}=tok")), None);
        // A cookie whose name merely starts with ours is NOT stripped.
        let similar = format!("{SESSION_COOKIE}2=x");
        assert_eq!(strip_session_cookie(&similar).as_deref(), Some(&*similar));
    }

    #[test]
    fn strip_session_cookie_bytes_handles_mixed_non_utf8_pairs() {
        // The load-bearing case: an ASCII session pair mixed with a non-UTF-8
        // pair. A str-first strip would fail wholesale and leak the token;
        // the byte-level strip removes only the session pair.
        let mut raw = format!("a=1; {SESSION_COOKIE}=tok; b=").into_bytes();
        raw.push(0xff); // invalid UTF-8 in the `b` value
        let out = strip_session_cookie_bytes(&raw).expect("our cookie was present");
        assert_eq!(&out[..out.len() - 1], b"a=1; b=");
        assert_eq!(out.last(), Some(&0xff));

        // Our cookie absent → None (caller forwards verbatim), even with a
        // non-UTF-8 byte present.
        assert!(strip_session_cookie_bytes(&[b'x', b'=', 0xff]).is_none());

        // Only the session cookie present → Some(empty) (send no Cookie).
        assert_eq!(
            strip_session_cookie_bytes(format!("{SESSION_COOKIE}=tok").as_bytes()),
            Some(Vec::new())
        );
        // A name that merely starts with ours is preserved.
        let similar = format!("{SESSION_COOKIE}2=x");
        assert!(strip_session_cookie_bytes(similar.as_bytes()).is_none());
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

    // -- Request-level gate()/handle_auth() coverage -------------------------

    use veld_core::config::WebAccessMode;

    fn test_state(trust_forwarded: bool) -> AppState {
        use crate::config::GatewayConfig;
        use crate::registry::{Registry, RelayAllowList};
        AppState {
            config: std::sync::Arc::new(GatewayConfig {
                domain: "share.example".into(),
                listen: "127.0.0.1:0".parse().unwrap(),
                tls: None,
                auth_token: veld_core::config::SecretSource::Literal("t".into()),
                relays: None,
                lease: Duration::from_secs(90),
                state_dir: None,
                max_registrations: 8,
                trust_forwarded_headers: trust_forwarded,
                trust_forwarded_host: trust_forwarded,
            }),
            registry: Registry::new(
                "share.example".into(),
                Duration::from_secs(90),
                RelayAllowList::Unconfined,
                iroh::SecretKey::generate(),
                8,
            ),
            auth_token: "t".into(),
            limiter: std::sync::Arc::new(RateLimiter::default()),
        }
    }

    fn slug_auth(access: WebAccessMode, password: Option<&str>) -> SlugAuth {
        SlugAuth {
            slug: "abc123abc123abc123abc123ab".into(),
            access,
            password: password.map(str::to_owned),
            session_key: session_key(&Capability::generate()),
            expires_at: chrono::Utc::now().timestamp() + 3600,
        }
    }

    fn get(path: &str) -> Request {
        Request::builder()
            .method("GET")
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    fn post_form(body: &str) -> Request {
        Request::builder()
            .method("POST")
            .uri(AUTH_PATH)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap()
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn gate_link_access_is_fully_transparent() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Link, None);
        // Ordinary path AND the reserved prefix both pass straight through.
        assert!(matches!(
            gate(&state, &target, get("/x")).await,
            Gate::Allow(_)
        ));
        assert!(matches!(
            gate(&state, &target, get(AUTH_PATH)).await,
            Gate::Allow(_)
        ));
    }

    #[tokio::test]
    async fn gate_password_without_cookie_gets_login_page_with_next() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("pw"));
        let Gate::Respond(resp) = gate(&state, &target, get("/deep/path?q=1")).await else {
            panic!("expected a login page, not a proxied request");
        };
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let body = body_string(resp).await;
        assert!(body.contains("value=\"/deep/path?q=1\""), "{body}");
        assert!(body.contains(AUTH_PATH), "{body}");
    }

    #[tokio::test]
    async fn gate_valid_session_cookie_allows_and_bad_ones_do_not() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("pw"));
        let expiry = chrono::Utc::now().timestamp() + 600;
        let token = mint_token(&target.session_key, &target.slug, expiry);

        let with_cookie = |t: &str| {
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, format!("{SESSION_COOKIE}={t}"))
                .body(axum::body::Body::empty())
                .unwrap()
        };
        assert!(matches!(
            gate(&state, &target, with_cookie(&token)).await,
            Gate::Allow(_)
        ));
        // A token minted for a DIFFERENT slug (same key) is refused.
        let foreign = mint_token(&target.session_key, "othersl", expiry);
        assert!(matches!(
            gate(&state, &target, with_cookie(&foreign)).await,
            Gate::Respond(_)
        ));
        // Garbage is refused.
        assert!(matches!(
            gate(&state, &target, with_cookie("garbage")).await,
            Gate::Respond(_)
        ));
    }

    #[tokio::test]
    async fn gate_reserved_non_auth_path_is_404() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("pw"));
        let Gate::Respond(resp) = gate(&state, &target, get("/__veld_gateway__/other")).await
        else {
            panic!("reserved path must not be proxied on a password slug");
        };
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn auth_post_correct_password_sets_verifiable_cookie_and_redirects() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("k7dm-q2xp-9fzt"));
        let Gate::Respond(resp) = gate(
            &state,
            &target,
            post_form("password=k7dm-q2xp-9fzt&next=%2Fdeep%3Fq%3D1"),
        )
        .await
        else {
            panic!("auth POST must be answered by the gate");
        };
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/deep?q=1");
        let cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("Secure"), "{cookie}");
        assert!(cookie.contains("SameSite=Lax"), "{cookie}");
        // The minted cookie actually authorizes a follow-up request.
        let token = cookie
            .strip_prefix(&format!("{SESSION_COOKIE}="))
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let now = chrono::Utc::now().timestamp();
        assert!(verify_token(&target.session_key, &target.slug, now, token));
    }

    #[tokio::test]
    async fn auth_post_wrong_password_or_open_redirect_is_refused() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("right"));

        let Gate::Respond(resp) = gate(&state, &target, post_form("password=wrong")).await else {
            panic!("expected a response");
        };
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().get(header::SET_COOKIE).is_none());

        // Correct password + hostile `next` → redirect sanitized to "/".
        let Gate::Respond(resp) = gate(
            &state,
            &target,
            post_form("password=right&next=%2F%2Fevil.example"),
        )
        .await
        else {
            panic!("expected a response");
        };
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/");
    }

    #[tokio::test]
    async fn auth_post_near_or_past_share_expiry_mints_only_a_grace_session() {
        // Daemon/gateway clock skew (or a share in its last seconds) must not
        // brick logins: the session floors at a short grace window. The share
        // itself dies with its registration, so the cookie outliving
        // `expires_at` by minutes grants nothing once the slug is gone.
        let state = test_state(false);
        let mut target = slug_auth(WebAccessMode::Password, Some("pw"));
        target.expires_at = chrono::Utc::now().timestamp() - 1;
        let Gate::Respond(resp) = gate(&state, &target, post_form("password=pw")).await else {
            panic!("expected a response");
        };
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        let max_age: i64 = cookie
            .split("Max-Age=")
            .nth(1)
            .and_then(|s| s.split(';').next())
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=300).contains(&max_age), "grace-capped, got {max_age}");
    }

    #[tokio::test]
    async fn login_page_is_immune_to_brace_template_injection_via_next() {
        // `next` may legally contain the literal token `{error}`. The page is
        // built by ordered string replacement, so unescaped braces in the
        // first substitution's value would be re-expanded by the second —
        // html_escape must neutralize them.
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("right"));
        let Gate::Respond(resp) = gate(
            &state,
            &target,
            post_form("password=wrong&next=%2F%7Berror%7D"),
        )
        .await
        else {
            panic!("expected a response");
        };
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = body_string(resp).await;
        // Exactly one error paragraph (the real one), and the value attribute
        // holds the escaped braces — no second expansion.
        assert_eq!(body.matches("class=\"err\"").count(), 1, "{body}");
        assert!(body.contains("value=\"/&#123;error&#125;\""), "{body}");
    }

    #[tokio::test]
    async fn auth_get_with_valid_session_redirects_instead_of_reprompting() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("pw"));
        let expiry = chrono::Utc::now().timestamp() + 600;
        let token = mint_token(&target.session_key, &target.slug, expiry);
        let req = Request::builder()
            .method("GET")
            .uri(AUTH_PATH)
            .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let Gate::Respond(resp) = gate(&state, &target, req).await else {
            panic!("expected a response");
        };
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/");
    }

    #[tokio::test]
    async fn auth_post_is_rate_limited_before_password_comparison() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("pw"));
        // Exhaust the anonymous (no ConnectInfo → slug-budget) window.
        for _ in 0..SLUG_LIMIT {
            let _ = gate(&state, &target, post_form("password=wrong")).await;
        }
        // Even the CORRECT password is now throttled — the limiter runs first.
        let Gate::Respond(resp) = gate(&state, &target, post_form("password=pw")).await else {
            panic!("expected a response");
        };
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(resp.headers().get(header::SET_COOKIE).is_none());
    }

    #[tokio::test]
    async fn auth_get_renders_the_form_and_other_methods_are_rejected() {
        let state = test_state(false);
        let target = slug_auth(WebAccessMode::Password, Some("pw"));
        let Gate::Respond(resp) = gate(&state, &target, get(AUTH_PATH)).await else {
            panic!("expected a response");
        };
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("<form"), "{body}");
        // The login page must carry the brand (rendered via pages::shell).
        assert!(body.contains("class=\"wordmark\""), "{body}");
        // It renders only while the share is registered, so it stamps the
        // tab-local marker the share 404 reads to show "Sharing has stopped".
        assert!(
            body.contains(&format!(
                "sessionStorage.setItem('{}'",
                crate::pages::SHARE_SEEN_KEY
            )),
            "{body}"
        );

        let del = Request::builder()
            .method("DELETE")
            .uri(AUTH_PATH)
            .body(axum::body::Body::empty())
            .unwrap();
        let Gate::Respond(resp) = gate(&state, &target, del).await else {
            panic!("expected a response");
        };
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn login_page_ships_loading_state_that_preserves_the_password() {
        let body = body_string(login_page(StatusCode::OK, "/", None)).await;
        // The busy affordance and its trigger ship on the page…
        assert!(body.contains("Unlocking"), "{body}");
        assert!(body.contains("class=\"spinner\""), "{body}");
        assert!(body.contains("classList.add('loading')"), "{body}");
        assert!(body.contains("addEventListener('submit'"), "{body}");
        // The one-link path calls form.submit() (which does NOT fire the submit
        // event), so it must set the loading state itself right before it —
        // this coupling is the only loading cue the auto-submit flow gets.
        assert!(body.contains("setLoading();\n  form.submit();"), "{body}");
        // The button's id must never be `submit`: a like-named child shadows
        // HTMLFormElement.submit, turning form.submit() into a TypeError and
        // freezing the one-link flow on a permanent spinner.
        assert!(!body.contains("id=\"submit\""), "{body}");
        // …and the field is made read-only, NOT disabled — a disabled field is
        // omitted from the POST body, which would silently drop the password.
        assert!(body.contains("readOnly = true"), "{body}");
        assert!(body.contains("btn.disabled = true"), "{body}");
        assert!(!body.contains("pw.disabled"), "{body}");
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
