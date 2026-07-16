//! The HTTP front: proxy one public request onto the share's tunnel.
//!
//! Each request opens a fresh tunnel stream (QUIC streams are cheap; the host
//! splices each to a new TCP connection, so browser connection reuse maps to
//! stream-per-request here) and speaks HTTP/1.1 to the origin service.
//!
//! Fidelity policy (SHARING_V2.md §5.3): the upstream `Host` header is
//! rewritten to the **origin hostname** — dev servers (Vite & friends) enforce
//! host allow-lists and already accept their own `*.localhost` hostname, so
//! this makes the flagship case work zero-config. `Origin` and `Referer` are
//! rewritten to the origin in lockstep with `Host`, so an Origin-checking dev
//! server sees a coherent same-origin request. The public host travels in
//! `X-Forwarded-Host` (`X-Forwarded-*`/`Forwarded` from the client are stripped
//! — the gateway is the public trust boundary). On the way back: `Location`
//! redirects and `Access-Control-Allow-Origin` values naming origin hostnames
//! are rewritten to public URLs, `Set-Cookie` `Domain` attributes scoped to an
//! origin hostname (or a parent of one) are stripped to host-only, and
//! `Referrer-Policy: no-referrer` is set so the slug doesn't leak. Bodies are
//! never rewritten.

use axum::body::Body;
use axum::extract::Request;
use axum::extract::connect_info::ConnectInfo;
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use tracing::{debug, warn};

use crate::registry::{RegisteredNode, SlugTarget};
use crate::state::AppState;
use crate::tunnel;

/// How long to wait for the upstream's response *head* before giving up with a
/// 504 (body streaming afterward is unbounded — SSE/downloads must work).
const UPSTREAM_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Hop-by-hop headers that must not cross the proxy (RFC 9110 §7.6.1).
/// `Upgrade`/`Connection` are re-added deliberately on the upgrade path.
fn hop_by_hop() -> [HeaderName; 8] {
    [
        header::CONNECTION,
        HeaderName::from_static("keep-alive"),
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ]
}

/// Proxy `req` (already routed by slug) onto the registration's tunnel.
pub async fn handle(state: AppState, target: SlugTarget, req: Request) -> Response {
    let reg = &target.registration;
    if reg.conn.close_reason().is_some() {
        // The watcher will unpublish this slug momentarily; answer honestly.
        return (StatusCode::BAD_GATEWAY, "share is no longer connected").into_response();
    }

    let is_upgrade = wants_upgrade(req.headers());
    let (mut parts, body) = req.into_parts();

    // hyper's server half parks the client connection behind this extension;
    // taking it is how we splice after a 101.
    let client_upgrade = parts.extensions.remove::<hyper::upgrade::OnUpgrade>();
    let socket_ip = parts
        .extensions
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string());
    // The X-Forwarded-For value the origin will see: the socket peer alone
    // (default — inbound chains from an anonymous-reachable edge are spoofable)
    // or, behind a trusted sanitising LB, the inbound chain with the peer
    // appended (standard proxy behavior).
    let client_addr = forwarded_for_value(
        &parts.headers,
        socket_ip.as_deref(),
        state.config.trust_forwarded_headers,
    );
    // The host the viewer addressed — behind a trusted CDN/LB that rewrites
    // `Host` to its origin, the viewer's host arrives in `X-Forwarded-Host`.
    // This is what goes upstream as `X-Forwarded-Host` and what Referer
    // rewriting matches against.
    let public_host =
        crate::server::viewer_host(&parts.headers, state.config.trust_forwarded_headers)
            .unwrap_or_default()
            .to_owned();

    let mut upstream_req = match build_upstream_request(
        &parts,
        &target.hostname,
        &target.origin,
        &public_host,
        client_addr.as_deref(),
        is_upgrade,
    ) {
        Ok(r) => r,
        Err(err) => return err.into_response(),
    };
    *upstream_req.body_mut() = if is_upgrade { Body::empty() } else { body };

    let mut sender = match tunnel::connect(&reg.conn, &target.hostname).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %format!("{e:#}"), hostname = %target.hostname, "tunnel stream failed");
            return (
                StatusCode::BAD_GATEWAY,
                "could not reach the shared service",
            )
                .into_response();
        }
    };

    // Bound the wait for response *headers* (not the body) so a dev server
    // that accepts the tunnel stream but never replies can't pin the stream +
    // driver task forever. Body streaming (SSE, large downloads) is unbounded
    // by design — only the initial response is deadlined.
    let upstream_resp =
        match tokio::time::timeout(UPSTREAM_RESPONSE_TIMEOUT, sender.send_request(upstream_req))
            .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                debug!(error = %e, hostname = %target.hostname, "upstream request failed");
                return (
                    StatusCode::BAD_GATEWAY,
                    "the shared service did not respond",
                )
                    .into_response();
            }
            Err(_) => {
                debug!(hostname = %target.hostname, "upstream response timed out");
                return (
                    StatusCode::GATEWAY_TIMEOUT,
                    "the shared service did not respond in time",
                )
                    .into_response();
            }
        };

    if upstream_resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        return splice_upgrade(upstream_resp, client_upgrade, &target.hostname);
    }

    let (mut resp_parts, resp_body) = upstream_resp.into_parts();
    rewrite_response_headers(
        &mut resp_parts.headers,
        &reg.nodes,
        &state.config.domain,
        target.access == veld_core::config::WebAccessMode::Password,
    );
    Response::from_parts(resp_parts, Body::new(resp_body))
}

