//! Preview requests resolve to an active application port, never an opaque Caddy tunnel.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use axum::body::{Body, to_bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use veld_core::state::{NodeStatus, RunStatus};

use super::{MAX_REQUEST_BYTES, Principal, Remote, error, error_response, hop_header};

#[derive(Clone)]
struct Target {
    origin: String,
    port: u16,
}

impl Target {
    fn new(origin: &str, port: u16) -> Result<Self> {
        let url = reqwest::Url::parse(origin)?;
        if url.scheme() != "https"
            || !url
                .host_str()
                .is_some_and(|host| host.ends_with(".localhost"))
            || !url.username().is_empty()
            || url.password().is_some()
            || origin != url.origin().ascii_serialization()
            || port == 0
        {
            bail!("invalid registered preview origin");
        }
        Ok(Self {
            origin: origin.to_owned(),
            port,
        })
    }

    fn authority(&self) -> &str {
        self.origin
            .strip_prefix("https://")
            .expect("validated origin")
    }

    fn destination(&self, path: &str, websocket: bool) -> Result<String> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains(['\\', '#'])
            || path.chars().any(char::is_control)
            || path.len() > 16 * 1024
        {
            bail!("invalid preview request path");
        }
        let scheme = if websocket { "ws" } else { "http" };
        Ok(format!("{scheme}://127.0.0.1:{}{path}", self.port))
    }
}

async fn targets() -> Result<BTreeMap<String, Target>> {
    let mut targets = crate::offload::blocking(|| -> Result<BTreeMap<String, Target>> {
        let db = veld_core::db::Db::open()?;
        let registry = db.registry()?;
        let mut targets = BTreeMap::new();
        for project in registry.projects.values() {
            for run in project.runs.values() {
                if !matches!(run.status, RunStatus::Running | RunStatus::Starting) {
                    continue;
                }
                let Some(run) = db.get_run(&project.project_root, &run.name)? else {
                    continue;
                };
                for node in run.nodes.values() {
                    if !matches!(
                        node.status,
                        NodeStatus::Starting
                            | NodeStatus::HealthChecking
                            | NodeStatus::Healthy
                            | NodeStatus::Unhealthy
                    ) {
                        continue;
                    }
                    for endpoint in node.endpoints_or_legacy().values() {
                        let Some(url) = &endpoint.url else {
                            continue;
                        };
                        let origin = reqwest::Url::parse(url)?.origin().ascii_serialization();
                        if matches!(endpoint.port, 80 | 443 | 2019 | 19898 | 19899)
                            || endpoint.port == veld_core::instance::daemon_port()
                            || endpoint.port == veld_core::runner::DEFAULT_REMOTE_PORT
                        {
                            bail!("preview points at an infrastructure listener");
                        }
                        let target = Target::new(&origin, endpoint.port)?;
                        if targets.insert(origin, target).is_some() {
                            bail!("ambiguous preview origin");
                        }
                    }
                }
            }
        }
        Ok(targets)
    })
    .await?;
    if crate::http_server::files::is_ready() {
        let (https_port, _) = veld_core::helper::HelperClient::default_client()
            .web_ports()
            .await?
            .context("preview helper does not report its listener ports")?;
        let origin = reqwest::Url::parse(&format!(
            "https://{}:{https_port}",
            veld_core::instance::files_host()
        ))?
        .origin()
        .ascii_serialization();
        targets.insert(
            origin.clone(),
            Target::new(&origin, veld_core::instance::files_port())?,
        );
    }
    Ok(targets)
}

pub(super) async fn list(State(remote): State<Remote>, headers: HeaderMap) -> Response {
    if let Err(response) = remote.authorize(&headers) {
        return *response;
    }
    match targets().await {
        Ok(targets) => {
            axum::Json(serde_json::json!({"origins":targets.keys().collect::<Vec<_>>()}))
                .into_response()
        }
        Err(cause) => {
            tracing::warn!(error = %cause, "runner previews unavailable");
            error_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreviewQuery {
    origin: String,
    path: String,
}

pub(super) async fn proxy(
    State(remote): State<Remote>,
    Query(query): Query<PreviewQuery>,
    request: Request,
) -> Response {
    let principal = match remote.authorize(request.headers()) {
        Ok(p) => p,
        Err(response) => return *response,
    };
    let targets = match targets().await {
        Ok(targets) => targets,
        Err(_) => return error_response(),
    };
    let Some(target) = targets.get(&query.origin) else {
        return error(
            StatusCode::FORBIDDEN,
            "preview origin is not active on this runner",
        );
    };
    forward(remote, principal, target.clone(), query.path, request).await
}

fn application_headers(source: &HeaderMap, target: &Target) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in source {
        if !hop_header(name.as_str())
            && !name.as_str().starts_with("x-veld-")
            && !name.as_str().starts_with("x-forwarded-")
            && name != "forwarded"
            && !name.as_str().starts_with("sec-websocket-")
        {
            headers.append(name, value.clone());
        }
    }
    headers.insert(header::HOST, target.authority().parse()?);
    headers.insert("x-forwarded-host", target.authority().parse()?);
    headers.insert("x-forwarded-proto", "https".parse()?);
    for (source_name, destination) in [
        ("x-veld-preview-authorization", header::AUTHORIZATION),
        ("x-veld-preview-origin", header::ORIGIN),
    ] {
        if source.get_all(source_name).iter().count() > 1 {
            bail!("duplicate preview metadata");
        }
        if let Some(value) = source.get(source_name) {
            headers.insert(destination, value.clone());
        }
    }
    Ok(headers)
}

async fn forward(
    remote: Remote,
    principal: Principal,
    target: Target,
    path: String,
    request: Request,
) -> Response {
    let websocket = request.headers().contains_key(header::UPGRADE);
    let destination = match target.destination(&path, websocket) {
        Ok(destination) => destination,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid preview path"),
    };
    let headers = match application_headers(request.headers(), &target) {
        Ok(headers) => headers,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid preview headers"),
    };
    if websocket {
        return forward_socket(principal, request, destination, headers).await;
    }
    if request.method() == Method::CONNECT {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "nested tunnels are unavailable",
        );
    }
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "preview request is too large",
            );
        }
    };
    let response = tokio::select! {
        _ = principal.clone().revoked() => return error(StatusCode::UNAUTHORIZED, "client access revoked"),
        result = remote.http.request(parts.method, destination).headers(headers).body(bytes).send() => match result {
            Ok(response) => response,
            Err(_) => return error_response(),
        }
    };
    let status = response.status();
    let mut headers = response.headers().clone();
    for name in ["connection", "transfer-encoding", "content-length"] {
        headers.remove(name);
    }
    let mut output = Response::new(Body::from_stream(
        response.bytes_stream().take_until(principal.revoked()),
    ));
    *output.status_mut() = status;
    *output.headers_mut() = headers;
    output
}

