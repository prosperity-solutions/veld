//! The HTTP front: proxy one public request onto the share's tunnel.
//!
//! Each request opens a fresh tunnel stream (QUIC streams are cheap; the host
//! splices each to a new TCP connection, so browser connection reuse maps to
//! stream-per-request here) and speaks HTTP/1.1 to the origin service.
//!
//! Fidelity policy (SHARING_V2.md §5.3): the upstream `Host` header is
//! rewritten to the **origin hostname** — dev servers (Vite & friends) enforce
//! host allow-lists and already accept their own `*.localhost` hostname, so
//! this makes the flagship case work zero-config. The public host travels in
//! `X-Forwarded-Host`. On the way back, `Location` redirects naming origin
//! hostnames are rewritten to public URLs, and `Set-Cookie` `Domain`
//! attributes scoped to origin hostnames are stripped (making the cookie
//! host-only, which works on the public host). Bodies are never rewritten.

use axum::body::Body;
use axum::extract::Request;
use axum::extract::connect_info::ConnectInfo;
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use tracing::{debug, warn};

use crate::registry::{RegisteredNode, Registration, SlugTarget};
use crate::state::AppState;
use crate::tunnel;

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
    let client_addr = parts
        .extensions
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string());
    let public_host = parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    let mut upstream_req = match build_upstream_request(
        &parts,
        &target.hostname,
        &public_host,
        client_addr.as_deref(),
        &state,
        is_upgrade,
    ) {
        Ok(r) => r,
        Err(resp) => return resp,
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

    let upstream_resp = match sender.send_request(upstream_req).await {
        Ok(r) => r,
        Err(e) => {
            debug!(error = %e, hostname = %target.hostname, "upstream request failed");
            return (
                StatusCode::BAD_GATEWAY,
                "the shared service did not respond",
            )
                .into_response();
        }
    };

    if upstream_resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        return splice_upgrade(upstream_resp, client_upgrade, &target.hostname);
    }

    let (mut resp_parts, resp_body) = upstream_resp.into_parts();
    rewrite_response_headers(&mut resp_parts.headers, reg, &state.config.domain);
    Response::from_parts(resp_parts, Body::new(resp_body))
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
    public_host: &str,
    client_ip: Option<&str>,
    state: &AppState,
    is_upgrade: bool,
) -> Result<axum::http::Request<Body>, Response> {
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
        if hop.contains(name) || name == header::HOST {
            continue;
        }
        headers.append(name.clone(), value.clone());
    }

    // Origin Host so dev-server host allow-lists pass (see module docs).
    headers.insert(
        header::HOST,
        HeaderValue::from_str(origin_hostname)
            .map_err(|_| (StatusCode::BAD_GATEWAY, "invalid origin hostname").into_response())?,
    );

    // Forwarding metadata. Existing values (an external LB's) are preserved;
    // we only fill gaps and append our hop to X-Forwarded-For.
    if !headers.contains_key("x-forwarded-host") {
        if let Ok(v) = HeaderValue::from_str(public_host) {
            headers.insert("x-forwarded-host", v);
        }
    }
    if !headers.contains_key("x-forwarded-proto") {
        let proto = if state.config.tls.is_some() {
            "https"
        } else {
            "http"
        };
        headers.insert("x-forwarded-proto", HeaderValue::from_static(proto));
    }
    if let Some(ip) = client_ip {
        let xff = match headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            Some(existing) => format!("{existing}, {ip}"),
            None => ip.to_owned(),
        };
        if let Ok(v) = HeaderValue::from_str(&xff) {
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
    }

    builder
        .body(Body::empty())
        .map_err(|_| (StatusCode::BAD_GATEWAY, "could not build upstream request").into_response())
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

/// Response-side fidelity rewrites: redirects back to public URLs, cookie
/// domains made host-only. (Bodies are never touched.)
fn rewrite_response_headers(headers: &mut HeaderMap, reg: &Registration, domain: &str) {
    for name in hop_by_hop() {
        headers.remove(name);
    }

    if let Some(location) = headers.get(header::LOCATION).and_then(|v| v.to_str().ok()) {
        if let Some(rewritten) = rewrite_absolute_url(location, &reg.nodes, domain) {
            if let Ok(v) = HeaderValue::from_str(&rewritten) {
                headers.insert(header::LOCATION, v);
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
            let value = match cookie.to_str() {
                Ok(s) => match strip_origin_cookie_domain(s, &reg.nodes) {
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

/// Remove a `Domain=<origin hostname>` attribute from one Set-Cookie value.
/// Returns `None` when nothing needs stripping.
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
            let domain_value = domain_value.trim().trim_start_matches('.');
            let matches_origin = nodes
                .iter()
                .any(|n| n.hostname.eq_ignore_ascii_case(domain_value));
            if matches_origin {
                changed = true;
            }
            !matches_origin
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
                slug: "abcdefabcdefabcdefabcdefab".into(),
                public_url: "https://abcdefabcdefabcdefabcdefab.share.example".into(),
            },
            RegisteredNode {
                node: "api".into(),
                hostname: "api.demo.p.localhost".into(),
                slug: "xyzxyzxyzxyzxyzxyzxyzxyzxy".into(),
                public_url: "https://xyzxyzxyzxyzxyzxyzxyzxyzxy.share.example".into(),
            },
        ]
    }

    fn rewrite(value: &str) -> Option<String> {
        rewrite_absolute_url(value, &nodes(), "share.example")
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