/// Client-supplied forwarding headers the public edge must not trust — they
/// are stripped on the way in and set authoritatively by the gateway.
fn is_forwarding_header(name: &HeaderName) -> bool {
    let n = name.as_str();
    n.eq_ignore_ascii_case("x-forwarded-for")
        || n.eq_ignore_ascii_case("x-forwarded-host")
        || n.eq_ignore_ascii_case("x-forwarded-proto")
        || n.eq_ignore_ascii_case("forwarded")
}

/// The `X-Forwarded-For` value to send upstream. Untrusted edge (default):
/// the socket peer alone — an inbound chain is client-controlled and must be
/// discarded. Behind a trusted sanitising LB (`trust_forwarded_headers`): the
/// inbound chain with the socket peer appended, so the real client IP
/// survives the extra hop.
fn forwarded_for_value(
    headers: &HeaderMap,
    socket_ip: Option<&str>,
    trust_forwarded: bool,
) -> Option<String> {
    let inbound = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    match (trust_forwarded, inbound, socket_ip) {
        (true, Some(chain), Some(ip)) => Some(format!("{chain}, {ip}")),
        (true, Some(chain), None) => Some(chain.to_owned()),
        (_, _, ip) => ip.map(str::to_owned),
    }
}

/// True when the request asks to switch protocols (WebSockets, HMR).
fn wants_upgrade(headers: &HeaderMap) -> bool {
    headers.contains_key(header::UPGRADE)
        && headers
            .get(header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"))
}

/// Assemble the upstream request head: origin-form URI, filtered headers,
/// origin `Host`, forwarding metadata.
fn build_upstream_request(
    parts: &axum::http::request::Parts,
    origin_hostname: &str,
    origin: &str,
    public_host: &str,
    client_ip: Option<&str>,
    is_upgrade: bool,
) -> Result<axum::http::Request<Body>, (StatusCode, &'static str)> {
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let mut builder = axum::http::Request::builder()
        .method(parts.method.clone())
        .uri(path_and_query)
        .version(axum::http::Version::HTTP_11);

    let hop = hop_by_hop();
    let headers = builder.headers_mut().expect("fresh builder");
    for (name, value) in &parts.headers {
        if hop.contains(name)
            || name == header::HOST
            || name == header::ORIGIN
            || name == header::REFERER
            || is_forwarding_header(name)
        {
            // Host/Origin/Referer are rewritten below in lockstep, and the
            // forwarding headers are set authoritatively below — a viewer's
            // own X-Forwarded-* / Forwarded must never pass through (the
            // gateway is the public trust boundary; trusting them is
            // host-header injection + client-IP spoofing). Everything else
            // passes through.
            continue;
        }
        if name == header::COOKIE {
            // The gateway's viewer-session cookie is internal credential
            // material — the origin service never sees it. Strip on the raw
            // bytes: a Cookie header can mix an ASCII session pair with a
            // non-UTF-8 pair, and dropping to `str` first would fail wholesale
            // and leak the session token. `None` = our cookie wasn't present,
            // forward the value verbatim; otherwise forward only the survivors.
            match crate::auth::strip_session_cookie_bytes(value.as_bytes()) {
                None => {
                    headers.append(header::COOKIE, value.clone());
                }
                Some(kept) if !kept.is_empty() => {
                    if let Ok(v) = HeaderValue::from_bytes(&kept) {
                        headers.append(header::COOKIE, v);
                    }
                }
                Some(_) => {} // only the session cookie was present → send none
            }
            continue;
        }
        headers.append(name.clone(), value.clone());
    }

    // Rewrite Host, Origin, and Referer to the ORIGIN together (see module
    // docs). Rewriting Host alone while leaving Origin/Referer at the public
    // host would manufacture a cross-origin request that Origin-checking dev
    // servers (Next Server Actions, Vite's DNS-rebinding guard) reject — the
    // dev server must see a coherent same-origin request.
    headers.insert(
        header::HOST,
        HeaderValue::from_str(origin_hostname)
            .map_err(|_| (StatusCode::BAD_GATEWAY, "invalid origin hostname"))?,
    );
    if parts.headers.contains_key(header::ORIGIN) {
        if let Ok(v) = HeaderValue::from_str(origin) {
            headers.insert(header::ORIGIN, v);
        }
    }
    if let Some(referer) = parts.headers.get(header::REFERER) {
        // Swap the scheme://authority prefix (the public URL) for the origin's,
        // preserving the path so a framework that inspects Referer's path still
        // sees it. If it doesn't parse as our public URL, drop it rather than
        // forward a public-host Referer that contradicts the rewritten Host.
        if let Some(rewritten) = rewrite_referer(referer.to_str().ok(), public_host, origin) {
            if let Ok(v) = HeaderValue::from_str(&rewritten) {
                headers.insert(header::REFERER, v);
            }
        }
    }

    // Forwarding metadata, set authoritatively (inbound copies were stripped
    // above). The public scheme is always https — the gateway either
    // terminates TLS itself or sits behind an external TLS terminator, and the
    // minted URLs are always https. X-Forwarded-For is reset to the immediate
    // peer only: trusting an inbound chain from an anonymous-reachable edge
    // would let any viewer spoof it. (An operator with a trusted upstream LB
    // that wants the real client chain is a future `trust_forwarded_headers`
    // opt-in — the safe default is to overwrite.)
    if let Ok(v) = HeaderValue::from_str(public_host) {
        headers.insert("x-forwarded-host", v);
    }
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    if let Some(ip) = client_ip {
        if let Ok(v) = HeaderValue::from_str(ip) {
            headers.insert("x-forwarded-for", v);
        }
    }

    if is_upgrade {
        // Re-add the upgrade pair we filtered as hop-by-hop: this hop *does*
        // negotiate the upgrade with the origin.
        if let Some(upgrade) = parts.headers.get(header::UPGRADE) {
            headers.insert(header::UPGRADE, upgrade.clone());
        }
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    } else {
        // One tunnel stream per request, and the host side is a dumb byte
        // splice that only ends when the upstream TCP closes. Force the dev
        // server to close after responding — otherwise a keep-alive upstream
        // never EOFs, the host splice never ends, and the QUIC stream leaks.
        headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    }

    builder
        .body(Body::empty())
        .map_err(|_| (StatusCode::BAD_GATEWAY, "could not build upstream request"))
}

/// Rewrite a `Referer` whose scheme://authority is this share's public URL to
/// the origin's scheme://authority, preserving path + query. Returns `None`
/// (drop the header) when the value is absent, unparseable, or names some
/// other host — never forward a public-host Referer alongside the rewritten
/// origin `Host`.
fn rewrite_referer(referer: Option<&str>, public_host: &str, origin: &str) -> Option<String> {
    let referer = referer?;
    let (scheme, after) = referer.split_once("://")?;
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (authority, rest) = after.split_at(end);
    // Match on host only (ignore any :port on the public authority).
    let ref_host = authority.split(':').next().unwrap_or(authority);
    if !ref_host.eq_ignore_ascii_case(public_host) {
        return None;
    }
    let _ = scheme;
    Some(format!("{origin}{rest}"))
}

/// Complete a protocol upgrade: answer 101 to the client and splice the two
/// upgraded byte streams (browser ⇄ gateway ⇄ tunnel ⇄ dev server).
fn splice_upgrade(
    upstream_resp: hyper::Response<hyper::body::Incoming>,
    client_upgrade: Option<hyper::upgrade::OnUpgrade>,
    hostname: &str,
) -> Response {
    let Some(client_upgrade) = client_upgrade else {
        return (
            StatusCode::BAD_GATEWAY,
            "client connection cannot be upgraded",
        )
            .into_response();
    };

    // Mirror the origin's 101 response head to the client (minus hop-by-hop
    // noise hyper re-adds itself).
    let mut client_resp = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    if let Some(headers) = client_resp.headers_mut() {
        for (name, value) in upstream_resp.headers() {
            if name == header::CONNECTION {
                continue;
            }
            headers.append(name.clone(), value.clone());
        }
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    }

    let hostname = hostname.to_owned();
    tokio::spawn(async move {
        let upstream = match hyper::upgrade::on(upstream_resp).await {
            Ok(u) => u,
            Err(e) => {
                debug!(error = %e, %hostname, "upstream upgrade failed");
                return;
            }
        };
        let client = match client_upgrade.await {
            Ok(u) => u,
            Err(e) => {
                debug!(error = %e, %hostname, "client upgrade failed");
                return;
            }
        };
        let mut upstream = hyper_util::rt::TokioIo::new(upstream);
        let mut client = hyper_util::rt::TokioIo::new(client);
        if let Err(e) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
            debug!(error = %e, %hostname, "upgraded stream ended with error");
        }
    });

    client_resp
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Response-side fidelity rewrites: `Location` + `Access-Control-Allow-Origin`
/// back to public URLs, cookie `Domain`s made host-only, and
/// `Referrer-Policy: no-referrer` set. (Bodies are never touched.)
fn rewrite_response_headers(
    headers: &mut HeaderMap,
    nodes: &[RegisteredNode],
    domain: &str,
    password_mode: bool,
) {
    for name in hop_by_hop() {
        headers.remove(name);
    }

    // Password-gated content must not be re-served by a URL-keyed shared
    // cache to viewers who never authenticated. When the app states its own
    // caching policy we respect it (operator responsibility); when it says
    // nothing, default closed.
    if password_mode && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }

    // The slug must not leak to third-party origins the app links to: on a
    // link-access node it is the only bearer credential, and even on a
    // password-mode node it names the target. Force
    // `Referrer-Policy: no-referrer` (overriding any weaker app value) —
    // cheap, and it closes the most common slug-leak channel. Defence in
    // depth alongside the §6.1 password gate.
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );

    if let Some(location) = headers.get(header::LOCATION).and_then(|v| v.to_str().ok()) {
        if let Some(rewritten) = rewrite_absolute_url(location, nodes, domain) {
            if let Ok(v) = HeaderValue::from_str(&rewritten) {
                headers.insert(header::LOCATION, v);
            }
        }
    }

    // Access-Control-Allow-Origin: per-node slugs make an app-slug → api-slug
    // call cross-origin, so an API that echoes an origin-host allow-list would
    // fail CORS on the public host. Rewrite a matching origin to its public
    // origin (no path — ACAO is an origin, not a URL). `*` and unrelated
    // values pass through untouched.
    if let Some(acao) = headers
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rewritten) = rewrite_origin_value(acao, nodes, domain) {
            if let Ok(v) = HeaderValue::from_str(&rewritten) {
                headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            }
        }
    }

    // Set-Cookie: a Domain attribute scoped to an origin hostname would make
    // the browser reject the cookie on the public host — strip it so the
    // cookie falls back to host-only (correct for the slug host).
    let cookies: Vec<HeaderValue> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .cloned()
        .collect();
    if !cookies.is_empty() {
        headers.remove(header::SET_COOKIE);
        for cookie in cookies {
            // The upstream app must never (re)set the gateway's own session
            // cookie — a hostile co-tenant's Set-Cookie could otherwise shadow
            // or clear other slugs' sessions. (Belt-and-braces: the __Host-
            // prefix already makes browsers reject Domain-scoped variants.)
            if cookie.to_str().is_ok_and(|s| {
                s.trim_start()
                    .strip_prefix(crate::auth::SESSION_COOKIE)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            }) {
                continue;
            }
            let value = match cookie.to_str() {
                Ok(s) => match strip_origin_cookie_domain(s, nodes) {
                    Some(stripped) => HeaderValue::from_str(&stripped).unwrap_or(cookie),
                    None => cookie,
                },
                Err(_) => cookie,
            };
            headers.append(header::SET_COOKIE, value);
        }
    }
}