async fn forward_socket(
    principal: Principal,
    request: Request,
    destination: String,
    headers: HeaderMap,
) -> Response {
    let (mut parts, _) = request.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid preview socket"),
    };
    let mut request = match destination.into_client_request() {
        Ok(request) => request,
        Err(_) => return error_response(),
    };
    request.headers_mut().extend(headers);
    if let Some(protocols) = parts.headers.get(header::SEC_WEBSOCKET_PROTOCOL) {
        request
            .headers_mut()
            .insert(header::SEC_WEBSOCKET_PROTOCOL, protocols.clone());
    }
    let (socket, response) = tokio::select! {
        _ = principal.clone().revoked() => return error(StatusCode::UNAUTHORIZED, "client access revoked"),
        result = tokio_tungstenite::connect_async(request) => match result {
            Ok(result) => result,
            Err(_) => return error_response(),
        }
    };
    let protocols: Vec<String> = response
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .map(|value| vec![value.to_owned()])
        .unwrap_or_default();
    let cookies: Vec<_> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .cloned()
        .collect();
    let mut response = upgrade.protocols(protocols).max_message_size(MAX_REQUEST_BYTES).on_upgrade(move |client| async move {
        tokio::select! {
            _ = principal.revoked() => {},
            result = super::bridge(client, socket) => if let Err(cause) = result { tracing::warn!(error = %cause, "preview socket closed"); },
        }
    });
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::super::Control;
    use super::*;
    use axum::routing::any;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn destination_cannot_be_retargeted_by_an_absolute_path_or_authority() {
        let target = Target::new("https://app.trial.localhost", 19001).unwrap();
        assert_eq!(
            target.destination("/page?q=1", false).unwrap(),
            "http://127.0.0.1:19001/page?q=1"
        );
        for path in [
            "https://veld.localhost/api/health",
            "//veld.localhost/api/health",
            "/\\veld.localhost",
            "/x#fragment",
            "/\r\nHost: veld.localhost",
        ] {
            assert!(target.destination(path, false).is_err());
        }
        assert!(Target::new("https://app.trial.localhost/other", 19001).is_err());
    }

    #[tokio::test]
    async fn preview_forces_application_authority_and_never_forwards_runner_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route(
            "/api/health",
            any(|headers: HeaderMap| async move {
                let values: BTreeMap<String, String> = headers
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
                    .collect();
                axum::Json(values)
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let store = veld_core::remote::RunnerStore::open(
            &directory.path().join("identity"),
            "https://runner.example.ts.net:19896",
        )
        .unwrap();
        let remote = Remote {
            control: Arc::new(Mutex::new(Control {
                store,
                clients: HashMap::new(),
                tickets: HashMap::new(),
            })),
            origin: "https://runner.example.ts.net:19896".into(),
            authority: "runner.example.ts.net:19896".into(),
            upstream: "http://127.0.0.1:19895".into(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        };
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let request = Request::builder()
            .uri("/remote/preview")
            .header(header::HOST, "veld.localhost")
            .header(header::AUTHORIZATION, "Bearer runner-secret")
            .header("x-veld-preview-authorization", "Bearer application-secret")
            .header("x-veld-preview-origin", "https://app.trial.localhost")
            .header("x-veld-remote-client", "forged-client")
            .header("x-forwarded-host", "veld.localhost")
            .body(Body::empty())
            .unwrap();
        let response = forward(
            remote,
            Principal {
                id: "fixture".into(),
                revoked: receiver,
            },
            Target::new("https://app.trial.localhost", port).unwrap(),
            "/api/health".into(),
            request,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let headers: BTreeMap<String, String> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(headers["host"], "app.trial.localhost");
        assert_eq!(headers["authorization"], "Bearer application-secret");
        assert_eq!(headers["origin"], "https://app.trial.localhost");
        assert_eq!(headers["x-forwarded-host"], "app.trial.localhost");
        assert!(!headers.keys().any(|name| name.starts_with("x-veld-")));
        assert!(
            !headers
                .values()
                .any(|value| value.contains("runner-secret"))
        );
        drop(sender);
        task.abort();
    }
}
