//! The HTTP front: proxy one public request onto the share's tunnel.
//!
//! Each request opens a fresh tunnel stream (QUIC streams are cheap; the host
//! splices each to a new TCP connection, so browser connection reuse maps to
//! stream-per-request here) and speaks HTTP/1.1 to the origin service.
//!
//! Fidelity policy: the upstream `Host` header is
//! rewritten to the **origin hostname** — dev servers (Vite & friends) enforce
//! host allow-lists and already accept their own `*.localhost` hostname, so
//! this makes the flagship case work zero-config. `Origin` and `Referer` are
//! rewritten to the origin in lockstep with `Host` — uniformly, including on
//! upgrade requests — so an Origin-checking dev server sees a coherent
//! same-origin request. (Next's HMR WebSocket allow-lists origins against
//! `localhost`/`allowedDevOrigins`, so its own hostname must be listed there;
//! set `allowedDevOrigins` in the framework config, or drop `Origin` entirely
//! with the per-service `proxy.request.remove: ["Origin"]` opt-in. veld does no
//! header stripping by default — see [`veld_core::config::ProxyConfig`].) The
//! public host travels in
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
        return crate::pages::share_error(
            StatusCode::BAD_GATEWAY,
            "Share disconnected",
            "This share is no longer connected. The developer may have stopped \
             sharing &mdash; ask them for a fresh link.",
        );
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
    let public_host = crate::server::viewer_host(&parts.headers, state.config.trust_forwarded_host)
        .unwrap_or_default()
        .to_owned();

    let mut upstream_req = match build_upstream_request(
        &parts,
        &target.hostname,
        &target.origin,
        &public_host,
        client_addr.as_deref(),
        is_upgrade,
        target.proxy.as_ref(),
    ) {
        Ok(r) => r,
        Err(err) => return err.into_response(),
    };
    *upstream_req.body_mut() = if is_upgrade { Body::empty() } else { body };

    let mut sender = match tunnel::connect(&reg.conn, &target.hostname).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %format!("{e:#}"), hostname = %target.hostname, "tunnel stream failed");
            return crate::pages::share_error(
                StatusCode::BAD_GATEWAY,
                "Could not reach the shared service",
                "The tunnel to the shared service failed. Try again in a moment.",
            );
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
                // On the upgrade path this is the signature of a dev server
                // destroying the handshake socket without a response — e.g.
                // Next's dev-origin gate rejecting the coherently-rewritten
                // Origin when the origin host isn't in `allowedDevOrigins` (and
                // the service didn't opt into `proxy.request.remove: ["Origin"]`).
                // Warn with the classification so a recurrence is diagnosable
                // from the gateway logs alone.
                if is_upgrade {
                    warn!(error = %e, hostname = %target.hostname,
                        "upstream closed the upgrade handshake without responding");
                } else {
                    debug!(error = %e, hostname = %target.hostname, "upstream request failed");
                }
                return crate::pages::share_error(
                    StatusCode::BAD_GATEWAY,
                    "The shared service did not respond",
                    "The service behind this share closed the connection without \
                     answering. Try again in a moment.",
                );
            }
            Err(_) => {
                debug!(hostname = %target.hostname, is_upgrade, "upstream response timed out");
                return crate::pages::share_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    "The shared service timed out",
                    "The service behind this share did not respond in time. \
                     Try again in a moment.",
                );
            }
        };

    if upstream_resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        return splice_upgrade(upstream_resp, client_upgrade, &target.hostname);
    }
    if is_upgrade {
        // The client asked to upgrade but the origin answered with a normal
        // response (e.g. a 4xx from an origin-gating dev server): pass it
        // through, but leave a trace — this is the first thing to look for
        // when someone reports "HMR doesn't work through the share".
        debug!(status = %upstream_resp.status(), hostname = %target.hostname,
            "upgrade request answered without 101; relaying as plain response");
    }

    let (mut resp_parts, resp_body) = upstream_resp.into_parts();
    rewrite_response_headers(
        &mut resp_parts.headers,
        &reg.nodes,
        &state.config.domain,
        target.access == veld_core::config::WebAccessMode::Password,
        target.proxy.as_ref(),
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
///
/// This detects HTTP/1.1 upgrade semantics only. An HTTP/2 WebSocket (RFC
/// 8441 extended CONNECT: `:method=CONNECT` + `:protocol=websocket`, no
/// Upgrade/Connection headers) is intentionally unsupported — the server
/// never enables h2 connect-protocol, so browsers fall back to a separate
/// HTTP/1.1 socket for WS. If connect-protocol is ever enabled, this
/// detection (and the splice path) must learn the h2 shape first.
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
    proxy: Option<&veld_core::config::ResolvedProxy>,
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
    // dev server must see a coherent same-origin request. This is intrinsic
    // correctness, NOT "stripping": the browser's public-host Origin is
    // translated to the origin's own host, applied uniformly to every request
    // including WebSocket upgrades.
    //
    // For frameworks that gate WS HMR on an origin allow-list (Next's
    // webpack/turbopack dev server checks `localhost` + `allowedDevOrigins`),
    // the coherent rewrite means the origin's own serving host must be
    // allow-listed — set `allowedDevOrigins` in the framework config (the
    // recommended fix). If that isn't possible, drop Origin entirely with the
    // per-service `proxy.request.remove: ["Origin"]` opt-in, which is applied
    // below after this rewrite. veld no longer drops Origin by default (it did
    // on upgrades before this change). NOTE the intrinsic default still differs
    // from the local Caddy proxy by necessity: the gateway's public host is not
    // the origin host, so it MUST rewrite Origin/Host/Referer coherently; the
    // local Caddy request is already same-origin, so it passes them through
    // untouched. Only the *user-config* header layer (`proxy.*`) is applied
    // identically on both.
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
    // minted URLs are always https. X-Forwarded-For defaults to the immediate
    // peer only — trusting an inbound chain from an anonymous-reachable edge
    // would let any viewer spoof it; behind a sanitising LB the
    // `trust_forwarded_headers` opt-in forwards the chain instead (see
    // `forwarded_for_value`).
    if let Ok(v) = HeaderValue::from_str(public_host) {
        headers.insert("x-forwarded-host", v);
    }
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    if let Some(ip) = client_ip {
        if let Ok(v) = HeaderValue::from_str(ip) {
            headers.insert("x-forwarded-for", v);
        }
    }

    // User-configured header rules, applied last so they win over the rewrites
    // above (this is where `proxy.request.remove: ["Origin"]` takes effect) —
    // but BEFORE the transport-critical Connection/Upgrade fixups below, which
    // must stay authoritative for the one-request-per-stream tunnel.
    if let Some(proxy) = proxy {
        apply_header_rules(headers, &proxy.request);
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
    // noise hyper re-adds itself). This path deliberately BYPASSES
    // `rewrite_response_headers` — Location/ACAO/Referrer-Policy/cache
    // rewrites are meaningless on a 101 — so any guard that must hold on
    // every response needs mirroring here. Today that is exactly one rule:
    // the upstream must never (re)set the gateway's own session cookie
    // (same reasoning as in `rewrite_response_headers`). NOTE: user
    // `proxy.response` rules are intentionally NOT applied here — a 101 carries
    // only handshake-critical headers (Upgrade/Connection/Sec-WebSocket-*), and
    // letting config strip/overwrite them would break the WebSocket. Documented
    // as an exception in docs/configuration.md.
    let mut client_resp = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    if let Some(headers) = client_resp.headers_mut() {
        for (name, value) in upstream_resp.headers() {
            if name == header::CONNECTION {
                continue;
            }
            if name == header::SET_COOKIE
                && value.to_str().is_ok_and(|s| {
                    s.trim_start()
                        .strip_prefix(crate::auth::SESSION_COOKIE)
                        .is_some_and(|rest| rest.trim_start().starts_with('='))
                })
            {
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
    proxy: Option<&veld_core::config::ResolvedProxy>,
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
    // depth alongside the §6.1 password gate. NOTE: this is re-asserted AFTER
    // user `proxy.response` rules below — that later insert is the authoritative
    // one (config must not be able to weaken it). Keep both; if you dedupe, keep
    // the LATE one.
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

    // User-configured response header rules, applied last so they win over the
    // gateway's intrinsic response rewrites above.
    //
    // The session-cookie strip above is NOT re-asserted after this: a host's
    // `proxy.response.set` of the `__Host-`-prefixed session cookie is confined
    // by the browser to that host's own slug (host-only, no Domain), so it can't
    // shadow another slug's session; and the value wouldn't verify anyway (the
    // session MAC uses a gateway-held capability-derived key the host doesn't
    // know). Co-tenant safety here rests on the `__Host-` prefix, not the strip.
    // Referrer-Policy differs — it IS re-asserted below because there is no such
    // structural guard behind it.
    if let Some(proxy) = proxy {
        apply_header_rules(headers, &proxy.response);
    }

    // Re-assert the non-negotiable slug-leak guard AFTER user rules: unlike
    // Cache-Control (which the gateway only sets when the app is silent, so
    // config overriding it is symmetric with the app doing so), the gateway
    // forces Referrer-Policy: no-referrer unconditionally — an upstream app
    // cannot weaken it, so a `proxy.response` rule must not be able to either.
    // On a link-access node the slug is the sole bearer credential.
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

/// Apply a set of static header rules to a header map: remove the named headers,
/// then set the configured name→value pairs (replacing any existing value).
/// Header names/values that aren't valid HTTP tokens are skipped rather than
/// failing the whole request — a config typo must not take the proxy down.
fn apply_header_rules(headers: &mut HeaderMap, rules: &veld_core::config::HeaderRules) {
    for name in &rules.remove {
        if let Ok(hn) = HeaderName::from_bytes(name.as_bytes()) {
            headers.remove(&hn);
        }
    }
    for (name, value) in &rules.set {
        if let (Ok(hn), Ok(hv)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(hn, hv);
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
                proxy: None,
            },
            RegisteredNode {
                node: "api".into(),
                hostname: "api.demo.p.localhost".into(),
                origin: "https://api.demo.p.localhost:18443".into(),
                slug: "xyzxyzxyzxyzxyzxyzxyzxyzxy".into(),
                public_url: "https://xyzxyzxyzxyzxyzxyzxyzxyzxy.share.example".into(),
                access: veld_core::config::WebAccessMode::Link,
                proxy: None,
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
        rewrite_response_headers(&mut h, &nodes(), "share.example", true, None);
        assert_eq!(h.get(header::CACHE_CONTROL).unwrap(), "no-store");

        // Password node + app states its own policy → respected, not overridden.
        let mut h = HeaderMap::new();
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
        rewrite_response_headers(&mut h, &nodes(), "share.example", true, None);
        assert_eq!(h.get(header::CACHE_CONTROL).unwrap(), "public, max-age=60");

        // Link node → the gateway never injects no-store.
        let mut h = HeaderMap::new();
        rewrite_response_headers(&mut h, &nodes(), "share.example", false, None);
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
        rewrite_response_headers(&mut h, &nodes(), "share.example", true, None);
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
            None,
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
            None,
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
            None,
        )
        .unwrap();
        assert!(out.headers().get(header::COOKIE).is_none());
    }

    #[test]
    fn origin_is_rewritten_coherently_on_both_upgrade_and_plain_requests() {
        // Default (no proxy config): the gateway rewrites Origin to the origin
        // host in lockstep with Host, UNIFORMLY — upgrades included. No more
        // upgrade-only Origin drop; header stripping is now opt-in via config.
        for (method, uri, is_upgrade, upgrade_headers) in [
            ("GET", "/_next/webpack-hmr?id=abc", true, true),
            ("POST", "/action", false, false),
        ] {
            let mut builder = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "abc.share.example")
                .header("origin", "https://abc.share.example");
            if upgrade_headers {
                builder = builder
                    .header("connection", "keep-alive, Upgrade")
                    .header("upgrade", "websocket");
            }
            let req = builder.body(Body::empty()).unwrap();
            let (parts, _) = req.into_parts();
            let out = build_upstream_request(
                &parts,
                "app.demo.p.localhost",
                "https://app.demo.p.localhost",
                "abc.share.example",
                None,
                is_upgrade,
                None,
            )
            .unwrap();
            assert_eq!(
                out.headers().get(header::ORIGIN).unwrap(),
                "https://app.demo.p.localhost",
                "{method} {uri}: Origin rewritten to the origin host",
            );
            if is_upgrade {
                assert_eq!(out.headers().get(header::UPGRADE).unwrap(), "websocket");
                assert_eq!(out.headers().get(header::CONNECTION).unwrap(), "upgrade");
            }
        }
    }

    #[test]
    fn proxy_request_remove_drops_origin_after_the_rewrite() {
        // Opt-in `proxy.request.remove: ["Origin"]` restores the pre-change
        // drop-on-upgrade behavior — but now for any request, and only when the
        // sharer's config asks for it. Applied AFTER the coherent rewrite.
        let proxy = veld_core::config::ResolvedProxy {
            request: veld_core::config::HeaderRules {
                remove: vec!["Origin".into()],
                set: Default::default(),
            },
            response: Default::default(),
        };
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_next/webpack-hmr?id=abc")
            .header("host", "abc.share.example")
            .header("origin", "https://abc.share.example")
            .header("connection", "keep-alive, Upgrade")
            .header("upgrade", "websocket")
            .body(Body::empty())
            .unwrap();
        let (parts, _) = req.into_parts();
        let out = build_upstream_request(
            &parts,
            "app.demo.p.localhost",
            "https://app.demo.p.localhost",
            "abc.share.example",
            None,
            true,
            Some(&proxy),
        )
        .unwrap();
        assert!(out.headers().get(header::ORIGIN).is_none());
        // Transport headers stay authoritative even with config applied.
        assert_eq!(out.headers().get(header::UPGRADE).unwrap(), "websocket");
        assert_eq!(out.headers().get(header::CONNECTION).unwrap(), "upgrade");
    }

    #[test]
    fn proxy_request_set_overrides_a_header() {
        let mut set = std::collections::BTreeMap::new();
        set.insert("X-Custom".to_string(), "veld".to_string());
        let proxy = veld_core::config::ResolvedProxy {
            request: veld_core::config::HeaderRules {
                remove: Vec::new(),
                set,
            },
            response: Default::default(),
        };
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .header("host", "abc.share.example")
            .header("x-custom", "attacker")
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
            Some(&proxy),
        )
        .unwrap();
        assert_eq!(out.headers().get("x-custom").unwrap(), "veld");
    }

    #[test]
    fn proxy_response_rules_apply_after_intrinsic_rewrites() {
        let mut set = std::collections::BTreeMap::new();
        set.insert("X-Frame-Options".to_string(), "DENY".to_string());
        let proxy = veld_core::config::ResolvedProxy {
            request: Default::default(),
            response: veld_core::config::HeaderRules {
                remove: vec!["Server".into()],
                set,
            },
        };
        let mut h = HeaderMap::new();
        h.insert(header::SERVER, HeaderValue::from_static("nginx"));
        rewrite_response_headers(&mut h, &nodes(), "share.example", false, Some(&proxy));
        assert!(h.get(header::SERVER).is_none());
        assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    }

    #[test]
    fn config_cannot_weaken_forced_referrer_policy() {
        // A response.set of Referrer-Policy must NOT re-open the slug-leak
        // channel — the gateway re-asserts no-referrer after user rules.
        let mut set = std::collections::BTreeMap::new();
        set.insert("Referrer-Policy".to_string(), "unsafe-url".to_string());
        let proxy = veld_core::config::ResolvedProxy {
            request: Default::default(),
            response: veld_core::config::HeaderRules {
                remove: Vec::new(),
                set,
            },
        };
        let mut h = HeaderMap::new();
        rewrite_response_headers(&mut h, &nodes(), "share.example", false, Some(&proxy));
        assert_eq!(h.get(header::REFERRER_POLICY).unwrap(), "no-referrer");
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