/// Rewrite an absolute URL pointing at one of the share's origin hostnames to
/// its public URL (path + query preserved). Returns `None` when the value is
/// relative or names a host outside the share.
fn rewrite_absolute_url(value: &str, nodes: &[RegisteredNode], domain: &str) -> Option<String> {
    let uri: Uri = value.parse().ok()?;
    let host = uri.authority()?.host().to_ascii_lowercase();
    let node = nodes
        .iter()
        .find(|n| n.hostname.eq_ignore_ascii_case(&host))?;
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    Some(format!("https://{}.{domain}{path_and_query}", node.slug))
}

/// Rewrite an `Origin`-shaped header value (`scheme://authority`, no path) that
/// names one of the share's origin hostnames to its public origin. Returns
/// `None` for `*`, relative, or foreign values (leave them untouched).
fn rewrite_origin_value(value: &str, nodes: &[RegisteredNode], domain: &str) -> Option<String> {
    let v = value.trim();
    if v == "*" || v.eq_ignore_ascii_case("null") {
        return None;
    }
    let uri: Uri = v.parse().ok()?;
    let host = uri.authority()?.host().to_ascii_lowercase();
    let node = nodes
        .iter()
        .find(|n| n.hostname.eq_ignore_ascii_case(&host))?;
    Some(format!("https://{}.{domain}", node.slug))
}

/// Remove a `Domain` attribute from one Set-Cookie value when it is scoped to
/// an origin hostname **or a parent of one** — either way the browser on the
/// public `<slug>.<domain>` host would reject the cookie. Stripping it makes
/// the cookie host-only, which works on the slug host. Returns `None` when
/// nothing needs stripping. (Cross-service cookies scoped to a shared parent
/// can't survive per-node slugs at all — a documented `web` fidelity limit.)
fn strip_origin_cookie_domain(cookie: &str, nodes: &[RegisteredNode]) -> Option<String> {
    let mut changed = false;
    let parts: Vec<&str> = cookie
        .split(';')
        .filter(|part| {
            let trimmed = part.trim();
            let Some(domain_value) = trimmed
                .strip_prefix("Domain=")
                .or_else(|| trimmed.strip_prefix("domain="))
            else {
                return true;
            };
            let d = domain_value
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase();
            // Strip if `d` is an origin hostname, or a parent domain of one
            // (an origin hostname ends with ".d").
            let dot_suffix = format!(".{d}");
            let scoped_to_origin = nodes.iter().any(|n| {
                let h = n.hostname.to_ascii_lowercase();
                h == d || h.ends_with(&dot_suffix)
            });
            if scoped_to_origin {
                changed = true;
            }
            !scoped_to_origin
        })
        .collect();
    changed.then(|| parts.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RegisteredNode;

    fn nodes() -> Vec<RegisteredNode> {
        vec![
            RegisteredNode {
                node: "app".into(),
                hostname: "app.demo.p.localhost".into(),
                origin: "https://app.demo.p.localhost".into(),
                slug: "abcdefabcdefabcdefabcdefab".into(),
                public_url: "https://abcdefabcdefabcdefabcdefab.share.example".into(),
                access: veld_core::config::WebAccessMode::Password,
            },
            RegisteredNode {
                node: "api".into(),
                hostname: "api.demo.p.localhost".into(),
                origin: "https://api.demo.p.localhost:18443".into(),
                slug: "xyzxyzxyzxyzxyzxyzxyzxyzxy".into(),
                public_url: "https://xyzxyzxyzxyzxyzxyzxyzxyzxy.share.example".into(),
                access: veld_core::config::WebAccessMode::Link,
            },
        ]
    }

    fn rewrite(value: &str) -> Option<String> {
        rewrite_absolute_url(value, &nodes(), "share.example")
    }

    #[test]
    fn password_mode_defaults_no_store_only_when_app_is_silent() {
        // Password node + no app cache policy → default closed.
        let mut h = HeaderMap::new();
        rewrite_response_headers(&mut h, &nodes(), "share.example", true);
        assert_eq!(h.get(header::CACHE_CONTROL).unwrap(), "no-store");

        // Password node + app states its own policy → respected, not overridden.
        let mut h = HeaderMap::new();
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
        rewrite_response_headers(&mut h, &nodes(), "share.example", true);
        assert_eq!(h.get(header::CACHE_CONTROL).unwrap(), "public, max-age=60");

        // Link node → the gateway never injects no-store.
        let mut h = HeaderMap::new();
        rewrite_response_headers(&mut h, &nodes(), "share.example", false);
        assert!(h.get(header::CACHE_CONTROL).is_none());
    }

    #[test]
    fn upstream_cannot_set_the_gateway_session_cookie() {
        // A hostile upstream trying to (re)set our __Host- session cookie is
        // dropped; its other Set-Cookies pass through.
        let mut h = HeaderMap::new();
        h.append(
            header::SET_COOKIE,
            HeaderValue::from_static("__Host-veld_gw_sess=forged; Path=/"),
        );
        h.append(
            header::SET_COOKIE,
            HeaderValue::from_static("app_sid=legit; Path=/"),
        );
        rewrite_response_headers(&mut h, &nodes(), "share.example", true);
        let cookies: Vec<&str> = h
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(cookies, vec!["app_sid=legit; Path=/"]);
    }

    #[test]
    fn location_rewrite_maps_origin_hosts_to_public_urls() {
        assert_eq!(
            rewrite("https://app.demo.p.localhost/login?next=%2Fx").unwrap(),
            "https://abcdefabcdefabcdefabcdefab.share.example/login?next=%2Fx"
        );
        // Ports on the origin URL are irrelevant — the hostname routes.
        assert_eq!(
            rewrite("https://api.demo.p.localhost:18443/v1").unwrap(),
            "https://xyzxyzxyzxyzxyzxyzxyzxyzxy.share.example/v1"
        );
        // Cross-service redirect (app → api) lands on the api slug.
        assert!(rewrite("http://api.demo.p.localhost/auth").is_some());
    }

    #[test]
    fn location_rewrite_leaves_foreign_and_relative_urls_alone() {
        assert_eq!(rewrite("/relative/path"), None);
        assert_eq!(rewrite("https://example.com/external"), None);
        assert_eq!(rewrite("https://other.demo.p.localhost/"), None);
    }

    #[test]
    fn cookie_domain_stripping_targets_origin_hosts_only() {
        let n = nodes();
        // Origin-scoped Domain is stripped (cookie becomes host-only).
        assert_eq!(
            strip_origin_cookie_domain(
                "sid=abc; Path=/; Domain=app.demo.p.localhost; HttpOnly",
                &n
            )
            .unwrap(),
            "sid=abc; Path=/; HttpOnly"
        );
        // Leading-dot form too.
        assert!(strip_origin_cookie_domain("sid=abc; domain=.api.demo.p.localhost", &n).is_some());
        // Foreign domains and domain-less cookies are untouched.
        assert!(strip_origin_cookie_domain("sid=abc; Domain=example.com", &n).is_none());
        assert!(strip_origin_cookie_domain("sid=abc; Path=/", &n).is_none());
        // A PARENT domain of an origin hostname is stripped too — otherwise a
        // shared-session cookie (Domain=.demo.p.localhost) is rejected on the
        // slug host and the session silently drops.
        assert!(strip_origin_cookie_domain("sid=abc; Domain=.demo.p.localhost", &n).is_some());
        assert!(strip_origin_cookie_domain("sid=abc; Domain=localhost", &n).is_some());
    }

    #[test]
    fn acao_rewrite_maps_origin_to_public_origin_only() {
        let n = nodes();
        // An origin-host ACAO becomes the public origin (no trailing path).
        assert_eq!(
            rewrite_origin_value("https://app.demo.p.localhost", &n, "share.example"),
            Some("https://abcdefabcdefabcdefabcdefab.share.example".to_string())
        );
        // Wildcard, null, foreign, and relative values are left untouched.
        assert_eq!(rewrite_origin_value("*", &n, "share.example"), None);
        assert_eq!(rewrite_origin_value("null", &n, "share.example"), None);
        assert_eq!(
            rewrite_origin_value("https://evil.example", &n, "share.example"),
            None
        );
    }

    #[test]
    fn referer_rewrite_swaps_public_host_for_origin_keeping_path() {
        // A Referer pointing at the public URL is rewritten to the origin,
        // path + query preserved, so the dev server sees a same-origin ref.
        assert_eq!(
            rewrite_referer(
                Some("https://abc123.share.example/login?next=%2Fx"),
                "abc123.share.example",
                "https://app.demo.p.localhost",
            ),
            Some("https://app.demo.p.localhost/login?next=%2Fx".to_string())
        );
        // Root referer → origin + "/".
        assert_eq!(
            rewrite_referer(
                Some("https://abc123.share.example/"),
                "abc123.share.example",
                "https://app.demo.p.localhost:18443",
            ),
            Some("https://app.demo.p.localhost:18443/".to_string())
        );
        // A Referer naming some OTHER host is dropped, never forwarded as-is.
        assert_eq!(
            rewrite_referer(
                Some("https://evil.example/x"),
                "abc123.share.example",
                "https://app.demo.p.localhost",
            ),
            None
        );
        // Absent / unparseable → dropped.
        assert_eq!(
            rewrite_referer(None, "abc123.share.example", "https://x"),
            None
        );
        assert_eq!(
            rewrite_referer(Some("not-a-url"), "abc123.share.example", "https://x"),
            None
        );
    }

    #[test]
    fn client_forwarding_headers_are_overwritten_not_trusted() {
        // A viewer spoofing X-Forwarded-* must not have them reach the origin.
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .header("host", "abc.share.example")
            .header("x-forwarded-host", "evil.example")
            .header("x-forwarded-proto", "http")
            .header("x-forwarded-for", "1.2.3.4")
            .header("forwarded", "for=6.6.6.6;host=evil.example")
            .body(Body::empty())
            .unwrap();
        let (parts, _) = req.into_parts();
        let out = build_upstream_request(
            &parts,
            "app.demo.p.localhost",
            "https://app.demo.p.localhost",
            "abc.share.example",
            Some("9.9.9.9"),
            false,
        )
        .unwrap();
        let h = out.headers();
        assert_eq!(h.get("x-forwarded-host").unwrap(), "abc.share.example");
        assert_eq!(h.get("x-forwarded-proto").unwrap(), "https");
        // Reset to the immediate peer — the spoofed chain is gone.
        assert_eq!(h.get("x-forwarded-for").unwrap(), "9.9.9.9");
        // The raw `Forwarded` header is stripped entirely (never set by us).
        assert!(h.get("forwarded").is_none());
        // Host rewritten to the origin; Connection: close forces upstream EOF.
        assert_eq!(h.get(header::HOST).unwrap(), "app.demo.p.localhost");
        assert_eq!(h.get(header::CONNECTION).unwrap(), "close");
    }

    #[test]
    fn forwarded_for_untrusted_vs_trusted() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("1.2.3.4, 5.6.7.8"),
        );
        // Default: the inbound chain is discarded — socket peer only.
        assert_eq!(
            forwarded_for_value(&h, Some("9.9.9.9"), false).as_deref(),
            Some("9.9.9.9")
        );
        // Trusted LB: chain preserved, socket peer appended.
        assert_eq!(
            forwarded_for_value(&h, Some("9.9.9.9"), true).as_deref(),
            Some("1.2.3.4, 5.6.7.8, 9.9.9.9")
        );
        // Trusted but no inbound chain → socket peer alone.
        let empty = HeaderMap::new();
        assert_eq!(
            forwarded_for_value(&empty, Some("9.9.9.9"), true).as_deref(),
            Some("9.9.9.9")
        );
    }

    #[test]
    fn session_cookie_is_stripped_from_upstream_request() {
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .header("host", "abc.share.example")
            .header(
                "cookie",
                "sid=app; __Host-veld_gw_sess=secret-token; theme=dark",
            )
            .body(Body::empty())
            .unwrap();
        let (parts, _) = req.into_parts();
        let out = build_upstream_request(
            &parts,
            "app.demo.p.localhost",
            "https://app.demo.p.localhost",
            "abc.share.example",
            None,
            false,
        )
        .unwrap();
        let cookie = out.headers().get(header::COOKIE).unwrap().to_str().unwrap();
        assert_eq!(cookie, "sid=app; theme=dark");

        // A request whose ONLY cookie is the session cookie sends none upstream.
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .header("host", "abc.share.example")
            .header("cookie", "__Host-veld_gw_sess=secret-token")
            .body(Body::empty())
            .unwrap();
        let (parts, _) = req.into_parts();
        let out = build_upstream_request(
            &parts,
            "app.demo.p.localhost",
            "https://app.demo.p.localhost",
            "abc.share.example",
            None,
            false,
        )
        .unwrap();
        assert!(out.headers().get(header::COOKIE).is_none());
    }

    #[test]
    fn upgrade_detection_requires_both_headers() {
        let mut h = HeaderMap::new();
        assert!(!wants_upgrade(&h));
        h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        assert!(!wants_upgrade(&h), "Upgrade without Connection: upgrade");
        h.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, Upgrade"),
        );
        assert!(wants_upgrade(&h));
    }
}
